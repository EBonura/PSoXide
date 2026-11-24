//! `psx-nest OUTER.bin POSITION MEMBER.bin OUT.bin [--no-entry-bit]`: nest a member scene disc
//! inside an outer one at outer image row POSITION (psx_iso::nest), writing OUT.bin and OUT.cue.
//! `--no-entry-bit` leaves bit 0 out of the row's word 5, breaking the nesting rule on purpose
//! for a negative test.

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let entry_bit = !args.iter().any(|a| a == "--no-entry-bit");
    let plain: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let [outer, position, member, out] = plain.as_slice() else {
        eprintln!("usage: psx-nest OUTER.bin POSITION MEMBER.bin OUT.bin [--no-entry-bit]");
        return ExitCode::from(2);
    };
    let Ok(position) = position.parse::<u8>() else {
        eprintln!("psx-nest: POSITION must be a row index");
        return ExitCode::from(2);
    };
    let read = |p: &str| std::fs::read(p).map_err(|e| format!("{p}: {e}"));
    let result = read(outer).and_then(|o| {
        read(member).and_then(|m| {
            psx_iso::nest::nest(&o, position, &m, entry_bit).map_err(|e| format!("{e:?}"))
        })
    });
    let bytes = match result {
        Ok(b) => b,
        Err(e) => {
            eprintln!("psx-nest: {e}");
            return ExitCode::FAILURE;
        }
    };
    let out_path = Path::new(out.as_str());
    let name = out_path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let cue = out_path.with_extension("cue");
    let written = std::fs::write(out_path, &bytes).and_then(|()| {
        std::fs::write(
            &cue,
            format!("FILE \"{name}\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n"),
        )
    });
    if let Err(e) = written {
        eprintln!("psx-nest: {out}: {e}");
        return ExitCode::FAILURE;
    }
    println!(
        "psx-nest: {out} ({} sectors), member at row {position}",
        bytes.len() / 2352
    );
    ExitCode::SUCCESS
}
