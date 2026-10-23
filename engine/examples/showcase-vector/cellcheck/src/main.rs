//! cellcheck: verify that the repeated parts of a stance HUD grid frame are pixel-identical and
//! the symmetric parts symmetric.
//!
//! cellcheck <frame.ppm> --strict <legacy|legacy-back|layout.txt> [--mark out.ppm]
//!     Exact pixel comparison of full-height strips and mirror symmetry (see `strict`).
//! cellcheck <frame.ppm> [--cols 2,162] [--y0 8] [--pitch 40] [--levels 10,6,3,0]
//!           [--back-lit 4] [--no-back] [--bg-row 9] [--gem-y 22,32]
//!     The round-1 check on connected components (see `loose`).
//! cellcheck --compare <a.ppm> <b.ppm> [--mark out.ppm]
//!     Two frames of the same size must be equal pixel for pixel (a host raster against an
//!     emulator frame, say); the differing pixels are listed and painted magenta in `out.ppm`.
//!
//! The exit status is 0 when every check passed, 1 when one failed, 2 on a usage error.

mod frame;
mod loose;
mod strict;

use std::process::ExitCode;

use frame::Frame;

fn run_strict(f: &Frame, spec: &str, mark: Option<&str>) -> Result<bool, String> {
    let layout = match spec {
        "legacy" => strict::Layout::legacy(false),
        "legacy-back" => strict::Layout::legacy(true),
        path => strict::Layout::parse(
            &std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?,
        )?,
    };
    let mut marks = strict::Marks::default();
    let ok = strict::run(f, &layout, &mut marks)?;
    if let Some(path) = mark {
        write_marked(f, &marks.0, path)?;
    }
    Ok(ok)
}

fn write_marked(f: &Frame, marks: &[(i32, i32)], path: &str) -> Result<(), String> {
    let mut out = Frame {
        w: f.w,
        h: f.h,
        px: f.px.clone(),
    };
    for &(x, y) in marks {
        if out.contains(x, y) {
            out.px[y as usize * out.w + x as usize] = [255, 0, 255];
        }
    }
    out.write(path)
}

fn compare(a: &[String]) -> Result<bool, String> {
    let (Some(pa), Some(pb)) = (a.get(1), a.get(2)) else {
        return Err("usage: cellcheck --compare <a.ppm> <b.ppm> [--mark out.ppm]".into());
    };
    let (fa, fb) = (Frame::read(pa)?, Frame::read(pb)?);
    if (fa.w, fa.h) != (fb.w, fb.h) {
        return Err(format!(
            "size differs: {}x{} against {}x{}",
            fa.w, fa.h, fb.w, fb.h
        ));
    }
    let mut diff = Vec::new();
    for y in 0..fa.h as i32 {
        for x in 0..fa.w as i32 {
            if fa.at(x, y) != fb.at(x, y) {
                diff.push((x, y));
            }
        }
    }
    println!("{} of {} pixels differ", diff.len(), fa.w * fa.h);
    let shown = if a.iter().any(|s| s == "--all") {
        usize::MAX
    } else {
        12
    };
    for &(x, y) in diff.iter().take(shown) {
        println!("  ({x},{y}): {:?} against {:?}", fa.at(x, y), fb.at(x, y));
    }
    if let Some(i) = a.iter().position(|s| s == "--mark") {
        if let Some(path) = a.get(i + 1) {
            write_marked(&fa, &diff, path)?;
        }
    }
    Ok(diff.is_empty())
}

fn main() -> ExitCode {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = a.first() else {
        eprintln!("usage: cellcheck <frame.ppm> [--strict <layout>] [options]");
        return ExitCode::from(2);
    };
    let value_of = |key: &str| {
        a.iter()
            .position(|s| s == key)
            .and_then(|i| a.get(i + 1))
            .map(String::as_str)
    };
    let result = if path == "--compare" {
        compare(&a)
    } else {
        match Frame::read(path) {
            Ok(f) => match value_of("--strict") {
                Some(spec) => run_strict(&f, spec, value_of("--mark")),
                None => Ok(loose::run(&f, &a)),
            },
            Err(e) => Err(e),
        }
    };
    match result {
        Ok(ok) => {
            println!("{}", if ok { "ALL PASS" } else { "SOME FAILED" });
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}
