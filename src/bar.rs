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
            eprintln!("warning: no Claude Code login found ({}). Run 'claude' once and log in; the widget reads that token.", keychain::credential_files()[0].display());
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
