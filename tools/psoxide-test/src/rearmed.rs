//! The PCSX-ReARMed adapter: a small libretro host that replays a smoke plan headless.
//!
//! It loads the core dylib with `dlopen`, boots the disc on the core's HLE BIOS with no audio
//! device and no window, and after every `retro_run` (one video frame) reads guest RAM through
//! `retro_get_memory_data(RETRO_MEMORY_SYSTEM_RAM)` to evaluate the plan's conditions. Buttons go
//! through the joypad device, the left stick through the analog device. A libretro core keeps
//! global state, so the harness runs each replay in its own process (`psoxide-test adapter
//! rearmed`), which is also what lets two replays share the emulator budget.

use std::ffi::{c_char, c_uint, c_void, CStr, CString};
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::journey::{Action, Plan};

/// What an adapter run produced (ReARMed and Redux write the same shape).
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Replay {
    /// Checkpoints reached, with the video frame they were checked on.
    pub reached: Vec<(String, u64)>,
    /// The first step that failed and why; `None` when every step passed.
    pub failure: Option<String>,
    pub frames: u64,
    /// Distinct pictures presented (frames whose picture hash differs from the previous one).
    pub distinct: u32,
}

const RETRO_DEVICE_JOYPAD: c_uint = 1;
const RETRO_DEVICE_ANALOG: c_uint = 5;
const RETRO_MEMORY_SYSTEM_RAM: c_uint = 2;
const ENV_GET_CAN_DUPE: c_uint = 3;
const ENV_GET_SYSTEM_DIRECTORY: c_uint = 9;
const ENV_SET_PIXEL_FORMAT: c_uint = 10;
const ENV_GET_VARIABLE: c_uint = 15;
const ENV_GET_VARIABLE_UPDATE: c_uint = 17;
const ENV_GET_SAVE_DIRECTORY: c_uint = 31;
const ENV_SET_CONTROLLER_INFO: c_uint = 35;
const JOYPAD_MASK: c_uint = 256;

/// libretro joypad ids in the order of `journey::BUTTONS` names.
fn button_id(name: &str) -> Option<u32> {
    Some(match name {
        "cross" => 0,
        "square" => 1,
        "select" => 2,
        "start" => 3,
        "up" => 4,
        "down" => 5,
        "left" => 6,
        "right" => 7,
        "circle" => 8,
        "triangle" => 9,
        "l1" => 10,
        "r1" => 11,
        "l2" => 12,
        "r2" => 13,
        "l3" => 14,
        "r3" => 15,
        _ => return None,
    })
}

#[repr(C)]
struct GameInfo {
    path: *const c_char,
    data: *const c_void,
    size: usize,
    meta: *const c_char,
}

#[repr(C)]
struct Variable {
    key: *const c_char,
    value: *const c_char,
}

#[repr(C)]
struct ControllerDescription {
    desc: *const c_char,
    id: c_uint,
}

#[repr(C)]
struct ControllerInfo {
    types: *const ControllerDescription,
    num_types: c_uint,
}

/// Everything the callbacks touch.
struct Host {
    system_dir: CString,
    save_dir: CString,
    bios_value: CString,
    pixel_format: u32,
    /// Port-1 controller types the core offered: (description, id).
    port1_types: Vec<(String, u32)>,
    buttons: u16,
    lstick: [i16; 2],
    last_hash: u64,
    have_frame: bool,
    distinct: u32,
    /// The last picture as RGB8, for checkpoint screenshots.
    rgb: Vec<u8>,
    width: u32,
    height: u32,
}

static HOST: Mutex<Option<Host>> = Mutex::new(None);

fn with_host<R>(f: impl FnOnce(&mut Host) -> R) -> Option<R> {
    let mut guard = HOST.lock().ok()?;
    guard.as_mut().map(f)
}

extern "C" fn environment(cmd: c_uint, data: *mut c_void) -> bool {
    if data.is_null() {
        return false;
    }
    with_host(|host| match cmd {
        ENV_GET_CAN_DUPE => {
            // SAFETY: libretro passes a `bool *` for this command; checked non-null above.
            unsafe { *(data as *mut bool) = true };
            true
        }
        ENV_GET_SYSTEM_DIRECTORY | ENV_GET_SAVE_DIRECTORY => {
            let dir = if cmd == ENV_GET_SYSTEM_DIRECTORY {
                host.system_dir.as_ptr()
            } else {
                host.save_dir.as_ptr()
            };
            // SAFETY: libretro passes a `const char **` here; the CString lives in HOST for the
            // whole run, so the pointer stays valid.
            unsafe { *(data as *mut *const c_char) = dir };
            true
        }
        ENV_SET_PIXEL_FORMAT => {
            // SAFETY: libretro passes a `const enum retro_pixel_format *` (32-bit) here.
            let format = unsafe { *(data as *const u32) };
            host.pixel_format = format;
            format <= 2
        }
        ENV_GET_VARIABLE => {
            // SAFETY: libretro passes a `struct retro_variable *`; its key is a C string.
            let var = unsafe { &mut *(data as *mut Variable) };
            if var.key.is_null() {
                return false;
            }
            // SAFETY: non-null key, a NUL-terminated string owned by the core.
            let key = unsafe { CStr::from_ptr(var.key) };
            if key.to_bytes() == b"pcsx_rearmed_bios" {
                var.value = host.bios_value.as_ptr();
                true
            } else {
                var.value = std::ptr::null();
                false
            }
        }
        ENV_GET_VARIABLE_UPDATE => {
            // SAFETY: libretro passes a `bool *` for this command.
            unsafe { *(data as *mut bool) = false };
            true
        }
        ENV_SET_CONTROLLER_INFO => {
            // SAFETY: an array of retro_controller_info ending with an entry whose types is
            // null; the first entry is port 1. Its descriptions are C strings the core owns.
            let first = unsafe { &*(data as *const ControllerInfo) };
            host.port1_types.clear();
            if !first.types.is_null() {
                for i in 0..first.num_types as usize {
                    // SAFETY: i < num_types, inside the array the core gave.
                    let d = unsafe { &*first.types.add(i) };
                    if !d.desc.is_null() {
                        // SAFETY: non-null C string owned by the core.
                        let desc = unsafe { CStr::from_ptr(d.desc) }
                            .to_string_lossy()
                            .into_owned();
                        host.port1_types.push((desc, d.id));
                    }
                }
            }
            true
        }
        _ => false,
    })
    .unwrap_or(false)
}

extern "C" fn video(data: *const c_void, width: c_uint, height: c_uint, pitch: usize) {
    with_host(|host| {
        if data.is_null() || width == 0 || height == 0 {
            return;
        }
        let bpp = if host.pixel_format == 1 { 4 } else { 2 };
        let row = width as usize * bpp;
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        host.rgb.clear();
        for y in 0..height as usize {
            // SAFETY: the core hands `height` rows of `pitch` bytes, each at least `row` long.
            let line =
                unsafe { std::slice::from_raw_parts((data as *const u8).add(y * pitch), row) };
            for chunk in line.chunks_exact(bpp) {
                let px = if bpp == 4 {
                    u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
                } else {
                    u32::from(u16::from_le_bytes([chunk[0], chunk[1]]))
                };
                hash = (hash ^ u64::from(px)).wrapping_mul(0x0100_0000_01b3);
                let (r, g, b) = match host.pixel_format {
                    1 => ((px >> 16) as u8, (px >> 8) as u8, px as u8),
                    2 => (
                        ((px >> 11) & 31) as u8 * 8,
                        ((px >> 5) & 63) as u8 * 4,
                        (px & 31) as u8 * 8,
                    ),
                    _ => (
                        ((px >> 10) & 31) as u8 * 8,
                        ((px >> 5) & 31) as u8 * 8,
                        (px & 31) as u8 * 8,
                    ),
                };
                host.rgb.extend_from_slice(&[r, g, b]);
            }
        }
        host.width = width;
        host.height = height;
        if !host.have_frame || hash != host.last_hash {
            host.distinct += 1;
        }
        host.have_frame = true;
        host.last_hash = hash;
    });
}

extern "C" fn audio_sample(_left: i16, _right: i16) {}

extern "C" fn audio_batch(_data: *const i16, frames: usize) -> usize {
    frames
}

extern "C" fn input_poll() {}

extern "C" fn input_state(port: c_uint, device: c_uint, index: c_uint, id: c_uint) -> i16 {
    if port != 0 {
        return 0;
    }
    with_host(|host| match device & 0xff {
        RETRO_DEVICE_JOYPAD if id == JOYPAD_MASK => host.buttons as i16,
        RETRO_DEVICE_JOYPAD if id < 16 => i16::from(host.buttons & (1 << id) != 0),
        RETRO_DEVICE_ANALOG if index == 0 && id < 2 => host.lstick[id as usize],
        _ => 0,
    })
    .unwrap_or(0)
}

type SetFn = unsafe extern "C" fn(*const c_void);

/// The core's entry points.
struct Core {
    handle: *mut c_void,
}

impl Core {
    fn open(path: &Path) -> Result<Core, String> {
        let c = CString::new(path.display().to_string()).map_err(|e| e.to_string())?;
        // SAFETY: a valid C path; RTLD_NOW resolves everything up front so a missing symbol
        // fails here and not mid-run.
        let handle = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            return Err(format!("dlopen {} failed", path.display()));
        }
        Ok(Core { handle })
    }

    fn symbol(&self, name: &str) -> Result<*mut c_void, String> {
        let c = CString::new(name).map_err(|e| e.to_string())?;
        // SAFETY: handle came from dlopen and is open; name is a C string.
        let sym = unsafe { libc::dlsym(self.handle, c.as_ptr()) };
        if sym.is_null() {
            Err(format!("the core has no `{name}`: is it a libretro core?"))
        } else {
            Ok(sym)
        }
    }
}

macro_rules! call {
    ($core:expr, $name:literal, $ty:ty $(, $arg:expr)*) => {{
        let sym = $core.symbol($name)?;
        // SAFETY: the libretro API fixes this entry point's signature as `$ty`.
        let f: $ty = unsafe { std::mem::transmute::<*mut c_void, $ty>(sym) };
        // SAFETY: called in the order the libretro API documents, on one thread.
        unsafe { f($($arg),*) }
    }};
}

/// Replay `plan` on the core. `out` receives `result.json` and a PNG per checkpoint.
pub fn replay(plan: &Plan, core_path: &Path, out: &Path) -> Result<Replay, String> {
    std::fs::create_dir_all(out).map_err(|e| format!("create {}: {e}", out.display()))?;
    let sys = out.join("sys");
    let save = out.join("save");
    for d in [&sys, &save] {
        std::fs::create_dir_all(d).map_err(|e| format!("create {}: {e}", d.display()))?;
    }
    let cstr = |p: &Path| CString::new(p.display().to_string()).map_err(|e| e.to_string());
    {
        let mut guard = HOST.lock().map_err(|_| "host lock")?;
        *guard = Some(Host {
            system_dir: cstr(&sys)?,
            save_dir: cstr(&save)?,
            bios_value: CString::new("HLE").map_err(|e| e.to_string())?,
            pixel_format: 0,
            port1_types: Vec::new(),
            buttons: 0,
            lstick: [0, 0],
            last_hash: 0,
            have_frame: false,
            distinct: 0,
            rgb: Vec::new(),
            width: 0,
            height: 0,
        });
    }
    let core = Core::open(core_path)?;
    let env_cb: extern "C" fn(c_uint, *mut c_void) -> bool = environment;
    let video_cb: extern "C" fn(*const c_void, c_uint, c_uint, usize) = video;
    let sample_cb: extern "C" fn(i16, i16) = audio_sample;
    let batch_cb: extern "C" fn(*const i16, usize) -> usize = audio_batch;
    let poll_cb: extern "C" fn() = input_poll;
    let state_cb: extern "C" fn(c_uint, c_uint, c_uint, c_uint) -> i16 = input_state;
    call!(
        core,
        "retro_set_environment",
        SetFn,
        env_cb as *const c_void
    );
    call!(
        core,
        "retro_set_video_refresh",
        SetFn,
        video_cb as *const c_void
    );
    call!(
        core,
        "retro_set_audio_sample",
        SetFn,
        sample_cb as *const c_void
    );
    call!(
        core,
        "retro_set_audio_sample_batch",
        SetFn,
        batch_cb as *const c_void
    );
    call!(
        core,
        "retro_set_input_poll",
        SetFn,
        poll_cb as *const c_void
    );
    call!(
        core,
        "retro_set_input_state",
        SetFn,
        state_cb as *const c_void
    );
    call!(core, "retro_init", unsafe extern "C" fn());
    let disc = cstr(&plan.disc)?;
    let info = GameInfo {
        path: disc.as_ptr(),
        data: std::ptr::null(),
        size: 0,
        meta: std::ptr::null(),
    };
    let loaded = call!(
        core,
        "retro_load_game",
        unsafe extern "C" fn(*const GameInfo) -> bool,
        &info
    );
    if !loaded {
        return Err(format!("the core refused {}", plan.disc.display()));
    }
    let types = with_host(|h| h.port1_types.clone()).unwrap_or_default();
    let wanted = if plan.pad == "dualshock" {
        "dualshock"
    } else {
        "standard"
    };
    let device = types
        .iter()
        .find(|(d, _)| d.to_ascii_lowercase().contains(wanted))
        .map(|(_, id)| *id)
        .ok_or_else(|| {
            format!(
                "the core offers no `{wanted}` controller on port 1 (it offers: {})",
                types
                    .iter()
                    .map(|(d, _)| d.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
    call!(
        core,
        "retro_set_controller_port_device",
        unsafe extern "C" fn(c_uint, c_uint),
        0,
        device
    );
    call!(
        core,
        "retro_set_controller_port_device",
        unsafe extern "C" fn(c_uint, c_uint),
        1,
        0
    );
    let ram = call!(
        core,
        "retro_get_memory_data",
        unsafe extern "C" fn(c_uint) -> *mut c_void,
        RETRO_MEMORY_SYSTEM_RAM
    ) as *const u8;
    let ram_size = call!(
        core,
        "retro_get_memory_size",
        unsafe extern "C" fn(c_uint) -> usize,
        RETRO_MEMORY_SYSTEM_RAM
    );
    if ram.is_null() || ram_size < 0x20_0000 {
        return Err("the core exposes no 2 MB system RAM".into());
    }
    let read = |addr: u32| -> u32 {
        let at = (addr & 0x1f_ffff) as usize;
        let mut bytes = [0u8; 4];
        for (i, b) in bytes.iter_mut().enumerate() {
            if at + i < ram_size {
                // SAFETY: inside the core's system RAM block of `ram_size` bytes.
                *b = unsafe { *ram.add(at + i) };
            }
        }
        u32::from_le_bytes(bytes)
    };
    let run_frame = || -> Result<(), String> {
        call!(core, "retro_run", unsafe extern "C" fn());
        Ok(())
    };
    let mut result = Replay::default();
    'steps: for step in &plan.steps {
        let holds = |read: &dyn Fn(u32) -> u32| step.probes.iter().all(|p| p.holds(read(p.addr)));
        match &step.action {
            Action::Check { name } => {
                let failing: Vec<String> = step
                    .probes
                    .iter()
                    .filter(|p| !p.holds(read(p.addr)))
                    .map(|p| p.describe(read(p.addr)))
                    .collect();
                save_png(out, name);
                if failing.is_empty() {
                    result.reached.push((name.clone(), result.frames));
                } else {
                    result.failure = Some(format!(
                        "checkpoint {name} at frame {}: {} does not hold",
                        result.frames,
                        failing.join(", ")
                    ));
                    break 'steps;
                }
            }
            action => {
                let (mask, stick) = match action {
                    Action::Hold { buttons, lstick } => (
                        buttons
                            .iter()
                            .filter_map(|b| button_id(b))
                            .fold(0u16, |m, id| m | (1 << id)),
                        lstick.map_or([0, 0], |[x, y]| {
                            [(i16::from(x) - 128) * 256, (i16::from(y) - 128) * 256]
                        }),
                    ),
                    _ => (0, [0, 0]),
                };
                let mut waited = 0;
                loop {
                    if holds(&read) {
                        break;
                    }
                    if waited >= step.timeout || result.frames >= plan.max_frames {
                        result.failure = Some(format!(
                            "{}: {} still false after {waited} frames (frame {})",
                            step.label,
                            step.probes
                                .iter()
                                .map(|p| p.describe(read(p.addr)))
                                .collect::<Vec<_>>()
                                .join(", "),
                            result.frames
                        ));
                        break 'steps;
                    }
                    with_host(|h| {
                        h.buttons = mask;
                        h.lstick = stick;
                    });
                    run_frame()?;
                    result.frames += 1;
                    waited += 1;
                }
                with_host(|h| {
                    h.buttons = 0;
                    h.lstick = [0, 0];
                });
            }
        }
    }
    if result.failure.is_none() {
        for _ in 0..plan.idle_frames {
            run_frame()?;
            result.frames += 1;
        }
    }
    result.distinct = with_host(|h| h.distinct).unwrap_or(0);
    let json = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
    std::fs::write(out.join("result.json"), json).map_err(|e| e.to_string())?;
    // The process exits right after; unloading a core that keeps threads is where hosts crash.
    Ok(result)
}

fn save_png(out: &Path, name: &str) {
    let Some((rgb, w, h)) = with_host(|h| (h.rgb.clone(), h.width, h.height)) else {
        return;
    };
    if let Some(img) = image::RgbImage::from_raw(w, h, rgb) {
        let _ = img.save(out.join(format!("{name}.png")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_journey_button_has_a_libretro_id() {
        for b in [
            "cross", "circle", "square", "triangle", "start", "select", "up", "down", "left",
            "right", "l1", "r1", "l2", "r2", "l3", "r3",
        ] {
            assert!(button_id(b).is_some(), "{b}");
        }
        assert_eq!(button_id("cross"), Some(0));
        assert_eq!(button_id("circle"), Some(8));
    }
}
