//! A deterministic stand-in for the emulator, used only by the performance
//! suite's tests. It records each run in the file named by `PSOXIDE_TEST_COUNT`,
//! writes a few artifacts next to `--config-dir`, and prints the launch summary
//! lines the suite parses. `PSOXIDE_TEST_MODE` selects `ok` (default), `fail`
//! (exit 2) or `cap` (report the instruction cap as reached). Lines of the
//! driver file named by the first argument that start with `print:` are echoed
//! to stdout, so a test can inject output by rewriting that hashed input.

use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let mode = std::env::var("PSOXIDE_TEST_MODE").unwrap_or_else(|_| "ok".to_string());
    let count = std::env::var("PSOXIDE_TEST_COUNT").expect("PSOXIDE_TEST_COUNT");
    if std::env::var_os("PSOXIDE_UNDECLARED_TEST").is_some() {
        eprintln!("undeclared PSOXIDE_ variable leaked into the replay environment");
        std::process::exit(3);
    }
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(count)
        .expect("count file");
    log.write_all(b"run\n").expect("count write");
    std::thread::sleep(std::time::Duration::from_millis(30));
    let config = flag("--config-dir").expect("--config-dir");
    let root = std::path::Path::new(&config)
        .parent()
        .expect("config parent");
    std::fs::create_dir(root.join("frames")).expect("frames dir");
    std::fs::write(root.join("frames/a.ppm"), b"frame1").expect("frame a");
    std::fs::write(root.join("frames/b.ppm"), b"frame2").expect("frame b");
    std::fs::write(root.join("audio.wav"), b"pcm").expect("audio");
    std::fs::write(root.join("state.json"), "{\"player\":1}").expect("state");
    if mode == "fail" {
        std::process::exit(2);
    }
    let steps: i64 = flag("--steps")
        .and_then(|v| v.parse().ok())
        .expect("--steps");
    let n = if mode == "cap" { steps } else { 10 };
    println!("tick={n}  cycles=20  pc=0x80010000  stopped-at={n}");
    println!(
        "route-ticks=5  port1-polls={}",
        flag("--stop-at-poll").unwrap_or_default()
    );
    println!("vram_fnv1a_64=0x1");
    println!("display_fnv1a_64=0x2  w=320  h=240");
    if let Some(driver) = args.get(1).and_then(|p| std::fs::read_to_string(p).ok()) {
        for line in driver.lines() {
            if let Some(text) = line.strip_prefix("print:") {
                println!("{text}");
            }
        }
    }
}
