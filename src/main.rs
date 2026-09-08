//! pulse-limits: your Claude and Codex plan limits as a retro patient monitor. One binary:
//! the SwiftBar plugin body (`swiftbar`), the Waybar module (`waybar`), the document the panel
//! and the popover read (`payload`), the transcript accounting (`activity`, `estimate`), the
//! terminal monitor (`tui`) and the install/doctor commands. See `pulse-limits help`.

mod activity;
mod bar;
mod doctor;
mod estimate;
mod keychain;
mod open;
mod payload;
mod providers;
mod swiftbar;
mod tui;
mod util;
mod waybar;

use std::env;
use std::fs;

use serde_json::Value;

use payload::{BAR_INTERVAL, PANEL_INTERVAL};
use util::{cache_dir, config_dir, is_macos, write_atomic};

const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));

const HELP: &str = "pulse-limits: your Claude and Codex plan limits as a retro patient monitor.

  pulse-limits install       set up the bar item (SwiftBar on macOS, Waybar on Linux) and start it
  pulse-limits uninstall     remove the bar item, drop cache and settings
  pulse-limits bar on|off|status  the bar item alone: link it, unlink it, or say whether it is
  pulse-limits theme NAME    crt | modern | cyber | synth | analog
  pulse-limits provider NAME enable or disable a provider: grok | claude | codex
  pulse-limits refresh       force a live fetch now
  pulse-limits open          show or hide the monitor
  pulse-limits tui [NAME]    the monitor in the terminal (q quits; `pulse-limits claude` / `codex` are the same)
  pulse-limits status        print the current reading as JSON (every enabled provider)
  pulse-limits doctor        check every step of the chain, per provider (paste this when asking for help)
  pulse-limits raw [NAME]    print a provider's last raw usage reply (no secrets in it)
  pulse-limits keychain NAME pin the Keychain entry that holds the claude.ai login (macOS; see doctor)
  pulse-limits update        update to the latest release (Homebrew or git, whichever installed it)
  pulse-limits version       print the installed version
  pulse-limits waybar        print one Waybar JSON line (what the Waybar module runs)
  pulse-limits swiftbar      print the SwiftBar menu (what the SwiftBar shim runs)
  pulse-limits payload       print the document the panel reads, refreshing it (the popover runs this)
  pulse-limits activity      what Claude Code is doing now, from its transcripts, as JSON
  pulse-limits estimate PCT FETCHED  the dead-reckoned session % from a reading, as JSON
  pulse-limits reset         drop the cached readings so the next run asks live";

fn main() {
    let lib = util::lib_dir(); // from the invoked path and the PATH we were started with, before it is pinned
                               // SwiftBar hands the plugin launchd's PATH: pin our own. Waybar (or a Nix wrapper) hands us one worth keeping.
    let path = env::var("PATH").unwrap_or_default();
    env::set_var(
        "PATH",
        if is_macos() {
            "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin".to_string()
        } else {
            format!("{path}{}/usr/local/bin:/usr/bin:/bin", if path.is_empty() { "" } else { ":" })
        },
    );
    let args: Vec<String> = env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    let arg = args.get(1);
    let code = match cmd {
        "install" => bar::install(&lib),
        "uninstall" => bar::uninstall(&lib),
        "bar" => bar::bar(&lib, arg),
        "theme" => match payload::set_theme(arg.map(String::as_str).unwrap_or("")) {
            Ok(()) => {
                bar::refresh();
                println!("theme: {}", arg.unwrap());
                0
            }
            Err(e) => {
                eprintln!("{e}");
                64
            }
        },
        "provider" => match providers::toggle(arg.map(String::as_str).unwrap_or("")) {
            Ok(list) => {
                bar::refresh();
                println!("providers: {}", list.join(" "));
                0
            }
            Err(e) => {
                eprintln!("{e}");
                64
            }
        },
        "refresh" => {
            providers::reset();
            bar::refresh();
            println!("refreshing");
            0
        }
        "reset" => {
            providers::reset();
            0
        }
        "open" => open::open(&lib),
        "status" => {
            let b = payload::build(PANEL_INTERVAL, true, None);
            println!("{}", serde_json::to_string_pretty(&b.payload).unwrap_or_default());
            0
        }
        "payload" => {
            let b = payload::build(PANEL_INTERVAL, true, None);
            println!("{}", b.payload);
            0
        }
        "swiftbar" => {
            let b = payload::build(BAR_INTERVAL, true, None);
            print!("{}", swiftbar::render(&b, &lib));
            0
        }
        "waybar" => {
            let b = payload::build(BAR_INTERVAL, true, None);
            println!("{}", waybar::render(&b));
            0
        }
        "activity" => {
            println!("{}", activity::measure().to_json());
            0
        }
        "estimate" => match (args.get(1).and_then(|p| p.parse::<f64>().ok()), args.get(2).and_then(|f| f.parse::<i64>().ok())) {
            (Some(pct), Some(fetched)) => {
                // "13" stays 13 in the output, as the Swift helper printed it
                let api = args[1].parse::<i64>().ok().map(Value::from);
                println!("{}", estimate::estimate(pct, fetched).to_json(api));
                0
            }
            _ => {
                eprintln!("usage: pulse-limits estimate PCT FETCHED (a percentage and an epoch)");
                64
            }
        },
        "doctor" => doctor::run(&lib, VERSION),
        "raw" => raw(arg),
        "keychain" => keychain_pin(arg),
        "version" => {
            println!("{VERSION}");
            0
        }
        "update" => bar::update(&lib, VERSION),
        "tui" => tui::run(&args[1..]),
        "claude" | "codex" | "grok" => tui::run(&args),
        "help" | "-h" | "--help" => {
            println!("{HELP}");
            0
        }
        other => {
            eprintln!("pulse-limits: unknown command '{other}' (try: pulse-limits help)");
            64
        }
    };
    std::process::exit(code);
}

/// A provider's last good reply, else its last attempt.
fn raw(name: Option<&String>) -> i32 {
    let p = match name {
        Some(n) => n.clone(),
        None => match providers::enabled().first() {
            Some(p) => p.clone(),
            None => {
                eprintln!("no provider enabled. Run: pulse-limits provider claude");
                return 1;
            }
        },
    };
    let dir = cache_dir();
    let mut good = dir.join(format!("usage-{p}.json"));
    let mut last = dir.join(format!("last-reply-{p}.json"));
    // a pre-providers install that has not run the plugin yet still has Claude's files unsuffixed
    if p == "claude" && !good.is_file() && dir.join("usage.json").is_file() {
        good = dir.join("usage.json");
    }
    if p == "claude" && !last.is_file() && dir.join("last-reply.json").is_file() {
        last = dir.join("last-reply.json");
    }
    let pretty = |f: &std::path::Path| -> Option<String> {
        serde_json::from_slice::<Value>(&fs::read(f).ok()?).ok().and_then(|v| serde_json::to_string_pretty(&v).ok())
    };
    if good.is_file() {
        println!("{}", pretty(&good).unwrap_or_else(|| "not JSON".into()));
        0
    } else if last.is_file() {
        eprintln!("{p}: no successful reply yet; the last attempt was:");
        println!("{}", pretty(&last).unwrap_or_else(|| "not JSON".into()));
        0
    } else {
        eprintln!("{p}: no reply of any kind yet: the plugin has not called the API. Run: pulse-limits doctor");
        1
    }
}

/// Pins the Keychain item that holds the claude.ai login.
fn keychain_pin(name: Option<&String>) -> i32 {
    if !is_macos() {
        eprintln!("pulse-limits keychain: macOS only");
        return 64;
    }
    let Some(n) = name.filter(|n| !n.is_empty()) else {
        eprintln!("usage: pulse-limits keychain 'Claude Code-credentials-...'");
        return 64;
    };
    if let Err(e) = write_atomic(&config_dir().join("keychain"), format!("{n}\n").as_bytes()) {
        eprintln!("cannot write the pin: {e}");
        return 1;
    }
    providers::reset();
    bar::refresh();
    println!("pinned: {n}");
    0
}
