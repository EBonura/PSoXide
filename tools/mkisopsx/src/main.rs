//! `mkisopsx` -- wrap a PSX-EXE into a bootable PS1 disc image.
//!
//! Usage:
//!
//! ```text
//!   mkisopsx --exe path/to/hello-tri.exe --out hello.bin [--volume HELLO] [--iso]
//! ```
//!
//! Default output is a raw 2352-byte-per-sector `.bin` image -- the
//! format PSoXide's own CDROM loader expects and that most desktop
//! emulators (PCSX-Redux, Duckstation, Mednafen) also accept. Pass
//! `--iso` to emit a cooked 2048-byte-per-sector `.iso` instead
//! (smaller, accepted by a few tools that dislike raw sectors).
//!
//! Both flavours contain the same ISO 9660 filesystem: `SYSTEM.CNF`
//! (points the BIOS at `PSX.EXE`) and the EXE itself, both in the
//! root directory. Pass `--cdtest-sectors N` to insert deterministic
//! `CDTEST.BIN` stream-benchmark data before `PSX.EXE`. Pass one or
//! more `--cdda-track <raw-pcm>` paths to append sector-aligned raw
//! CD-DA tracks into the same `.bin` and emit a sibling `.cue`.
//!
//! The tool is deliberately a tiny CLI -- actual encoding lives in
//! `psx-iso::iso9660` so it's reusable from build scripts, test
//! harnesses, or a future GUI bundler.

use psx_iso::{
    build_world_pack, cd_stream_bench_payload, default_system_cnf, Exe, IsoBuilder,
    CD_STREAM_BENCH_FILE_NAME, WORLD_PACK_FILE_NAME,
};
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const CDDA_PREGAP_SECTORS: u32 = 150;

struct Args {
    exe: PathBuf,
    out: PathBuf,
    volume: String,
    cooked_iso: bool,
    cdtest_sectors: Option<usize>,
    world_pack_rooms_dir: Option<PathBuf>,
    world_pack_order_file: Option<PathBuf>,
    cdda_tracks: Vec<PathBuf>,
    system_area: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut exe: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut volume = String::from("PSOXIDE");
    let mut cooked_iso = false;
    let mut cdtest_sectors = None;
    let mut world_pack_rooms_dir = None;
    let mut world_pack_order_file = None;
    let mut cdda_tracks = Vec::new();
    let mut system_area = env::var_os("PSOXIDE_SYSTEM_AREA").map(PathBuf::from);
    let mut it = env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--exe" | "-e" => {
                exe = Some(PathBuf::from(
                    it.next().ok_or_else(|| "--exe takes a path".to_string())?,
                ));
            }
            "--out" | "-o" => {
                out = Some(PathBuf::from(
                    it.next().ok_or_else(|| "--out takes a path".to_string())?,
                ));
            }
            "--volume" | "-v" => {
                volume = it
                    .next()
                    .ok_or_else(|| "--volume takes a string".to_string())?;
            }
            "--iso" => {
                cooked_iso = true;
            }
            "--cdtest-sectors" => {
                let raw = it
                    .next()
                    .ok_or_else(|| "--cdtest-sectors takes a sector count".to_string())?;
                let sectors = raw
                    .parse::<usize>()
                    .map_err(|_| format!("invalid --cdtest-sectors value: {raw}"))?;
                if sectors == 0 {
                    return Err("--cdtest-sectors must be greater than zero".to_string());
                }
                cdtest_sectors = Some(sectors);
            }
            "--world-pack-rooms-dir" => {
                world_pack_rooms_dir =
                    Some(PathBuf::from(it.next().ok_or_else(|| {
                        "--world-pack-rooms-dir takes a path".to_string()
                    })?));
            }
            "--world-pack-order-file" => {
                world_pack_order_file =
                    Some(PathBuf::from(it.next().ok_or_else(|| {
                        "--world-pack-order-file takes a path".to_string()
                    })?));
            }
            "--cdda-track" => {
                cdda_tracks.push(PathBuf::from(
                    it.next()
                        .ok_or_else(|| "--cdda-track takes a path".to_string())?,
                ));
            }
            "--system-area" => {
                system_area = Some(PathBuf::from(
                    it.next()
                        .ok_or_else(|| "--system-area takes a path".to_string())?,
                ));
            }
            "--help" | "-h" => {
                return Err(String::from("help"));
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let exe = exe.ok_or_else(|| "missing --exe".to_string())?;
    let out = out.ok_or_else(|| "missing --out".to_string())?;
    Ok(Args {
        exe,
        out,
        volume,
        cooked_iso,
        cdtest_sectors,
        world_pack_rooms_dir,
        world_pack_order_file,
        cdda_tracks,
        system_area,
    })
}

fn print_usage() {
    eprintln!(
        "usage: mkisopsx --exe <file.exe> --out <file.bin> [--volume <id>] [--iso]\n\
         \n\
         Wraps a PSX-EXE into a bootable PS1 disc image containing\n\
         `SYSTEM.CNF` and `PSX.EXE` in the ISO 9660 root directory.\n\
         \n\
         --exe, -e       Path to the PSX-EXE (e.g. `hello-tri.exe`).\n\
         --out, -o       Destination path for the image.\n\
         --volume, -v    Volume identifier (default: PSOXIDE).\n\
         --cdtest-sectors N\n\
                         Add deterministic CDTEST.BIN benchmark data\n\
                         before PSX.EXE.\n\
         --world-pack-rooms-dir PATH\n\
                         Pack room_*.psxc stream chunks into WORLD.PAK before\n\
                         PSX.EXE.\n\
         --world-pack-order-file PATH\n\
                         Optional newline-delimited room id order for\n\
                         WORLD.PAK payload placement.\n\
         --cdda-track PATH\n\
                         Append a sector-aligned raw CD-DA track to\n\
                         the output .bin and emit a sibling .cue. May\n\
                         be repeated.\n\
         --system-area PATH\n\
                         Inject the first 16 sectors from a local PS1\n\
                         system-area file or disc image. May also be\n\
                         supplied by PSOXIDE_SYSTEM_AREA.\n\
         --iso           Emit a cooked 2048-byte-per-sector .iso\n\
                         instead of the default raw 2352-byte .bin.\n"
    );
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) if msg == "help" => {
            print_usage();
            return ExitCode::SUCCESS;
        }
        Err(msg) => {
            eprintln!("{msg}");
            print_usage();
            return ExitCode::from(2);
        }
    };
    if args.cooked_iso && !args.cdda_tracks.is_empty() {
        eprintln!("--cdda-track requires raw .bin output; .iso cannot carry audio tracks");
        return ExitCode::from(2);
    }

    let exe_bytes = match fs::read(&args.exe) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("read {}: {e}", args.exe.display());
            return ExitCode::from(1);
        }
    };

    // Validate the input actually is a PSX-EXE before we bake it into
    // an ISO -- silently packing a corrupt file would produce a disc
    // that just sits at the BIOS screen, which is annoying to debug.
    if let Err(e) = Exe::parse(&exe_bytes) {
        eprintln!("{}: not a valid PSX-EXE ({e:?})", args.exe.display());
        return ExitCode::from(1);
    }

    let mut builder = IsoBuilder::new().volume_id(&args.volume);
    if let Some(path) = args.system_area.as_deref() {
        let bytes = match read_system_area(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(1);
            }
        };
        builder = match builder.system_area(bytes) {
            Ok(builder) => builder,
            Err(_) => {
                eprintln!(
                    "{} did not decode to exactly 16 cooked sectors",
                    path.display()
                );
                return ExitCode::from(1);
            }
        };
    } else {
        eprintln!(
            "warning: no PS1 system area supplied; emulators may boot this, real hardware may not"
        );
    }
    builder.add_file("SYSTEM.CNF", default_system_cnf());
    if let Some(sectors) = args.cdtest_sectors {
        builder.add_file(CD_STREAM_BENCH_FILE_NAME, cd_stream_bench_payload(sectors));
    }
    if let Some(dir) = args.world_pack_rooms_dir.as_deref() {
        let pack = match build_world_pack_from_rooms_dir(dir, args.world_pack_order_file.as_deref())
        {
            Ok(pack) => pack,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(1);
            }
        };
        builder.add_file(WORLD_PACK_FILE_NAME, pack);
    }
    builder.add_file("PSX.EXE", exe_bytes);

    let (mut image, sector_size, format_label) = if args.cooked_iso {
        let iso = builder.build();
        (iso, psx_iso::iso9660::SECTOR_SIZE, "cooked .iso")
    } else {
        let bin = builder.build_bin();
        (bin, psx_iso::iso9660::RAW_SECTOR_SIZE, "raw .bin")
    };

    let cdda_tracks = if args.cdda_tracks.is_empty() {
        Vec::new()
    } else {
        match append_cdda_tracks_to_image(&mut image, &args.cdda_tracks) {
            Ok(tracks) => tracks,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(1);
            }
        }
    };

    if let Err(e) = fs::write(&args.out, &image) {
        eprintln!("write {}: {e}", args.out.display());
        return ExitCode::from(1);
    }

    if !args.cooked_iso {
        let cue_path = args.out.with_extension("cue");
        if let Err(e) = write_cue(&cue_path, &args.out, &cdda_tracks) {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
        println!("wrote {}", cue_path.display());
    }

    println!(
        "wrote {} ({} bytes = {} sectors, {format_label}) from {}",
        args.out.display(),
        image.len(),
        image.len() / sector_size,
        args.exe.display(),
    );
    ExitCode::SUCCESS
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CddaCueTrack {
    number: u8,
    index0_sector: u32,
    index1_sector: u32,
}

fn append_cdda_tracks_to_image(
    image: &mut Vec<u8>,
    tracks: &[PathBuf],
) -> Result<Vec<CddaCueTrack>, String> {
    let mut cue_tracks = Vec::with_capacity(tracks.len());
    for (index, track) in tracks.iter().enumerate() {
        if index >= 98 {
            return Err("CUE sheets can only address tracks 01 through 99".to_string());
        }
        let bytes = fs::read(track).map_err(|e| format!("read {}: {e}", track.display()))?;
        if bytes.is_empty() || bytes.len() % psx_iso::SECTOR_BYTES != 0 {
            return Err(format!(
                "{} is not a non-empty whole number of 2352-byte CDDA sectors",
                track.display()
            ));
        }
        let index0_sector = (image.len() / psx_iso::SECTOR_BYTES) as u32;
        image.resize(
            image.len() + CDDA_PREGAP_SECTORS as usize * psx_iso::SECTOR_BYTES,
            0,
        );
        let index1_sector = (image.len() / psx_iso::SECTOR_BYTES) as u32;
        image.extend_from_slice(&bytes);
        cue_tracks.push(CddaCueTrack {
            number: (index + 2) as u8,
            index0_sector,
            index1_sector,
        });
    }
    Ok(cue_tracks)
}

fn write_cue(
    cue_path: &Path,
    image_path: &Path,
    cdda_tracks: &[CddaCueTrack],
) -> Result<(), String> {
    let data_name = cue_file_name(image_path)?;
    let mut text = String::new();
    text.push_str(&format!(
        "FILE \"{}\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n",
        data_name
    ));
    for track in cdda_tracks {
        text.push_str(&format!(
            "  TRACK {:02} AUDIO\n    INDEX 00 {}\n    INDEX 01 {}\n",
            track.number,
            cue_msf(track.index0_sector),
            cue_msf(track.index1_sector)
        ));
    }
    fs::write(cue_path, text).map_err(|e| format!("write {}: {e}", cue_path.display()))
}

fn cue_msf(frames: u32) -> String {
    let m = frames / (60 * 75);
    let s = (frames / 75) % 60;
    let f = frames % 75;
    format!("{m:02}:{s:02}:{f:02}")
}

fn cue_file_name(path: &Path) -> Result<String, String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has no UTF-8 file name", path.display()))?;
    if name.contains('"') || name.contains('\n') || name.contains('\r') {
        return Err(format!(
            "{} is not safe for a CUE FILE line",
            path.display()
        ));
    }
    Ok(name.to_string())
}

fn read_system_area(path: &Path) -> Result<Vec<u8>, String> {
    let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    extract_system_area(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

fn extract_system_area(bytes: &[u8]) -> Result<Vec<u8>, String> {
    const COOKED_BYTES: usize = 16 * psx_iso::iso9660::SECTOR_SIZE;
    const RAW_BYTES: usize = 16 * psx_iso::iso9660::RAW_SECTOR_SIZE;

    if bytes.len() >= RAW_BYTES && looks_like_raw_sector(&bytes[..psx_iso::SECTOR_BYTES]) {
        let mut out = Vec::with_capacity(COOKED_BYTES);
        for sector in bytes[..RAW_BYTES].chunks_exact(psx_iso::SECTOR_BYTES) {
            out.extend_from_slice(&sector[24..24 + psx_iso::iso9660::SECTOR_SIZE]);
        }
        return Ok(out);
    }

    if bytes.len() >= COOKED_BYTES {
        return Ok(bytes[..COOKED_BYTES].to_vec());
    }

    Err(format!(
        "system area needs at least {COOKED_BYTES} cooked bytes or {RAW_BYTES} raw bytes"
    ))
}

fn looks_like_raw_sector(sector: &[u8]) -> bool {
    sector.len() >= psx_iso::SECTOR_BYTES
        && sector[0] == 0x00
        && sector[11] == 0x00
        && sector[1..11] == [0xFF; 10]
}

fn build_world_pack_from_rooms_dir(
    dir: &std::path::Path,
    order_file: Option<&std::path::Path>,
) -> Result<Vec<u8>, String> {
    let mut rooms = Vec::new();
    let entries = fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
        let path = entry.path();
        if !is_world_pack_room_payload(&path) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let Some(raw_index) = stem.strip_prefix("room_") else {
            continue;
        };
        let chunk_id = raw_index
            .parse::<u32>()
            .map_err(|_| format!("invalid room chunk filename: {}", path.display()))?;
        let bytes = fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        rooms.push((chunk_id, bytes));
    }
    rooms.sort_by_key(|(chunk_id, _)| *chunk_id);
    if rooms.is_empty() {
        return Err(format!(
            "no room_*.psxc or room_*.psxw files found in {}",
            dir.display()
        ));
    }
    if let Some(order_file) = order_file {
        let order = read_world_pack_order_file(order_file)?;
        apply_world_pack_order(&mut rooms, &order, order_file)?;
    }
    let refs: Vec<(u32, &[u8])> = rooms
        .iter()
        .map(|(chunk_id, bytes)| (*chunk_id, bytes.as_slice()))
        .collect();
    Ok(build_world_pack(&refs))
}

fn read_world_pack_order_file(path: &std::path::Path) -> Result<Vec<u32>, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    parse_world_pack_order(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn parse_world_pack_order(text: &str) -> Result<Vec<u32>, String> {
    let mut order = Vec::new();
    let mut seen = HashSet::new();
    for (line_index, line) in text.lines().enumerate() {
        let trimmed = line.split('#').next().unwrap_or("").trim();
        if trimmed.is_empty() {
            continue;
        }
        let room = trimmed
            .parse::<u32>()
            .map_err(|_| format!("line {} is not a room id: {trimmed}", line_index + 1))?;
        if !seen.insert(room) {
            return Err(format!(
                "duplicate room id {room} on line {}",
                line_index + 1
            ));
        }
        order.push(room);
    }
    Ok(order)
}

fn apply_world_pack_order(
    rooms: &mut Vec<(u32, Vec<u8>)>,
    order: &[u32],
    order_file: &std::path::Path,
) -> Result<(), String> {
    if order.is_empty() {
        return Err(format!(
            "{}: world pack order is empty",
            order_file.display()
        ));
    }
    let mut ordered = Vec::with_capacity(rooms.len());
    for &chunk_id in order {
        let Some(index) = rooms.iter().position(|(room, _)| *room == chunk_id) else {
            return Err(format!(
                "{}: room id {chunk_id} has no matching room_{chunk_id:03}.psxc/.psxw",
                order_file.display()
            ));
        };
        ordered.push(rooms.remove(index));
    }
    if !rooms.is_empty() {
        let missing = rooms
            .iter()
            .map(|(chunk_id, _)| chunk_id.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "{}: order file missing room ids {missing}",
            order_file.display()
        ));
    }
    *rooms = ordered;
    Ok(())
}

fn is_world_pack_room_payload(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("psxc" | "psxw")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_pack_order_parser_allows_comments_and_blank_lines() {
        assert_eq!(
            parse_world_pack_order(" 2\n# comment\n0 # spawn neighbour\n\n3\n").unwrap(),
            vec![2, 0, 3]
        );
    }

    #[test]
    fn world_pack_order_rejects_duplicates() {
        let err = parse_world_pack_order("1\n1\n").unwrap_err();
        assert!(err.contains("duplicate room id 1"));
    }

    #[test]
    fn world_pack_order_reorders_room_payloads() {
        let mut rooms = vec![(0, vec![0]), (1, vec![1]), (2, vec![2])];
        apply_world_pack_order(
            &mut rooms,
            &[2, 0, 1],
            std::path::Path::new("world_pack_order.txt"),
        )
        .unwrap();

        assert_eq!(
            rooms.iter().map(|(room, _)| *room).collect::<Vec<_>>(),
            vec![2, 0, 1]
        );
    }

    #[test]
    fn cue_file_name_rejects_unsafe_names() {
        assert_eq!(
            cue_file_name(std::path::Path::new("song.bin")).unwrap(),
            "song.bin"
        );
        assert!(cue_file_name(std::path::Path::new("bad\"name.bin")).is_err());
    }

    #[test]
    fn cue_msf_formats_file_relative_frames() {
        assert_eq!(cue_msf(0), "00:00:00");
        assert_eq!(cue_msf(74), "00:00:74");
        assert_eq!(cue_msf(75), "00:01:00");
        assert_eq!(cue_msf(150), "00:02:00");
    }

    #[test]
    fn extract_system_area_accepts_cooked_bytes() {
        let mut bytes = vec![0u8; 16 * psx_iso::iso9660::SECTOR_SIZE + 1];
        bytes[4 * psx_iso::iso9660::SECTOR_SIZE] = 0xA5;
        let area = extract_system_area(&bytes).unwrap();
        assert_eq!(area.len(), 16 * psx_iso::iso9660::SECTOR_SIZE);
        assert_eq!(area[4 * psx_iso::iso9660::SECTOR_SIZE], 0xA5);
    }

    #[test]
    fn extract_system_area_accepts_raw_bytes() {
        let mut bytes = vec![0u8; 16 * psx_iso::SECTOR_BYTES];
        for lba in 0..16 {
            let sector = &mut bytes[lba * psx_iso::SECTOR_BYTES..(lba + 1) * psx_iso::SECTOR_BYTES];
            sector[0] = 0;
            sector[1..11].fill(0xFF);
            sector[11] = 0;
        }
        bytes[4 * psx_iso::SECTOR_BYTES + 24] = 0x5A;
        let area = extract_system_area(&bytes).unwrap();
        assert_eq!(area.len(), 16 * psx_iso::iso9660::SECTOR_SIZE);
        assert_eq!(area[4 * psx_iso::iso9660::SECTOR_SIZE], 0x5A);
    }

    #[test]
    fn cdda_tracks_are_appended_to_single_bin_with_index_offsets() {
        let dir = std::env::temp_dir().join(format!(
            "mkisopsx-test-{}-{}",
            std::process::id(),
            "single-bin-cdda"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let track_path = dir.join("track02.cdda");
        let mut audio = vec![0x7Au8; psx_iso::SECTOR_BYTES * 2];
        audio[psx_iso::SECTOR_BYTES] = 0x5D;
        std::fs::write(&track_path, &audio).unwrap();

        let mut image = vec![0x11u8; psx_iso::SECTOR_BYTES * 10];
        let tracks = append_cdda_tracks_to_image(&mut image, &[track_path]).unwrap();
        assert_eq!(
            tracks,
            vec![CddaCueTrack {
                number: 2,
                index0_sector: 10,
                index1_sector: 160,
            }]
        );
        assert_eq!(image.len(), psx_iso::SECTOR_BYTES * 162);
        assert_eq!(image[10 * psx_iso::SECTOR_BYTES], 0);
        assert_eq!(image[160 * psx_iso::SECTOR_BYTES], 0x7A);
        assert_eq!(image[161 * psx_iso::SECTOR_BYTES], 0x5D);

        let cue_path = dir.join("game.cue");
        let image_path = dir.join("game.bin");
        write_cue(&cue_path, &image_path, &tracks).unwrap();
        let cue = std::fs::read_to_string(cue_path).unwrap();
        assert!(cue.contains("FILE \"game.bin\" BINARY\n"));
        assert!(cue.contains("  TRACK 02 AUDIO\n"));
        assert!(cue.contains("    INDEX 00 00:00:10\n"));
        assert!(cue.contains("    INDEX 01 00:02:10\n"));

        std::fs::remove_dir_all(dir).unwrap();
    }
}
