//! `psx-fault IN.bin OUT.bin TABLE_LBA FAULT [N [M]]`: write one defect into a scene disc for the
//! scene core's failure gates (psx_iso::fault), writing OUT.bin and OUT.cue. TABLE_LBA is the
//! table the rows are read from (22, or a nested member's base + 22). FAULT is `table-magic`,
//! `image-byte ROW`, `short ROW`, `needs SCENE BIT` or `core-hash ROW`.

use psx_iso::fault::{apply, Fault};
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = || {
        eprintln!(
            "usage: psx-fault IN.bin OUT.bin TABLE_LBA \
             (table-magic | image-byte ROW | short ROW | needs SCENE BIT | core-hash ROW)"
        );
        ExitCode::from(2)
    };
    let number = |i: usize| args.get(i).and_then(|a| a.parse::<u32>().ok());
    let (Some(input), Some(out), Some(table_lba), Some(kind)) =
        (args.first(), args.get(1), number(2), args.get(3))
    else {
        return usage();
    };
    let row = || number(4).and_then(|n| u8::try_from(n).ok());
    let fault = match kind.as_str() {
        "table-magic" => Some(Fault::TableMagic),
        "image-byte" => row().map(Fault::ImageByte),
        "short" => row().map(Fault::Short),
        "core-hash" => row().map(Fault::CoreHash),
        "needs" => match (number(4).and_then(|n| u16::try_from(n).ok()), number(5)) {
            (Some(scene), Some(bit)) => u8::try_from(bit)
                .ok()
                .map(|bit| Fault::Needs { scene, bit }),
            _ => None,
        },
        _ => None,
    };
    let Some(fault) = fault else {
        return usage();
    };
    let bytes = match std::fs::read(input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("psx-fault: {input}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let faulty = match apply(&bytes, table_lba, fault) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("psx-fault: {e:?}");
            return ExitCode::FAILURE;
        }
    };
    let out_path = Path::new(out.as_str());
    let name = out_path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let written = std::fs::write(out_path, &faulty).and_then(|()| {
        std::fs::write(
            out_path.with_extension("cue"),
            format!("FILE \"{name}\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n"),
        )
    });
    if let Err(e) = written {
        eprintln!("psx-fault: {out}: {e}");
        return ExitCode::FAILURE;
    }
    println!("psx-fault: {out}: {fault:?}");
    ExitCode::SUCCESS
}
