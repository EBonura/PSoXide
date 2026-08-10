//! Headless host gate for resident loading, PVS and surface materialization.
//!
//! With no arguments, this reads quake-psx's raw `chunk_100.psb` through
//! `chunk_108.psb` files. Paths to individual raw XBSP files may be supplied.
//!
//! ```sh
//! cargo run -p psx-bsp --example xbsp_render_flight
//! ```

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use psx_bsp::render::{configure_projection, load_view, Camera, Renderer, DEFAULT_PACKET_WORDS};
use psx_bsp::resident::ResidentMap;
use psx_bsp::{ReadAt, Vec3I32};
use psx_math::int32::mul_q12_i32;

const DEFAULT_MAP_DIR: &str = "/Users/ebonura/Desktop/repos/quake-psx/build-psoxide/world-chunks";
const FIRST_MAP_ID: u32 = 100;
const LAST_MAP_ID: u32 = 108;
const FRAMES_PER_MAP: u32 = 8;
const EYE_HEIGHT_Q12: i32 = 22 << 12;
const FLIGHT_DISTANCE_Q12: i32 = 4 << 12;
const FNV_OFFSET: u32 = 0x811c_9dc5;
// Packet-route hashes from the raw maps cooked by quake-psx commit 83a6349.
// Hashing stops before OT pointer linking so host addresses never enter it.
const EXPECTED_MAP_HASHES: [(u32, u32); 9] = [
    (100, 0x6f11_0ec5),
    (101, 0x24c3_e6c2),
    (102, 0x2b50_91b4),
    (103, 0x0500_d24a),
    (104, 0x9a67_959d),
    (105, 0x4c51_dffa),
    (106, 0xac43_21de),
    (107, 0x733e_1a7c),
    (108, 0xf21c_f3bb),
];

struct FileReader {
    file: File,
    len: u32,
}

impl FileReader {
    fn open(path: &Path) -> std::io::Result<Self> {
        let file = File::open(path)?;
        let raw_len = file.metadata()?.len();
        let len = u32::try_from(raw_len).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{} is larger than the XBSP address space", path.display()),
            )
        })?;
        Ok(Self { file, len })
    }
}

impl ReadAt for FileReader {
    type Error = std::io::Error;

    fn len(&self) -> u32 {
        self.len
    }

    fn read_exact_at(&mut self, offset: u32, output: &mut [u8]) -> Result<(), Self::Error> {
        self.file.seek(SeekFrom::Start(offset as u64))?;
        self.file.read_exact(output)
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("xbsp render flight failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let paths = input_paths()?;
    configure_projection();
    let mut map = ResidentMap::new();
    let mut renderer = Renderer::new();
    let mut packets = vec![0u32; DEFAULT_PACKET_WORDS];
    let mut total_faces = 0u64;
    let mut total_batches = 0u64;

    for path in &paths {
        let map_id = map_id(path)?;
        let mut reader = FileReader::open(path).map_err(|error| format!("{error}"))?;
        map.load(map_id, &mut reader)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let start = map
            .entities()
            .get(1)
            .ok_or_else(|| format!("{}: no cooked player start", path.display()))?;
        let mut map_hash = FNV_OFFSET;

        for frame_index in 0..FRAMES_PER_MAP {
            let camera = flight_camera(start.origin, start.angles.y, frame_index);
            let view = load_view(camera);
            let frame = renderer.draw_frame(&map, camera, view, &[], 0, &mut packets);
            if frame.stats.visible_faces == 0 || frame.stats.surface_batches == 0 {
                return Err(format!(
                    "{} frame {frame_index}: faces={} batches={}",
                    path.display(),
                    frame.stats.visible_faces,
                    frame.stats.surface_batches,
                ));
            }
            total_faces += u64::from(frame.stats.visible_faces);
            total_batches += u64::from(frame.stats.surface_batches);
            let packet_words = &packets[..frame.packet_words];
            let frame_hash = hash_words(FNV_OFFSET, packet_words);
            map_hash = hash_word(map_hash, frame_index);
            map_hash = hash_word(map_hash, frame.packet_words as u32);
            map_hash = hash_words(map_hash, packet_words);
            println!(
                "map={map_id} frame={frame_index} faces={} batches={} packets={} triangles={} words={} hash={frame_hash:08x}",
                frame.stats.visible_faces,
                frame.stats.surface_batches,
                frame.stats.packets,
                frame.stats.hardware_triangles,
                frame.packet_words,
            );
        }
        if let Some((_, expected)) = EXPECTED_MAP_HASHES
            .iter()
            .find(|(expected_id, _)| *expected_id == map_id)
        {
            if map_hash != *expected {
                return Err(format!(
                    "{}: packet hash {map_hash:08x}, expected {expected:08x}",
                    path.display()
                ));
            }
        }
        println!("map={map_id} packet_route_hash={map_hash:08x}");
    }

    println!(
        "gate=ok maps={} frames={} faces={} batches={}",
        paths.len(),
        paths.len() as u32 * FRAMES_PER_MAP,
        total_faces,
        total_batches,
    );
    Ok(())
}

fn hash_words(mut hash: u32, words: &[u32]) -> u32 {
    for &word in words {
        hash = hash_word(hash, word);
    }
    hash
}

fn hash_word(mut hash: u32, word: u32) -> u32 {
    for byte in word.to_le_bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

fn input_paths() -> Result<Vec<PathBuf>, String> {
    let supplied: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if !supplied.is_empty() {
        return Ok(supplied);
    }
    let directory = Path::new(DEFAULT_MAP_DIR);
    let paths: Vec<PathBuf> = (FIRST_MAP_ID..=LAST_MAP_ID)
        .map(|id| directory.join(format!("chunk_{id}.psb")))
        .collect();
    if let Some(missing) = paths.iter().find(|path| !path.is_file()) {
        return Err(format!(
            "{} is missing; pass raw XBSP paths explicitly",
            missing.display()
        ));
    }
    Ok(paths)
}

fn map_id(path: &Path) -> Result<u32, String> {
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| format!("{} has no UTF-8 file stem", path.display()))?;
    stem.strip_prefix("chunk_")
        .unwrap_or(stem)
        .parse()
        .map_err(|_| format!("{} does not end in a numeric map ID", path.display()))
}

fn flight_camera(origin: Vec3I32, yaw: i16, frame_index: u32) -> Camera {
    let phase = frame_index as i32;
    let denominator = (FRAMES_PER_MAP - 1) as i32;
    let distance = FLIGHT_DISTANCE_Q12.saturating_mul(phase) / denominator;
    let yaw_q12 = yaw as u16 & 0x0fff;
    Camera {
        origin: Vec3I32 {
            x: origin
                .x
                .saturating_add(mul_q12_i32(distance, psx_math::cos_q12(yaw_q12))),
            y: origin
                .y
                .saturating_add(mul_q12_i32(distance, psx_math::sin_q12(yaw_q12))),
            z: origin.z.saturating_add(EYE_HEIGHT_Q12),
        },
        angles: [0, yaw.wrapping_add(frame_index as i16 * 16), 0],
    }
}
