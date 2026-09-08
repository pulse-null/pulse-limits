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
use std::path::Path;

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
    pin_path();
    let args: Vec<String> = env::args().skip(1).collect();
    std::process::exit(dispatch(&lib, &args));
}

/// SwiftBar hands the plugin launchd's PATH: pin our own. Waybar (or a Nix wrapper) hands us one worth keeping.
fn pin_path() {
    let path = env::var("PATH").unwrap_or_default();
    env::set_var(
        "PATH",
        if is_macos() {
            "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin".to_string()
        } else {
            format!("{path}{}/usr/local/bin:/usr/bin:/bin", if path.is_empty() { "" } else { ":" })
        },
    );
}

/// Runs one command line (without the program name) and returns its exit code.
fn dispatch(lib: &Path, args: &[String]) -> i32 {
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    let arg = args.get(1);
    match cmd {
        "install" => bar::install(lib),
        "uninstall" => bar::uninstall(lib),
        "bar" => bar::bar(lib, arg),
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
        "open" => open::open(lib),
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
            print!("{}", swiftbar::render(&b, lib));
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
        "doctor" => doctor::run(lib, VERSION),
        "raw" => raw(arg),
        "keychain" => keychain_pin(arg),
        "version" => {
            println!("{VERSION}");
            0
        }
        "update" => bar::update(lib, VERSION),
        "tui" => tui::run(&args[1..]),
        "claude" | "codex" | "grok" => tui::run(args),
        "help" | "-h" | "--help" => {
            println!("{HELP}");
            0
        }
        other => {
            eprintln!("pulse-limits: unknown command '{other}' (try: pulse-limits help)");
            64
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::testing::{refused, serve, Sandbox, ENV};

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn run(s: &Sandbox, list: &[&str]) -> i32 {
        dispatch(&s.scratch.0.join("lib"), &args(list))
    }

    #[test]
    fn argument_handling_and_exit_codes() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Sandbox::new("main-args");
        assert_eq!(run(&s, &[]), 0); // help
        for a in [["help"], ["-h"], ["--help"], ["version"]] {
            assert_eq!(run(&s, &a), 0, "{a:?}");
        }
        assert_eq!(run(&s, &["frobnicate"]), 64);
        // theme: written, then the bar is poked (open/pkill are stand-ins in the sandbox)
        assert_eq!(run(&s, &["theme", "synth"]), 0);
        assert_eq!(payload::theme(), "synth");
        assert_eq!(run(&s, &["theme", "neon"]), 64);
        assert_eq!(run(&s, &["theme"]), 64);
        // provider
        assert_eq!(run(&s, &["provider", "codex"]), 0);
        assert_eq!(providers::enabled(), vec!["codex"]);
        assert_eq!(run(&s, &["provider", "gemini"]), 64);
        assert_eq!(run(&s, &["provider"]), 64);
        // bar: the argument check only
        assert_eq!(run(&s, &["bar", "sideways"]), 64);
        // estimate
        assert_eq!(run(&s, &["estimate", "13", "1788876097"]), 0);
        assert!(s.scratch.cache().join("calib.json").is_file());
        assert_eq!(run(&s, &["estimate", "13.5", "1788876097"]), 0);
        assert_eq!(run(&s, &["estimate", "13"]), 64);
        assert_eq!(run(&s, &["estimate", "x", "1"]), 64);
        assert_eq!(run(&s, &["estimate", "13", "notanepoch"]), 64);
        // tui: bad arguments never reach a terminal
        assert_eq!(run(&s, &["tui", "gemini"]), 64);
        assert_eq!(run(&s, &["tui", "--bogus"]), 64);
        assert_eq!(run(&s, &["tui", "--theme", "neon"]), 64);
        assert_eq!(run(&s, &["tui", "--help"]), 0);
        assert_eq!(run(&s, &["claude", "--theme=neon"]), 64);
        assert_eq!(run(&s, &["codex", "--bogus"]), 64);
        assert_eq!(run(&s, &["grok", "gemini"]), 64);
        // keychain
        if is_macos() {
            assert_eq!(run(&s, &["keychain"]), 64);
            assert_eq!(run(&s, &["keychain", ""]), 64);
            assert_eq!(run(&s, &["keychain", "Claude Code-credentials-work"]), 0);
            assert_eq!(std::fs::read_to_string(config_dir().join("keychain")).unwrap(), "Claude Code-credentials-work\n");
        } else {
            assert_eq!(run(&s, &["keychain", "x"]), 64);
        }
        drop(s);
    }

    #[test]
    fn readings_raw_reset_and_the_path_pin() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Sandbox::new("main-readings");
        let cache = s.scratch.cache();
        // nothing enabled
        assert_eq!(run(&s, &["raw"]), 1);
        for cmd in ["status", "payload", "swiftbar", "waybar", "activity"] {
            assert_eq!(run(&s, &[cmd]), 0, "{cmd}");
        }
        assert!(cache.join("panel.url").is_file());
        // grok enabled with a login: status fetches from the local server, raw then shows the cached reply
        s.enable("grok");
        let grok = s.home().join(".grok");
        std::fs::create_dir_all(&grok).unwrap();
        std::fs::write(grok.join("auth.json"), r#"{"https://auth.x.ai::c": {"key": "opaque-token", "expires_at": "2099-01-01T00:00:00Z"}}"#).unwrap();
        std::fs::write(cache.join("plan-grok"), "SUPERGROK\n").unwrap();
        std::env::set_var("GROK_CLI_CHAT_PROXY_BASE_URL", serve(200, "{\"config\":{\"creditUsagePercent\":3,\"billingPeriodEnd\":\"2099-01-08T00:00:00Z\"}}"));
        assert_eq!(run(&s, &["status"]), 0);
        assert!(cache.join("usage-grok.json").is_file());
        std::env::set_var("GROK_CLI_CHAT_PROXY_BASE_URL", refused());
        assert_eq!(run(&s, &["raw"]), 0);
        assert_eq!(run(&s, &["raw", "grok"]), 0);
        assert_eq!(run(&s, &["swiftbar"]), 0);
        assert_eq!(run(&s, &["waybar"]), 0);
        // only a failed attempt: shown, and said so; nothing at all: 1; a cache that is not JSON: still 0
        std::fs::remove_file(cache.join("usage-grok.json")).unwrap();
        assert_eq!(run(&s, &["raw", "grok"]), 0);
        assert_eq!(run(&s, &["raw", "codex"]), 1);
        std::fs::write(cache.join("usage-grok.json"), "junk").unwrap();
        assert_eq!(run(&s, &["raw"]), 0);
        // an install from before the providers split still has Claude's files unsuffixed
        std::fs::write(cache.join("usage.json"), "{\"five_hour\":{\"utilization\":1}}").unwrap();
        std::fs::write(cache.join("last-reply.json"), "{\"http\":401}").unwrap();
        assert_eq!(run(&s, &["raw", "claude"]), 0);
        std::fs::remove_file(cache.join("usage.json")).unwrap();
        assert_eq!(run(&s, &["raw", "claude"]), 0);
        // refresh and reset drop the caches
        assert_eq!(run(&s, &["refresh"]), 0);
        assert!(!cache.join("usage-grok.json").exists());
        assert_eq!(run(&s, &["reset"]), 0);
        // the PATH pin: our own folders on macOS, the given PATH first on Linux
        let saved = std::env::var_os("PATH").unwrap();
        pin_path();
        let p = std::env::var("PATH").unwrap();
        assert!(p.ends_with("/usr/local/bin:/usr/bin:/bin"), "{p}");
        std::env::set_var("PATH", "");
        pin_path();
        assert!(std::env::var("PATH").unwrap().ends_with("/usr/local/bin:/usr/bin:/bin"));
        std::env::set_var("PATH", saved);
        drop(s);
    }
}
