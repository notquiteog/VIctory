//! A stand-in for `victory.exe`, for trying the launcher without the game
//! (`victory-launcher.exe --victory-exe victory-stub.exe`). Not shipped.
//!
//! It answers what the launcher asks (`--version`, `warm --help`), prints
//! the arguments and `VICTORY_*` environment it was given, and for
//! `shaders` and `warm` counts up as the real program does: with
//! `--progress` in `PROGRESS <done> <total>` lines, without it in the real
//! program's own words.

use std::io::Write;
use std::time::{Duration, Instant};

fn number(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let command = args.first().map(String::as_str).unwrap_or_default();
    if command == "--version" {
        println!("victory 0.0.0-stub");
        return;
    }
    if has("--help") {
        println!(
            "Usage: victory-stub {command} [OPTIONS]\n\nOptions:\n      --progress  Print PROGRESS <done> <total> lines\n  -h, --help      Print help"
        );
        return;
    }
    eprintln!("stub: arguments: {args:?}");
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| k.starts_with("VICTORY_"))
        .collect();
    env.sort();
    for (name, value) in &env {
        eprintln!("stub: {name}={value}");
    }
    // `VICTORY_STUB_TOTAL`, `VICTORY_STUB_MS` and `VICTORY_STUB_FAIL` come
    // through the Advanced box of the launcher's settings.
    let total = number("VICTORY_STUB_TOTAL", 6000);
    let pause = Duration::from_millis(number("VICTORY_STUB_MS", 5));
    let began = Instant::now();
    let secs = || began.elapsed().as_secs();
    let out = std::io::stdout();
    match command {
        "shaders" | "warm" => {
            let progress = has("--progress");
            if command == "warm" {
                println!(
                    "{} pipelines known, 0 made before (stub0000), 0 bad; {total} to make, 4 processes",
                    total * 3
                );
            }
            for done in 1..=total {
                std::thread::sleep(pause);
                if progress && (done % 20 == 0 || done == total) {
                    println!("PROGRESS {done} {total}");
                } else if !progress && done % 1000 == 0 {
                    match command {
                        "warm" => println!("{done} of {total}, {} s", secs()),
                        _ => println!("{done} shaders in the cache, {} s", secs()),
                    }
                }
                let _ = out.lock().flush();
                if number("VICTORY_STUB_FAIL", 0) == done {
                    eprintln!("Error: the stub was asked to fail here");
                    std::process::exit(3);
                }
            }
            match command {
                "warm" => println!(
                    "{total} made in {} s (0 have no host pipeline, 0 ended their process); ms each: median 1.0, p90 1.0, p99 1.0, most 1, sum 1 s",
                    secs()
                ),
                _ => println!(
                    "{total} shaders in the cache, 0 do not translate, {} s",
                    secs()
                ),
            }
        }
        "run" => {
            println!("stub: the game would run now");
            std::thread::sleep(Duration::from_secs(number("VICTORY_STUB_SECONDS", 4)));
            println!("stub: the game was closed");
        }
        other => {
            eprintln!("stub: no such command: {other}");
            std::process::exit(2);
        }
    }
}
