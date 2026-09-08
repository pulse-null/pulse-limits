//! `pulse-limits doctor`: every link of the chain, top to bottom, without printing a token.
//! Paste its output when asking for help.

use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::bar::{plugin_dir, waybar_dir, waybar_module, PLUGIN};
use crate::payload::{self, PANEL_INTERVAL};
use crate::providers::{self, bad, ok, say};
use crate::util::{command_output, is_macos, iso_utc, local_offset, local_stamp, now, process_running, which};

pub fn run(lib: &Path, version: &str) -> i32 {
    run_on(lib, version, is_macos())
}

/// The report for one platform: `macos` picks the SwiftBar or the Waybar chain.
fn run_on(lib: &Path, version: &str, macos: bool) -> i32 {
    say(&format!("PulseLimits doctor  ({})", local_stamp(now(), local_offset())));
    say("system");
    let arch = command_output("uname", &["-m"]).unwrap_or_default();
    if macos {
        ok(&format!("macOS {} {arch}, pulse-limits {version}", command_output("sw_vers", &["-productVersion"]).unwrap_or_default()));
        if command_output("readlink", &["-f", "/"]).is_some() {
            ok("readlink -f works");
        } else {
            bad("readlink -f unsupported (macOS older than 12.3): the SwiftBar shim will not find the binary");
        }
    } else {
        ok(&format!("{} {arch}, pulse-limits {version}", command_output("uname", &["-sr"]).unwrap_or_default()));
    }
    let tools: &[&str] = if macos { &["security", "pgrep"] } else { &["pgrep", "xdg-open"] };
    for t in tools {
        match which(t) {
            Some(p) => ok(&format!("{t}: {}", p.display())),
            None => bad(&format!("{t} is missing")),
        }
    }
    say("files");
    ok(&format!("lib: {}", lib.display()));
    let files: &[&str] = if macos {
        &[PLUGIN, "panel.html", "bin/pulse-limits", "bin/pulse-popover", "bin/pulse-menubar"]
    } else {
        &[PLUGIN, "panel.html", "bin/pulse-limits"]
    };
    for f in files {
        if lib.join(f).exists() {
            ok(f);
        } else {
            bad(&format!("{f} missing (run ./build.sh for bin/*)"));
        }
    }
    if macos {
        say("swiftbar");
        if crate::bar::swiftbar_installed() {
            ok(&format!(
                "SwiftBar {} installed",
                command_output("defaults", &["read", "/Applications/SwiftBar.app/Contents/Info.plist", "CFBundleShortVersionString"]).unwrap_or_default()
            ));
        } else {
            bad("SwiftBar not in /Applications");
        }
        if process_running("SwiftBar") {
            ok("SwiftBar running");
        } else {
            bad("SwiftBar not running (open -a SwiftBar)");
        }
        let link = plugin_dir().join(PLUGIN);
        match fs::read_link(&link) {
            Ok(t) if link.exists() => ok(&format!("plugin linked: {} -> {}", link.display(), t.display())),
            Ok(_) => bad(&format!("plugin link is dangling: {}", link.display())),
            Err(_) => bad(&format!("plugin not linked in {} (run: pulse-limits bar on)", plugin_dir().display())),
        }
    } else {
        say("waybar");
        if process_running("waybar") {
            ok("waybar running");
        } else {
            bad("waybar not running");
        }
        let module = waybar_module();
        if module.is_file() {
            ok(&format!("module file: {}", module.display()));
        } else {
            bad("module file not written (run: pulse-limits bar on)");
        }
        let mut cfg = waybar_dir().join("config.jsonc");
        if !cfg.is_file() {
            cfg = waybar_dir().join("config");
        }
        match fs::read_to_string(&cfg) {
            Ok(text) if text.contains("pulse-limits.jsonc") && text.contains("\"custom/pulse-limits\"") => {
                ok(&format!("{} includes the module", cfg.display()))
            }
            Ok(_) => bad(&format!("{} does not include the module yet (pulse-limits bar on prints the two lines)", cfg.display())),
            Err(_) => bad(&format!("no Waybar config at {}", waybar_dir().display())),
        }
    }
    say("providers");
    let enabled = providers::enabled();
    if enabled.is_empty() {
        bad("none enabled: pulse-limits provider claude");
    } else {
        ok(&format!("enabled: {}  (first is the menu bar default; toggle with: pulse-limits provider NAME)", enabled.join(" ")));
    }
    let running: Vec<&str> = enabled.iter().filter(|p| process_running(p)).map(String::as_str).collect();
    ok(&format!(
        "CLI running now: {} (the menu bar follows the first enabled one that runs)",
        if running.is_empty() { "none".to_string() } else { running.join(" ") }
    ));
    say("plugin run");
    let b = payload::build(PANEL_INTERVAL, true, None);
    ok(&format!("menu bar shows: {}", if b.active.is_empty() { "nothing" } else { &b.active }));
    for d in b.docs.iter().filter(|d| enabled.contains(&d.provider)) {
        if d.status.is_empty() {
            let wins: Vec<String> = d.windows.iter().map(|w| format!("{} {}%", w.label, w.pct_f().floor())).collect();
            ok(&format!("{}: {} {}, fetched {}", d.provider, d.plan, wins.join(", "), iso_utc(d.fetched)));
        } else {
            bad(&format!("{}: {}  ({})", d.provider, d.status, d.hint));
        }
    }
    for p in &enabled {
        say(&format!("provider {p}"));
        let pstatus = b.docs.iter().find(|d| &d.provider == p).map(|d| d.status.clone()).unwrap_or_default();
        providers::doctor(p, &pstatus);
    }
    say("activity");
    ok(&b.payload.get("activity").map(Value::to_string).unwrap_or_default());
    0
}
