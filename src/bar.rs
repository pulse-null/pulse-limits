//! The bar item alone: the SwiftBar plugin link on macOS, the Waybar module file on Linux.
//! `install` is the checks plus `bar on`; `uninstall` is `bar off` plus cache and settings.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::keychain;
use crate::util::{cache_dir, command_output, config_dir, home, is_executable, is_macos, process_running, run_quiet, tilde, which};

pub const PLUGIN: &str = "pulse-limits.1m.sh";
pub const OLD_PLUGIN: &str = "pulse-limits.5m.sh"; // pre-0.4 installs linked this name

/// Where SwiftBar looks for plugins.
pub fn plugin_dir() -> PathBuf {
    command_output("defaults", &["read", "com.ameba.SwiftBar", "PluginDirectory"])
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config").join("swiftbar").join("plugins"))
}

pub fn waybar_dir() -> PathBuf {
    match env::var_os("XDG_CONFIG_HOME") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => home().join(".config"),
    }
    .join("waybar")
}

/// Linux: written by `bar on`, included from the user's own config.
pub fn waybar_module() -> PathBuf {
    waybar_dir().join("pulse-limits.jsonc")
}

/// Asks the bar to run the plugin again.
pub fn refresh() {
    if is_macos() {
        run_quiet("open", &["swiftbar://refreshallplugins"]);
    } else {
        run_quiet("pkill", &["-RTMIN+8", "waybar"]);
    }
}

pub fn kill_popover() {
    run_quiet("pkill", &["-f", "bin/pulse-popover"]);
}

pub fn swiftbar_installed() -> bool {
    #[cfg(test)]
    if let Some(app) = crate::util::env_path("PULSE_TEST_SWIFTBAR_APP") {
        return app.is_dir();
    }
    Path::new("/Applications/SwiftBar.app").is_dir()
}

/// The checks, then `bar on`.
pub fn install(lib: &Path) -> i32 {
    if is_macos() {
        if !swiftbar_installed() {
            // the one prerequisite Homebrew's formula cannot pull in itself: a cask
            if which("brew").is_some() {
                println!("installing SwiftBar with Homebrew");
                let ok = std::process::Command::new("brew").args(["install", "--cask", "swiftbar"]).status().map(|s| s.success()).unwrap_or(false);
                if !ok || !swiftbar_installed() {
                    eprintln!("could not install SwiftBar; get it from https://github.com/swiftbar/SwiftBar/releases and re-run");
                    return 1;
                }
            } else {
                eprintln!("SwiftBar is not installed: brew install --cask swiftbar, or https://github.com/swiftbar/SwiftBar/releases");
                return 1;
            }
        }
        if !is_executable(&lib.join("bin").join("pulse-popover")) || !is_executable(&lib.join("bin").join("pulse-menubar")) {
            let ok = std::process::Command::new(lib.join("build.sh")).current_dir(lib).status().map(|s| s.success()).unwrap_or(false);
            if !ok {
                eprintln!("could not build the helpers: run ./build.sh in {}", lib.display());
                return 1;
            }
        }
        if keychain::find_login().is_none() {
            eprintln!("warning: no Claude Code login in the Keychain. Run 'claude' once and log in; the widget reads that token.");
        }
    } else {
        if which("waybar").is_none() {
            eprintln!("warning: waybar not found; the command still works, the bar item will not show until it is.");
        }
        if !keychain::credential_files().iter().any(|f| f.is_file()) {
            eprintln!(
                "warning: no Claude Code login found ({}). Run 'claude' once and log in; the widget reads that token.",
                keychain::credential_files()[0].display()
            );
        }
    }
    // no default provider: the first install enables the CLIs that have a login here
    let pf = config_dir().join("providers");
    if !pf.is_file() {
        let d = crate::providers::detected();
        let _ = fs::create_dir_all(config_dir());
        let _ = fs::write(&pf, format!("{}\n", d.join("\n")));
        if d.is_empty() {
            eprintln!("no CLI login found on this machine: run grok login, claude or codex login, then: pulse-limits provider NAME");
        } else {
            println!("providers enabled from the logins found: {}", d.join(" "));
        }
    }
    on(lib)
}

pub fn uninstall(lib: &Path) -> i32 {
    off(lib);
    let _ = fs::remove_dir_all(cache_dir());
    let _ = fs::remove_dir_all(config_dir());
    println!("cache and settings removed.");
    0
}

/// `bar on|off|status`
pub fn bar(lib: &Path, what: Option<&String>) -> i32 {
    match what.map(String::as_str).unwrap_or("status") {
        "on" => on(lib),
        "off" => off(lib),
        "status" => status(lib),
        _ => {
            eprintln!("usage: pulse-limits bar on|off|status");
            64
        }
    }
}

/// How Waybar should run us: by name when `pulse-limits` on PATH is this binary (or we were
/// invoked by name), else by absolute path.
fn command_name() -> String {
    let argv0 = env::args().next().unwrap_or_default();
    let exe = env::current_exe().ok().and_then(|p| p.canonicalize().ok());
    let on_path = which("pulse-limits").and_then(|p| p.canonicalize().ok());
    if !argv0.contains('/') || (on_path.is_some() && on_path == exe) {
        "pulse-limits".into()
    } else {
        format!("'{}'", exe.map(|p| p.display().to_string()).unwrap_or(argv0))
    }
}

pub fn on(lib: &Path) -> i32 {
    if is_macos() {
        let d = plugin_dir();
        let _ = fs::create_dir_all(&d);
        run_quiet("defaults", &["write", "com.ameba.SwiftBar", "PluginDirectory", &d.display().to_string()]);
        let _ = fs::remove_file(d.join(OLD_PLUGIN));
        let link = d.join(PLUGIN);
        let _ = fs::remove_file(&link);
        if let Err(e) = std::os::unix::fs::symlink(lib.join(PLUGIN), &link) {
            eprintln!("cannot link {}: {e}", link.display());
            return 1;
        }
        kill_popover();
        if process_running("SwiftBar") {
            refresh();
        } else {
            run_quiet("open", &["-a", "SwiftBar"]);
        }
        println!("linked {} -> {}", link.display(), lib.join(PLUGIN).display());
        println!("Look for the ring at the right of the menu bar. Left-click opens the monitor, right-click picks a theme and the providers.");
    } else {
        let cmd = command_name();
        let module = waybar_module();
        let _ = fs::create_dir_all(waybar_dir());
        let text = format!(
            r#"// PulseLimits Waybar module. Written by `pulse-limits bar on`, deleted by `pulse-limits bar off`.
// Include it from your own config and add "custom/pulse-limits" to a modules list; the ring
// glyphs are Nerd Font (md-circle-slice-1..8), picked by Waybar from the percentage.
{{
  "custom/pulse-limits": {{
    "exec": "{cmd} waybar",
    "return-type": "json",
    "interval": 60,
    "format": "{{text}} {{icon}}",
    "format-icons": ["󰪞", "󰪟", "󰪠", "󰪡", "󰪢", "󰪣", "󰪤", "󰪥"],
    "on-click": "{cmd} open",
    "signal": 8,
    "tooltip": true
  }}
}}
"#
        );
        if let Err(e) = fs::write(&module, text) {
            eprintln!("cannot write {}: {e}", module.display());
            return 1;
        }
        println!("wrote {}", module.display());
        println!("Add these two lines to your Waybar config ({}/config.jsonc):", waybar_dir().display());
        println!("  \"include\": [\"{}\"],", tilde(&module));
        println!("  \"modules-right\": [..., \"custom/pulse-limits\"],");
        println!("and the tones to style.css (the class follows the session window):");
        println!("  #custom-pulse-limits {{ min-width: 12px; margin: 0 7.5px; }}");
        println!("  #custom-pulse-limits.warn  {{ color: #ffb000; }}");
        println!("  #custom-pulse-limits.crit  {{ color: #ff5c5c; }}");
        println!("  #custom-pulse-limits.stale {{ color: #ffb000; }}");
        println!("  #custom-pulse-limits.dead  {{ color: #8c8c8c; }}");
        println!("Then reload Waybar (pkill -SIGUSR2 waybar; on Omarchy: omarchy-restart-waybar).");
    }
    0
}

pub fn off(_lib: &Path) -> i32 {
    if is_macos() {
        kill_popover();
        let d = plugin_dir();
        let _ = fs::remove_file(d.join(PLUGIN));
        let _ = fs::remove_file(d.join(OLD_PLUGIN));
        refresh();
        println!("unlinked. SwiftBar itself is untouched.");
    } else {
        let module = waybar_module();
        let _ = fs::remove_file(&module);
        println!("removed {}", module.display());
        println!("Now drop \"{}\" from \"include\" and \"custom/pulse-limits\" from your modules", tilde(&module));
        println!("in {}/config.jsonc, and the #custom-pulse-limits rules from style.css, then reload Waybar.", waybar_dir().display());
    }
    0
}

pub fn status(_lib: &Path) -> i32 {
    if is_macos() {
        let link = plugin_dir().join(PLUGIN);
        match fs::read_link(&link) {
            Ok(target) if link.exists() => println!("on: {} -> {}", link.display(), target.display()),
            _ => {
                println!("off: not linked in {} (pulse-limits bar on)", plugin_dir().display());
                return 1;
            }
        }
        if process_running("SwiftBar") {
            println!("SwiftBar: running");
        } else {
            println!("SwiftBar: not running (open -a SwiftBar)");
        }
    } else {
        let module = waybar_module();
        if module.is_file() {
            println!("on: {}", module.display());
        } else {
            println!("off: no {} (pulse-limits bar on)", module.display());
            return 1;
        }
        println!("waybar: {}", if process_running("waybar") { "running" } else { "not running" });
    }
    0
}

/// `pulse-limits update`: Homebrew or git, whichever installed it.
pub fn update(lib: &Path, version: &str) -> i32 {
    let lib_s = lib.display().to_string();
    if lib_s.starts_with("/nix/store/") {
        eprintln!("installed with Nix: update the flake input and rebuild (nix flake update pulse-limits)");
        return 1;
    } else if lib_s.contains("/Cellar/pulse-limits/") || lib_s.contains("/opt/pulse-limits/") {
        println!("installed with Homebrew: upgrading");
        if !std::process::Command::new("brew").args(["upgrade", "pulse-null/tap/pulse-limits"]).status().map(|s| s.success()).unwrap_or(false) {
            eprintln!("brew upgrade failed");
            return 1;
        }
    } else if lib.join(".git").exists() {
        println!("git checkout at {lib_s}: pulling");
        if !std::process::Command::new("git").args(["-C", &lib_s, "pull", "--ff-only"]).status().map(|s| s.success()).unwrap_or(false) {
            eprintln!("git pull failed (local changes?)");
            return 1;
        }
        if !std::process::Command::new(lib.join("build.sh")).current_dir(lib).status().map(|s| s.success()).unwrap_or(false) {
            eprintln!("build failed");
            return 1;
        }
    } else {
        eprintln!("installed by copying the folder: re-run the installer");
        eprintln!("  curl -fsSL https://raw.githubusercontent.com/pulse-null/pulse-limits/main/install.sh | bash");
        return 1;
    }
    kill_popover();
    let _ = fs::remove_file(cache_dir().join("popover.pid"));
    let _ = fs::remove_file(cache_dir().join("popover.closed"));
    // relink through the new binary, in case the layout changed
    let new = lib.join("bin").join("pulse-limits");
    let _ = std::process::Command::new(&new).arg("install").stdout(std::process::Stdio::null()).status();
    let after = command_output(&new.display().to_string(), &["version"]).unwrap_or_default();
    if after == version {
        println!("already up to date: {after}");
    } else {
        println!("updated: {version} -> {after}");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::testing::{Scratch, ENV};
    use crate::util::testing::{calls, fake_bin, pretend, Vars};

    /// Every command the bar code may run, as fakes: `defaults read` answers the scratch
    /// plugin folder, `pgrep -x NAME` says yes when `bin/running.NAME` exists, the rest log.
    fn fakes(s: &Scratch) -> (PathBuf, PathBuf) {
        let bin = s.0.join("bin");
        let plugins = s.0.join("plugins");
        fake_bin(&bin, "defaults", &format!("[ \"$1\" = read ] && printf '%s\\n' '{}'; exit 0", plugins.display()));
        fake_bin(&bin, "open", "");
        fake_bin(&bin, "pkill", "");
        fake_bin(&bin, "pgrep", &format!("[ -e '{}/running.'\"$2\" ]", bin.display()));
        fake_bin(&bin, "security", "exit 1");
        fake_bin(&bin, "git", "exit 1");
        fake_bin(&bin, "xdg-open", "");
        (bin, plugins)
    }

    /// A lib folder with the shim and, when asked, the two built helpers.
    fn lib_fixture(s: &Scratch, helpers: bool) -> PathBuf {
        let lib = s.0.join("lib");
        fs::create_dir_all(lib.join("bin")).unwrap();
        fs::write(lib.join(PLUGIN), "#!/bin/bash\n").unwrap();
        fs::write(lib.join("panel.html"), "x").unwrap();
        if helpers {
            fake_bin(&lib.join("bin"), "pulse-popover", "");
            fake_bin(&lib.join("bin"), "pulse-menubar", "");
        }
        lib
    }

    fn arg(s: &str) -> Option<String> {
        Some(s.to_string())
    }

    #[test]
    fn install_on_macos() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("bar-install-mac");
        let (bin, plugins) = fakes(&s);
        let lib = lib_fixture(&s, true);
        let app = s.0.join("SwiftBar.app");
        let mut vars = Vars::default();
        vars.set("PATH", &bin).set("HOME", &s.0).set("PULSE_TEST_SWIFTBAR_APP", &app);
        vars.unset("CODEX_HOME").unset("GROK_HOME").unset("CLAUDE_CONFIG_DIR");
        let _mac = pretend(true);
        // SwiftBar missing and no brew: stop before touching anything
        assert_eq!(install(&lib), 1);
        assert!(!config_dir().join("providers").exists());
        // brew there but the cask does not land, or brew says ok and the app is still missing: stop
        let brew = fake_bin(&bin, "brew", "exit 1");
        assert_eq!(install(&lib), 1);
        assert_eq!(calls(&brew), vec!["install --cask swiftbar"]);
        fake_bin(&bin, "brew", "exit 0");
        assert_eq!(install(&lib), 1);
        // brew installs it: on we go. No login anywhere: an empty providers file, SwiftBar started
        fake_bin(&bin, "brew", "mkdir -p \"$PULSE_TEST_SWIFTBAR_APP\"");
        assert_eq!(install(&lib), 0);
        assert!(app.is_dir());
        assert_eq!(fs::read_to_string(config_dir().join("providers")).unwrap(), "\n");
        let link = plugins.join(PLUGIN);
        assert_eq!(fs::read_link(&link).unwrap(), lib.join(PLUGIN));
        assert_eq!(
            calls(&bin.join("defaults.log")),
            vec!["read com.ameba.SwiftBar PluginDirectory".to_string(), format!("write com.ameba.SwiftBar PluginDirectory {}", plugins.display())]
        );
        assert_eq!(calls(&bin.join("pkill.log")), vec!["-f bin/pulse-popover"]);
        assert_eq!(calls(&bin.join("open.log")), vec!["-a SwiftBar"]);
        assert!(!calls(&bin.join("security.log")).is_empty(), "the Keychain was asked for the login");
        // again: the providers file is kept, the old link name dropped, a running SwiftBar refreshed
        fs::write(config_dir().join("providers"), "codex\n").unwrap();
        fs::File::create(bin.join("running.SwiftBar")).unwrap();
        fs::write(plugins.join(OLD_PLUGIN), "old").unwrap();
        assert_eq!(install(&lib), 0);
        assert!(!plugins.join(OLD_PLUGIN).exists());
        assert_eq!(fs::read_to_string(config_dir().join("providers")).unwrap(), "codex\n");
        assert_eq!(calls(&bin.join("open.log")), vec!["-a SwiftBar", "swiftbar://refreshallplugins"]);
    }

    #[test]
    fn install_builds_the_helpers() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("bar-install-build");
        let (bin, _) = fakes(&s);
        let lib = lib_fixture(&s, false);
        let mut vars = Vars::default();
        vars.set("PATH", &bin).set("HOME", &s.0).set("PULSE_TEST_SWIFTBAR_APP", &s.0);
        vars.unset("CODEX_HOME").unset("GROK_HOME").unset("CLAUDE_CONFIG_DIR");
        let _mac = pretend(true);
        let build = fake_bin(&lib, "build.sh", "exit 1");
        assert_eq!(install(&lib), 1);
        assert_eq!(calls(&build), vec![""]);
        fake_bin(&lib, "build.sh", "");
        // a Codex login on this machine: the providers file starts with it
        fs::create_dir_all(s.0.join(".codex")).unwrap();
        fs::write(s.0.join(".codex").join("auth.json"), "{}").unwrap();
        assert_eq!(install(&lib), 0);
        assert_eq!(fs::read_to_string(config_dir().join("providers")).unwrap(), "codex\n");
        assert_eq!(calls(&build).len(), 2);
        // uninstall: link gone, cache and settings gone
        fs::write(cache_dir().join("usage-codex.json"), "{}").unwrap();
        assert_eq!(uninstall(&lib), 0);
        assert!(!s.0.join("plugins").join(PLUGIN).exists());
        assert!(!cache_dir().exists());
        assert!(!config_dir().exists());
        assert_eq!(calls(&bin.join("open.log")), vec!["-a SwiftBar", "swiftbar://refreshallplugins"]);
    }

    #[test]
    fn on_off_status_on_macos() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("bar-mac");
        let (bin, plugins) = fakes(&s);
        let lib = lib_fixture(&s, true);
        let mut vars = Vars::default();
        vars.set("PATH", &bin).set("HOME", &s.0);
        let _mac = pretend(true);
        // without the test override the real folder is looked at (read only)
        assert_eq!(swiftbar_installed(), Path::new("/Applications/SwiftBar.app").is_dir());
        assert_eq!(bar(&lib, None), 1, "status: nothing linked");
        assert_eq!(bar(&lib, arg("status").as_ref()), 1);
        assert_eq!(bar(&lib, arg("on").as_ref()), 0);
        let link = plugins.join(PLUGIN);
        assert_eq!(fs::read_link(&link).unwrap(), lib.join(PLUGIN));
        assert_eq!(bar(&lib, arg("status").as_ref()), 0, "linked, SwiftBar not running");
        fs::File::create(bin.join("running.SwiftBar")).unwrap();
        assert_eq!(status(&lib), 0, "linked, SwiftBar running");
        // a dangling link is off
        fs::remove_file(lib.join(PLUGIN)).unwrap();
        assert_eq!(status(&lib), 1);
        fs::write(lib.join(PLUGIN), "#!/bin/bash\n").unwrap();
        assert_eq!(status(&lib), 0);
        assert_eq!(bar(&lib, arg("off").as_ref()), 0);
        assert!(!link.exists());
        assert_eq!(status(&lib), 1);
        assert_eq!(bar(&lib, arg("nope").as_ref()), 64);
        assert_eq!(calls(&bin.join("pkill.log")), vec!["-f bin/pulse-popover", "-f bin/pulse-popover"]);
        assert_eq!(calls(&bin.join("open.log")), vec!["-a SwiftBar", "swiftbar://refreshallplugins"]);
        // no answer from defaults, or an empty one: the default plugin folder under HOME
        fake_bin(&bin, "defaults", "exit 1");
        assert_eq!(plugin_dir(), s.0.join(".config").join("swiftbar").join("plugins"));
        fake_bin(&bin, "defaults", "[ \"$1\" = read ] && echo; exit 0");
        assert_eq!(plugin_dir(), s.0.join(".config").join("swiftbar").join("plugins"));
        assert_eq!(on(&lib), 0);
        assert!(s.0.join(".config").join("swiftbar").join("plugins").join(PLUGIN).is_symlink());
        // a file where the plugin folder should be: the link cannot be made
        let blocked = s.0.join("blocked");
        fs::write(&blocked, "x").unwrap();
        fake_bin(&bin, "defaults", &format!("[ \"$1\" = read ] && printf '%s\\n' '{}'; exit 0", blocked.display()));
        assert_eq!(on(&lib), 1);
    }

    #[test]
    fn on_off_status_on_linux() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("bar-linux");
        let (bin, _) = fakes(&s);
        let lib = lib_fixture(&s, false);
        let mut vars = Vars::default();
        vars.set("PATH", &bin).set("HOME", &s.0);
        let _linux = pretend(false);
        assert_eq!(waybar_dir(), s.0.join("config").join("waybar"));
        vars.set("XDG_CONFIG_HOME", "");
        assert_eq!(waybar_dir(), s.0.join(".config").join("waybar"));
        vars.set("XDG_CONFIG_HOME", s.0.join("config"));
        let module = waybar_module();
        assert_eq!(module, s.0.join("config").join("waybar").join("pulse-limits.jsonc"));
        assert_eq!(bar(&lib, None), 1, "status: no module file");
        assert_eq!(bar(&lib, arg("on").as_ref()), 0);
        let text = fs::read_to_string(&module).unwrap();
        let exe = env::current_exe().unwrap().canonicalize().unwrap();
        assert!(text.contains(&format!("\"exec\": \"'{}' waybar\",\n", exe.display())), "{text}");
        assert!(text.contains(&format!("\"on-click\": \"'{}' open\",\n", exe.display())));
        assert!(text.contains("\"custom/pulse-limits\": {\n") && text.contains("\"signal\": 8,\n"));
        // by name when pulse-limits on PATH is this very binary
        std::os::unix::fs::symlink(&exe, bin.join("pulse-limits")).unwrap();
        assert_eq!(on(&lib), 0);
        let text = fs::read_to_string(&module).unwrap();
        assert!(text.contains("\"exec\": \"pulse-limits waybar\",\n") && text.contains("\"on-click\": \"pulse-limits open\",\n"), "{text}");
        assert_eq!(status(&lib), 0, "module there, waybar not running");
        fs::File::create(bin.join("running.waybar")).unwrap();
        assert_eq!(status(&lib), 0);
        refresh();
        assert_eq!(calls(&bin.join("pkill.log")), vec!["-RTMIN+8 waybar"]);
        assert_eq!(bar(&lib, arg("off").as_ref()), 0);
        assert!(!module.exists());
        assert_eq!(status(&lib), 1);
        assert_eq!(uninstall(&lib), 0);
        assert!(!cache_dir().exists() && !config_dir().exists());
        // a file where the waybar folder should be: the module cannot be written
        fs::remove_dir_all(waybar_dir()).unwrap();
        fs::write(waybar_dir(), "x").unwrap();
        assert_eq!(on(&lib), 1);
    }

    #[test]
    fn install_on_linux() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("bar-install-linux");
        let (bin, _) = fakes(&s);
        let lib = lib_fixture(&s, false);
        let mut vars = Vars::default();
        vars.set("PATH", &bin).set("HOME", &s.0);
        vars.unset("CODEX_HOME").unset("GROK_HOME").unset("CLAUDE_CONFIG_DIR");
        let _linux = pretend(false);
        // no waybar, no login: warnings only; the module lands, no provider enabled
        assert_eq!(install(&lib), 0);
        assert!(waybar_module().is_file());
        assert_eq!(fs::read_to_string(config_dir().join("providers")).unwrap(), "\n");
        assert!(calls(&bin.join("security.log")).is_empty(), "Linux never asks the Keychain");
        // waybar on PATH and a Claude login file: enabled from the start
        fake_bin(&bin, "waybar", "");
        fs::remove_file(config_dir().join("providers")).unwrap();
        fs::create_dir_all(s.0.join(".claude")).unwrap();
        fs::write(s.0.join(".claude").join(".credentials.json"), "{\"claudeAiOauth\":{\"accessToken\":\"t\"}}").unwrap();
        fs::create_dir_all(s.0.join(".grok")).unwrap();
        fs::write(s.0.join(".grok").join("auth.json"), "{}").unwrap();
        assert_eq!(install(&lib), 0);
        assert_eq!(fs::read_to_string(config_dir().join("providers")).unwrap(), "grok\nclaude\n");
    }

    #[test]
    fn update_paths() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("bar-update");
        let (bin, _) = fakes(&s);
        let mut vars = Vars::default();
        vars.set("PATH", &bin).set("HOME", &s.0);
        let _mac = pretend(true);
        // Nix and a copied folder: told how, nothing run
        assert_eq!(update(Path::new("/nix/store/abc-pulse-limits/libexec/pulse-limits"), "v0.5.6"), 1);
        let copied = s.0.join("copied");
        fs::create_dir_all(&copied).unwrap();
        assert_eq!(update(&copied, "v0.5.6"), 1);
        assert!(!bin.join("brew.log").exists() && !bin.join("git.log").exists() && !bin.join("pkill.log").exists());
        // Homebrew: brew upgrade, then the new binary relinks and reports its version
        let opt = s.0.join("opt").join("pulse-limits").join("libexec");
        let newbin = fake_bin(&opt.join("bin"), "pulse-limits", "[ \"$1\" = version ] && printf 'v9.9.9\\n'; exit 0");
        let brew = fake_bin(&bin, "brew", "exit 1");
        assert_eq!(update(&opt, "v0.5.6"), 1);
        assert_eq!(calls(&brew), vec!["upgrade pulse-null/tap/pulse-limits"]);
        assert!(!newbin.exists());
        fake_bin(&bin, "brew", "");
        fs::write(cache_dir().join("popover.pid"), "1").unwrap();
        fs::write(cache_dir().join("popover.closed"), "1").unwrap();
        assert_eq!(update(&opt, "v0.5.6"), 0);
        assert!(!cache_dir().join("popover.pid").exists() && !cache_dir().join("popover.closed").exists());
        assert_eq!(calls(&newbin), vec!["install", "version"]);
        assert_eq!(calls(&bin.join("pkill.log")), vec!["-f bin/pulse-popover"]);
        assert_eq!(update(&opt, "v9.9.9"), 0, "already up to date");
        let cellar = s.0.join("Cellar").join("pulse-limits").join("0.5.6").join("libexec");
        fake_bin(&cellar.join("bin"), "pulse-limits", "");
        assert_eq!(update(&cellar, "v0.5.6"), 0);
        // a git checkout: pull, build, relink
        let co = s.0.join("checkout");
        fs::create_dir_all(co.join(".git")).unwrap();
        let git = fake_bin(&bin, "git", "exit 1");
        assert_eq!(update(&co, "v0.5.6"), 1);
        assert_eq!(calls(&git), vec![format!("-C {} pull --ff-only", co.display())]);
        fake_bin(&bin, "git", "");
        let build = fake_bin(&co, "build.sh", "exit 1");
        assert_eq!(update(&co, "v0.5.6"), 1);
        fake_bin(&co, "build.sh", "");
        let cobin = fake_bin(&co.join("bin"), "pulse-limits", "[ \"$1\" = version ] && printf 'v0.5.6\\n'; exit 0");
        assert_eq!(update(&co, "v0.5.6"), 0);
        assert_eq!(calls(&build), vec!["", ""]);
        assert_eq!(calls(&cobin), vec!["install", "version"]);
    }
}
