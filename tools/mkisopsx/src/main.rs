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
//! `CDTEST.BIN` stream-benchmark data before `PSX.EXE`.
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
use std::path::PathBuf;
use std::process::ExitCode;

struct Args {
    exe: PathBuf,
    out: PathBuf,
    volume: String,
    cooked_iso: bool,
    cdtest_sectors: Option<usize>,
    world_pack_rooms_dir: Option<PathBuf>,
    world_pack_order_file: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut exe: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut volume = String::from("PSOXIDE");
    let mut cooked_iso = false;
    let mut cdtest_sectors = None;
    let mut world_pack_rooms_dir = None;
    let mut world_pack_order_file = None;
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

    let (image, sector_size, format_label) = if args.cooked_iso {
        let iso = builder.build();
        (iso, psx_iso::iso9660::SECTOR_SIZE, "cooked .iso")
    } else {
        let bin = builder.build_bin();
        (bin, psx_iso::iso9660::RAW_SECTOR_SIZE, "raw .bin")
    };

    if let Err(e) = fs::write(&args.out, &image) {
        eprintln!("write {}: {e}", args.out.display());
        return ExitCode::from(1);
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
}
