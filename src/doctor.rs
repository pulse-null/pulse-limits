//! `pulse-limits doctor`: every link of the chain, top to bottom, without printing a token.
//! Paste its output when asking for help.

use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::activity;
use crate::bar::{plugin_dir, waybar_dir, waybar_module, PLUGIN};
use crate::payload::{self, PANEL_INTERVAL};
use crate::providers::{self, bad, ok, say};
use crate::util::{command_output, is_macos, iso_utc, local_offset, local_stamp, now, process_running, short, tilde, which};

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
            Ok(_) => bad(&format!("{} does not include the module yet (pulse-limits bar on adds it)", cfg.display())),
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
    for p in &enabled {
        match activity::source(p) {
            Some(src) => {
                let newest = src.newest.map(|n| format!(", newest {} ago", short(now() - n))).unwrap_or_default();
                ok(&format!("{p}: {}, {} file{}{newest}", tilde(&src.root), src.files, if src.files == 1 { "" } else { "s" }));
            }
            None => ok(&format!("{p}: none")),
        }
    }
    ok(&b.payload.get("activity").map(Value::to_string).unwrap_or_default());
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::testing::{capture, refused, serve, Sandbox, ENV};

    const GROK_REPLY: &str = include_str!("../tests/fixtures/grok/fixture-200.json");

    #[test]
    fn the_chain_on_this_platform() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Sandbox::new("doctor-here");
        let lib = s.scratch.0.join("lib");
        let out = capture(|| assert_eq!(run(&lib, "vTEST"), 0));
        assert!(out.starts_with("PulseLimits doctor  ("), "{out}");
        assert!(out.contains(", pulse-limits vTEST\n"), "{out}");
        assert!(out.contains(&format!("  ok       pgrep: {}\n", s.bin().join("pgrep").display())), "{out}");
        assert!(
            out.contains(&format!(
                "files\n  ok       lib: {}\n  PROBLEM  pulse-limits.1m.sh missing (run ./build.sh for bin/*)\n  PROBLEM  panel.html missing",
                lib.display()
            )),
            "{out}"
        );
        if is_macos() {
            assert!(out.contains("  PROBLEM  SwiftBar not running (open -a SwiftBar)\n"), "{out}");
            assert!(out.contains(&format!("  PROBLEM  plugin not linked in {} (run: pulse-limits bar on)\n", plugin_dir().display())), "{out}");
        }
        assert!(
            out.contains("providers\n  PROBLEM  none enabled: pulse-limits provider claude\n  ok       CLI running now: none (the menu bar follows the first enabled one that runs)\nplugin run\n  ok       menu bar shows: nothing\nactivity\n"),
            "{out}"
        );
        assert!(out.ends_with("activity\n  ok       null\n"), "{out}"); // nothing enabled: no reading on top
                                                                        // every file in place, the plugin linked, two providers enabled: one with a login and a live reply, one without
        for f in [PLUGIN, "panel.html", "bin/pulse-limits", "bin/pulse-popover", "bin/pulse-menubar"] {
            crate::util::write_atomic(&lib.join(f), b"x").unwrap();
        }
        let link = plugin_dir().join(PLUGIN);
        std::fs::create_dir_all(plugin_dir()).unwrap();
        std::os::unix::fs::symlink(lib.join(PLUGIN), &link).unwrap();
        s.enable("grok codex");
        let grok = s.home().join(".grok");
        std::fs::create_dir_all(&grok).unwrap();
        std::fs::write(grok.join("auth.json"), r#"{"https://auth.x.ai::c": {"key": "opaque-token", "expires_at": "2099-01-01T00:00:00Z"}}"#).unwrap();
        std::fs::write(s.scratch.cache().join("plan-grok"), "SUPERGROK\n").unwrap(); // fresh: no settings lookup after the billing reply
        std::env::set_var("GROK_CLI_CHAT_PROXY_BASE_URL", serve(200, GROK_REPLY));
        let out = capture(|| {
            run(&lib, "vTEST");
        });
        assert!(out.contains("  ok       pulse-limits.1m.sh\n  ok       panel.html\n  ok       bin/pulse-limits\n"), "{out}");
        if is_macos() {
            assert!(out.contains(&format!("  ok       plugin linked: {} -> {}\n", link.display(), lib.join(PLUGIN).display())), "{out}");
        }
        assert!(out.contains("  ok       enabled: grok codex  (first is the menu bar default; toggle with: pulse-limits provider NAME)\n"), "{out}");
        assert!(out.contains("plugin run\n  ok       menu bar shows: grok\n  ok       grok: SUPERGROK WEEK 1%, fetched "), "{out}");
        assert!(out.contains("  PROBLEM  codex: NO LOGIN  (NO CODEX LOGIN ON THIS "), "{out}");
        assert!(out.contains("provider grok\n  ok       GROK_HOME="), "{out}");
        assert!(out.contains("provider codex\n  ok       CODEX_HOME="), "{out}");
        assert!(out.contains("  ok       reached through the plugin (not probed again: keep the calls rare)\n"), "{out}");
        assert!(out.ends_with("activity\n  ok       grok: none\n  ok       codex: none\n  ok       null\n"), "{out}");
        // the plugin file gone: a dangling link; grok's session logs appear: where, how many, how fresh, and the reading on top
        std::fs::remove_file(lib.join(PLUGIN)).unwrap();
        std::env::set_var("GROK_CLI_CHAT_PROXY_BASE_URL", refused());
        let session = grok.join("sessions").join("cwd").join("s1");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("updates.jsonl"), "").unwrap();
        let out = capture(|| {
            run(&lib, "vTEST");
        });
        assert!(out.contains("  PROBLEM  pulse-limits.1m.sh missing"), "{out}");
        assert!(out.contains("activity\n  ok       grok: ~/.grok/sessions, 1 file, newest "), "{out}");
        assert!(out.contains("S ago\n  ok       codex: none\n  ok       {\"tok_per_min\":0,\"idle_s\":"), "{out}");
        assert!(out.ends_with(",\"sessions\":1}\n"), "{out}");
        if is_macos() {
            assert!(out.contains(&format!("  PROBLEM  plugin link is dangling: {}\n", link.display())), "{out}");
        }
        drop(s);
    }

    #[test]
    fn the_waybar_chain() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Sandbox::new("doctor-waybar");
        let lib = s.scratch.0.join("lib");
        let out = capture(|| assert_eq!(run_on(&lib, "vTEST", false), 0));
        assert!(out.contains(" pulse-limits vTEST\n"), "{out}");
        assert!(out.contains(&format!("  ok       pgrep: {}\n", s.bin().join("pgrep").display())), "{out}");
        assert!(out.contains("  PROBLEM  bin/pulse-limits missing"), "{out}");
        assert!(!out.contains("bin/pulse-popover"), "{out}");
        assert!(
            out.contains(&format!(
                "waybar\n  PROBLEM  waybar not running\n  PROBLEM  module file not written (run: pulse-limits bar on)\n  PROBLEM  no Waybar config at {}\n",
                waybar_dir().display()
            )),
            "{out}"
        );
        // a config that does not include the module yet, then one that does, and the module file written
        let cfg = waybar_dir().join("config");
        crate::util::write_atomic(&cfg, b"{ \"modules-right\": [\"clock\"] }\n").unwrap();
        let out = capture(|| {
            run_on(&lib, "vTEST", false);
        });
        assert!(out.contains(&format!("  PROBLEM  {} does not include the module yet (pulse-limits bar on adds it)\n", cfg.display())), "{out}");
        let cfg = waybar_dir().join("config.jsonc");
        crate::util::write_atomic(&cfg, b"{ \"include\": [\"pulse-limits.jsonc\"], \"modules-right\": [\"custom/pulse-limits\"] }\n").unwrap();
        crate::util::write_atomic(&waybar_module(), b"{}").unwrap();
        let out = capture(|| {
            run_on(&lib, "vTEST", false);
        });
        assert!(out.contains(&format!("  ok       module file: {}\n  ok       {} includes the module\n", waybar_module().display(), cfg.display())), "{out}");
        drop(s);
    }
}
