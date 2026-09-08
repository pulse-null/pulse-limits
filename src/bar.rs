//! The bar item alone: the SwiftBar plugin link on macOS; on Linux the Waybar module file,
//! included from the user's config and styled from their stylesheet by `bar on` itself.
//! `install` is the checks plus `bar on`; `uninstall` is `bar off` plus cache and settings.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::keychain;
use crate::util::{cache_dir, command_output, config_dir, home, is_executable, is_macos, process_running, run_quiet, tilde, which, write_atomic};

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
// `bar on` includes it from your config and puts "custom/pulse-limits" in modules-right; the ring
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
        return wire(&module);
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
        let removed = fs::remove_file(&module).is_ok();
        if removed {
            println!("removed {}", module.display());
        }
        let unwired = unwire(&module);
        if removed || unwired {
            reload_waybar();
        }
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
        println!("{}", wiring_status());
        println!("waybar: {}", if process_running("waybar") { "running" } else { "not running" });
    }
    0
}

// ---- Waybar wiring: the user's config and stylesheet, edited as text ---------------------
// No JSON parser: the config is JSONC with comments and trailing commas, and the user's bytes
// stay as they were outside the few insertions, so `bar off` can take exactly those out again.

const MODULE_FILE: &str = "pulse-limits.jsonc";
const MODULE_NAME: &str = "custom/pulse-limits";
const CSS_START: &str = "/* pulse-limits: written by `pulse-limits bar on`, removed by `pulse-limits bar off` */";
const CSS_END: &str = "/* /pulse-limits */";
const CSS_RULES: &str = "#custom-pulse-limits { min-width: 12px; margin: 0 7.5px; }
#custom-pulse-limits.warn  { color: #ffb000; }
#custom-pulse-limits.crit  { color: #ff5c5c; }
#custom-pulse-limits.stale { color: #ffb000; }
#custom-pulse-limits.dead  { color: #8c8c8c; }";

/// The tone rules between their markers, as appended to style.css.
fn css_block() -> String {
    format!("{CSS_START}\n{CSS_RULES}\n{CSS_END}\n")
}

/// `config.jsonc`, else `config`: the first one there, a link counted (Home Manager's are links).
fn waybar_config() -> Option<PathBuf> {
    ["config.jsonc", "config"].iter().map(|n| waybar_dir().join(n)).find(|p| p.symlink_metadata().is_ok())
}

fn waybar_style() -> PathBuf {
    waybar_dir().join("style.css")
}

/// Home Manager generates the config into the store and links it; nothing may edit it there.
fn in_nix_store(target: &Path) -> bool {
    target.starts_with("/nix/store")
}

/// The store path `p` links to, when it does.
fn nix_link(p: &Path) -> Option<PathBuf> {
    fs::read_link(p).ok().filter(|t| in_nix_store(t))
}

fn writable(p: &Path) -> bool {
    fs::OpenOptions::new().append(true).open(p).is_ok()
}

fn backup_path(real: &Path) -> PathBuf {
    PathBuf::from(format!("{}.pulse-limits.bak", real.display()))
}

/// A file `bar on` edits: read and written through any link (a dotfiles checkout keeps its
/// link), backed up once beside the real file, written atomically.
struct Edit {
    shown: PathBuf,
    real: PathBuf,
    text: String,
    exists: bool,
}

impl Edit {
    /// Err when the file is there but must be left alone; `exists` false when it is not there.
    fn open(p: &Path) -> Result<Edit, String> {
        let real = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        let Ok(meta) = fs::metadata(&real) else {
            return Ok(Edit { shown: p.to_path_buf(), real, text: String::new(), exists: false });
        };
        if !meta.is_file() || !writable(&real) {
            return Err(format!("{} is not a writable file", p.display()));
        }
        let text = fs::read_to_string(&real).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
        Ok(Edit { shown: p.to_path_buf(), real, text, exists: true })
    }

    fn backup(&self) -> PathBuf {
        backup_path(&self.real)
    }

    /// Writes `text` after the one-time backup; the line to print.
    fn save(&self, text: &str) -> io::Result<String> {
        if !self.exists {
            write_atomic(&self.real, text.as_bytes())?;
            return Ok(format!("created {}", self.shown.display()));
        }
        let bak = self.backup();
        let note = if bak.exists() {
            String::new()
        } else {
            fs::copy(&self.real, &bak)?;
            format!(" (backup: {})", bak.display())
        };
        write_atomic(&self.real, text.as_bytes())?;
        Ok(format!("edited {}{note}", self.shown.display()))
    }
}

/// Byte offset of the first character outside whitespace and comments.
fn first_significant(text: &str) -> Option<usize> {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
        } else if b[i..].starts_with(b"//") {
            i += text[i..].find('\n').unwrap_or(b.len() - i);
        } else if b[i..].starts_with(b"/*") {
            i += text[i + 2..].find("*/").map_or(b.len() - i, |n| n + 4);
        } else {
            return Some(i);
        }
    }
    None
}

/// Index just past the closing quote of the string opening at `start`.
fn string_end(text: &str, start: usize) -> Option<usize> {
    let b = text.as_bytes();
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// Where the value of the top-level `"key"` starts: past the colon and the blanks after it.
/// Comments, string contents and anything nested are stepped over, not parsed.
fn value_of(text: &str, key: &str) -> Option<usize> {
    let b = text.as_bytes();
    let (mut i, mut depth) = (0, 0);
    while i < b.len() {
        match b[i] {
            b'/' if b[i..].starts_with(b"//") => i += text[i..].find('\n').unwrap_or(b.len() - i),
            b'/' if b[i..].starts_with(b"/*") => i += text[i + 2..].find("*/").map_or(b.len() - i, |n| n + 4),
            b'"' => {
                let end = string_end(text, i)?;
                let colon = end + text[end..].len() - text[end..].trim_start().len();
                if depth == 1 && &text[i + 1..end - 1] == key && b.get(colon) == Some(&b':') {
                    let v = colon + 1;
                    return Some(v + text[v..].len() - text[v..].trim_start().len());
                }
                i = end;
                continue;
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    None
}

/// `text` with `"element"` first in the top-level list `key`; the list is added after the opening
/// brace when the key is missing, and a string value (Waybar allows `"include": "path"`) becomes
/// a list of both. Err when the value is of a shape we do not edit.
fn add_to_list(text: &str, key: &str, element: &str) -> Result<String, String> {
    let mut out = text.to_string();
    match value_of(text, key) {
        Some(v) if text[v..].starts_with('[') => {
            let inner = &text[v + 1..];
            let comma = if inner.trim_start().starts_with(']') { "" } else { "," };
            if let Some(line) = inner.strip_prefix('\n') {
                // one entry per line: ours on its own line, indented like the next
                let indent = &line[..line.len() - line.trim_start_matches([' ', '\t']).len()];
                out.insert_str(v + 1, &format!("\n{indent}\"{element}\"{comma}"));
            } else {
                out.insert_str(v + 1, &format!("\"{element}\"{comma}{}", if comma.is_empty() { "" } else { " " }));
            }
        }
        Some(v) if text[v..].starts_with('"') => {
            let end = string_end(text, v).ok_or_else(|| format!("\"{key}\" is an unterminated string"))?;
            out.replace_range(v..end, &format!("[\"{element}\", {}]", &text[v..end]));
        }
        Some(_) => return Err(format!("\"{key}\" is neither a list nor a string")),
        None => {
            let open = first_significant(text).unwrap_or(0);
            out.insert_str(open + 1, &format!("\n  \"{key}\": [\"{element}\"],"));
        }
    }
    Ok(out)
}

/// `text` without the list element `"element"`, wherever it sits: with the comma after it (and
/// the space we put there), or the comma before it when it is last, or alone leaving `[]`; on
/// a line of its own the whole line goes. None when it is not a list element anywhere.
fn drop_element(text: &str, element: &str) -> Option<String> {
    let q = format!("\"{element}\"");
    let p = text
        .match_indices(&q)
        .map(|(p, _)| p)
        .find(|&p| text[..p].trim_end().ends_with(['[', ',']) && text[p + q.len()..].trim_start().starts_with([',', ']']))?;
    let (before, after) = (&text[..p], &text[p + q.len()..]);
    let line_start = before.trim_end_matches([' ', '\t']);
    let rest = after.trim_start_matches([',', ' ', '\t']);
    if line_start.ends_with('\n') && rest.starts_with('\n') {
        return Some(format!("{line_start}{}", &rest[1..]));
    }
    if let Some(rest) = after.strip_prefix(',') {
        return Some(format!("{before}{}", rest.strip_prefix(' ').unwrap_or(rest)));
    }
    let kept = before.trim_end();
    Some(format!("{}{after}", kept.strip_suffix(',').map_or(before, str::trim_end)))
}

/// The config with the module included and first in modules-right. Ok(None): it already is.
/// Err: not one bar object (Waybar also takes a list of bars), or a value we do not edit.
fn wire_config(text: &str, entry: &str) -> Result<Option<String>, String> {
    match first_significant(text) {
        Some(i) if text[i..].starts_with('{') => {}
        Some(_) => return Err("it is not one bar object (a list of bars?)".into()),
        None => return Err("it is empty".into()),
    }
    let mut out = text.to_string();
    // modules-right first: when both keys are missing the include line then lands on top
    if !out.contains(&format!("\"{MODULE_NAME}\"")) {
        out = add_to_list(&out, "modules-right", MODULE_NAME)?;
    }
    if !out.contains(MODULE_FILE) {
        out = add_to_list(&out, "include", entry)?;
    }
    Ok((out != text).then_some(out))
}

/// The config without what `wire_config` put in: a whole line when it wrote one, else the
/// element. None when nothing of ours is there.
fn unwire_config(text: &str, entry: &str) -> Option<String> {
    let mut out = text.to_string();
    let mut found = false;
    for (key, element) in [("include", entry), ("modules-right", MODULE_NAME)] {
        let line = format!("\n  \"{key}\": [\"{element}\"],");
        let next = match out.find(&line) {
            Some(p) => Some(format!("{}{}", &out[..p], &out[p + line.len()..])),
            None => drop_element(&out, element),
        };
        if let Some(t) = next {
            out = t;
            found = true;
        }
    }
    found.then_some(out)
}

/// style.css with the tone block on the end, after one blank line. None: already there.
fn wire_css(text: &str) -> Option<String> {
    if text.contains(CSS_START) {
        return None;
    }
    let mut out = text.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() && !out.ends_with("\n\n") {
        out.push('\n');
    }
    out.push_str(&css_block());
    Some(out)
}

/// style.css without the block, markers included, and the blank line before it. None: not there.
fn unwire_css(text: &str) -> Option<String> {
    let start = text.find(CSS_START)?;
    let mut end = text[start..].find(CSS_END)? + start + CSS_END.len();
    if text[end..].starts_with('\n') {
        end += 1;
    }
    let start = if text[..start].ends_with("\n\n") { start - 1 } else { start };
    Some(format!("{}{}", &text[..start], &text[end..]))
}

fn print_by_hand(entry: &str) {
    println!("Add these two lines to your Waybar config ({}/config.jsonc):", waybar_dir().display());
    println!("  \"include\": [\"{entry}\"],");
    println!("  \"modules-right\": [..., \"{MODULE_NAME}\"],");
    println!("and the tones to style.css (the class follows the session window):");
    for rule in CSS_RULES.lines() {
        println!("  {rule}");
    }
    println!("Then reload Waybar (pkill -SIGUSR2 waybar; on Omarchy: omarchy-restart-waybar).");
}

fn print_home_manager() {
    println!("On NixOS, declare it with the Home Manager module instead:");
    println!("  inputs.pulse-limits.url = \"github:pulse-null/pulse-limits\";");
    println!("  imports = [ inputs.pulse-limits.homeManagerModules.default ];");
    println!("  programs.pulse-limits = {{ enable = true; waybar.enable = true; }};");
}

/// Waybar re-reads its config and stylesheet on SIGUSR2.
fn reload_waybar() {
    if process_running("waybar") {
        run_quiet("pkill", &["-SIGUSR2", "waybar"]);
        println!("reloaded Waybar");
    } else {
        println!("Waybar is not running");
    }
}

/// Puts the module into the user's config and stylesheet, backups first, and reloads Waybar.
/// A file Home Manager generates, or one we cannot write, is left alone with the fix printed.
fn wire(module: &Path) -> i32 {
    let entry = tilde(module);
    let Some(cfg_path) = waybar_config() else {
        println!("no Waybar config in {}; when you make one:", waybar_dir().display());
        print_by_hand(&entry);
        return 0;
    };
    let css_path = waybar_style();
    for p in [&cfg_path, &css_path] {
        if let Some(target) = nix_link(p) {
            println!("{} -> {}: Home Manager's, not edited", p.display(), target.display());
            print_home_manager();
            return 0;
        }
    }
    let (cfg, css) = match (Edit::open(&cfg_path), Edit::open(&css_path)) {
        (Ok(cfg), Ok(css)) => (cfg, css),
        (Err(why), _) | (_, Err(why)) => {
            println!("{why}: not edited");
            print_home_manager();
            return 0;
        }
    };
    if !cfg.exists {
        println!("{} points nowhere; when you make a config:", cfg_path.display());
        print_by_hand(&entry);
        return 0;
    }
    let new_cfg = match wire_config(&cfg.text, &entry) {
        Ok(new) => new,
        Err(why) => {
            println!("not editing {}: {why}", cfg_path.display());
            print_by_hand(&entry);
            return 0;
        }
    };
    let new_css = wire_css(&css.text);
    if new_cfg.is_none() && new_css.is_none() {
        println!("already wired: {} includes the module, {} has the tones", cfg_path.display(), css_path.display());
    }
    for (edit, new) in [(&cfg, new_cfg), (&css, new_css)] {
        if let Some(text) = new {
            match edit.save(&text) {
                Ok(line) => println!("{line}"),
                Err(e) => {
                    eprintln!("cannot write {}: {e}", edit.shown.display());
                    return 1;
                }
            }
        }
    }
    reload_waybar();
    0
}

/// Takes out of the config and stylesheet exactly what `wire` put in, then the backups.
/// Whether anything changed.
fn unwire(module: &Path) -> bool {
    let entry = tilde(module);
    let mut changed = false;
    for (p, is_css) in [(waybar_config(), false), (Some(waybar_style()), true)] {
        let Some(p) = p else { continue };
        if let Some(target) = nix_link(&p) {
            println!("{} -> {}: Home Manager's, left alone", p.display(), target.display());
            continue;
        }
        let edit = match Edit::open(&p) {
            Ok(e) if e.exists => e,
            Ok(_) => continue,
            Err(why) => {
                println!("{why}: left alone");
                continue;
            }
        };
        let undone = if is_css { unwire_css(&edit.text) } else { unwire_config(&edit.text, &entry) };
        if let Some(text) = undone {
            // a stylesheet `bar on` made from nothing (so never backed up) goes away again
            let done = if text.is_empty() && !edit.backup().exists() { fs::remove_file(&edit.real) } else { write_atomic(&edit.real, text.as_bytes()) };
            match done {
                Ok(()) => {
                    println!("unwired {}", p.display());
                    changed = true;
                }
                Err(e) => eprintln!("cannot write {}: {e}", p.display()),
            }
        }
        if fs::remove_file(edit.backup()).is_ok() {
            println!("removed {}", edit.backup().display());
        }
    }
    if !changed {
        println!("nothing of ours in {}", waybar_dir().display());
    }
    changed
}

/// One line for `bar status`: the config includes the module and the stylesheet has the
/// tones, or not yet, or Home Manager owns the config.
fn wiring_status() -> String {
    let css = waybar_style();
    let Some(cfg) = waybar_config() else {
        return format!("config: none in {}", waybar_dir().display());
    };
    if let Some(target) = nix_link(&cfg).or_else(|| nix_link(&css)) {
        return format!("config: {} is Nix-managed ({})", cfg.display(), target.display());
    }
    let has = |p: &Path, needle: &str| fs::read_to_string(p).map(|t| t.contains(needle)).unwrap_or(false);
    let included = has(&cfg, MODULE_FILE) && has(&cfg, &format!("\"{MODULE_NAME}\""));
    format!(
        "config: {} {} the module; {} {} the tones",
        cfg.display(),
        if included { "includes" } else { "does not include" },
        css.display(),
        if has(&css, CSS_START) { "has" } else { "lacks" }
    )
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
    use crate::util::testing::{calls, fake_bin, pretend, Os, Vars};
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::sync::MutexGuard;

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
        assert!(!waybar_style().exists(), "no config to wire: the two lines are printed, no stylesheet is made");
        assert_eq!(wiring_status(), format!("config: none in {}", waybar_dir().display()));
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

    /// The Linux side for one test: the fakes on PATH, HOME and the Waybar folder in the
    /// scratch, the pretend OS. Fields drop in order: the scratch first, the lock last.
    struct Linux {
        s: Scratch,
        bin: PathBuf,
        lib: PathBuf,
        _vars: Vars,
        _os: Os,
        _g: MutexGuard<'static, ()>,
    }

    fn linux(tag: &str) -> Linux {
        let g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new(tag);
        let (bin, _) = fakes(&s);
        let lib = lib_fixture(&s, false);
        let mut vars = Vars::default();
        vars.set("PATH", &bin).set("HOME", &s.0);
        fs::create_dir_all(waybar_dir()).unwrap();
        Linux { s, bin, lib, _vars: vars, _os: pretend(false), _g: g }
    }

    /// Omarchy's layout: comments, one entry per line, trailing commas, both keys.
    const OMARCHY: &str = r#"/* Omarchy's Waybar: it reads JSONC, so comments and trailing commas are fine */
{
  // "include": ["the old layout, kept as a note"],
  "include": [
    "~/.config/waybar/omarchy.jsonc",
  ],
  "reload_style_on_change": true,
  "layer": "top",
  "position": "top",
  "spacing": 0,
  "height": 26,
  "modules-left": ["custom/omarchy", "hyprland/workspaces"],
  "modules-center": ["clock", "custom/update", "custom/screenrecording-indicator"],
  "modules-right": [
    "group/tray-expander",
    "bluetooth", // BlueZ
    "network",
    "pulseaudio",
    "cpu",
    "battery",
  ],
  "hyprland/workspaces": {
    "on-click": "activate",
    "format": "{icon}",
    "format-icons": { "default": "", "active": "󱓻" },
    "persistent-workspaces": { "1": [], "2": [], "3": [], "4": [], "5": [] },
  },
  "clock": {
    "format": "{:%A %H:%M}",
    "format-alt": "{:%d %B W%V %Y}",
    "tooltip": false,
  },
  "battery": {
    "format": "{capacity}% {icon}",
    "format-icons": { "charging": "󰂄", "default": ["󰁺", "󰁻", "󰁼", "󰁽", "󰁾", "󰁿", "󰂀", "󰂁", "󰂂", "󰁹"] },
    "interval": 5,
    "states": { "warning": 20, "critical": 10 },
  },
}
"#;

    const OMARCHY_CSS: &str = r#"@import "../omarchy/current/theme/waybar.css";

* {
  background-color: @background;
  color: @foreground;
  border: none;
  font-family: CaskaydiaMono Nerd Font Propo;
  font-size: 12px;
}

#custom-omarchy { padding: 0 0 0 8px; }
#battery.warning:not(.charging) { color: #ffb000; } /* one of our colours without our marker: stays */
"#;

    #[test]
    fn wires_omarchy_config() {
        let t = linux("bar-wire-omarchy");
        let (cfg, css) = (waybar_dir().join("config.jsonc"), waybar_style());
        fs::write(&cfg, OMARCHY).unwrap();
        fs::write(&css, OMARCHY_CSS).unwrap();
        fs::File::create(t.bin.join("running.waybar")).unwrap();
        assert_eq!(on(&t.lib), 0);
        let entry = tilde(&waybar_module());
        assert_eq!(entry, "~/config/waybar/pulse-limits.jsonc");
        let wired = fs::read_to_string(&cfg).unwrap();
        let (inc, right) = (format!("\n    \"{entry}\","), "\n    \"custom/pulse-limits\",");
        assert!(wired.contains(&format!("\"include\": [{inc}\n    \"~/.config/waybar/omarchy.jsonc\",\n  ],")), "{wired}");
        assert!(wired.contains(&format!("\"modules-right\": [{right}\n    \"group/tray-expander\",")), "{wired}");
        assert_eq!(wired.replacen(&inc, "", 1).replacen(right, "", 1), OMARCHY, "nothing else moved");
        let styled = format!("{OMARCHY_CSS}\n{}", css_block());
        assert_eq!(fs::read_to_string(&css).unwrap(), styled);
        let (cfg_bak, css_bak) = (backup_path(&cfg), backup_path(&css));
        assert_eq!(fs::read_to_string(&cfg_bak).unwrap(), OMARCHY);
        assert_eq!(fs::read_to_string(&css_bak).unwrap(), OMARCHY_CSS);
        let pkill = t.bin.join("pkill.log");
        assert_eq!(calls(&pkill), vec!["-SIGUSR2 waybar"]);
        assert_eq!(wiring_status(), format!("config: {} includes the module; {} has the tones", cfg.display(), css.display()));
        assert_eq!(status(&t.lib), 0);
        // again: nothing moves, the backups still hold the originals
        assert_eq!(on(&t.lib), 0);
        assert_eq!(fs::read_to_string(&cfg).unwrap(), wired);
        assert_eq!(fs::read_to_string(&css).unwrap(), styled);
        assert_eq!(fs::read_to_string(&cfg_bak).unwrap(), OMARCHY);
        assert_eq!(calls(&pkill), vec!["-SIGUSR2 waybar", "-SIGUSR2 waybar"]);
        // undone by hand, then on again: wired again, the first backup kept
        fs::write(&cfg, OMARCHY).unwrap();
        fs::write(&cfg_bak, "older").unwrap();
        assert_eq!(on(&t.lib), 0);
        assert_eq!(fs::read_to_string(&cfg).unwrap(), wired);
        assert_eq!(fs::read_to_string(&cfg_bak).unwrap(), "older");
        // off: both files byte for byte as they were, the backups and the module gone
        assert_eq!(off(&t.lib), 0);
        assert_eq!(fs::read_to_string(&cfg).unwrap(), OMARCHY);
        assert_eq!(fs::read_to_string(&css).unwrap(), OMARCHY_CSS);
        assert!(!cfg_bak.exists() && !css_bak.exists() && !waybar_module().exists());
        assert_eq!(calls(&pkill).len(), 4);
        assert_eq!(wiring_status(), format!("config: {} does not include the module; {} lacks the tones", cfg.display(), css.display()));
        // off again: nothing of ours, nothing to reload
        assert_eq!(off(&t.lib), 0);
        assert_eq!(calls(&pkill).len(), 4);
        assert_eq!(fs::read_to_string(&cfg).unwrap(), OMARCHY);
        // Waybar not running: no signal
        fs::remove_file(t.bin.join("running.waybar")).unwrap();
        assert_eq!(on(&t.lib), 0);
        assert_eq!(off(&t.lib), 0);
        assert_eq!(calls(&pkill).len(), 4);
        assert_eq!(fs::read_to_string(&cfg).unwrap(), OMARCHY);
    }

    #[test]
    fn wires_other_config_shapes() {
        let t = linux("bar-wire-shapes");
        let (cfg, css) = (waybar_dir().join("config"), waybar_style()); // the bare name, and no stylesheet yet
        let entry = tilde(&waybar_module());
        let e = format!("\"{entry}\"");
        // on, then off, from a given config: what it becomes, and what comes back
        let round = |before: &str, after: String, back: &str| {
            fs::write(&cfg, before).unwrap();
            assert_eq!(on(&t.lib), 0);
            assert_eq!(fs::read_to_string(&cfg).unwrap(), after, "on: {before}");
            assert_eq!(fs::read_to_string(backup_path(&cfg)).unwrap(), before);
            assert_eq!(fs::read_to_string(&css).unwrap(), css_block(), "made from nothing");
            assert_eq!(off(&t.lib), 0);
            assert_eq!(fs::read_to_string(&cfg).unwrap(), back, "off: {before}");
            assert!(!backup_path(&cfg).exists() && !css.exists(), "the made stylesheet goes away again");
        };
        // one line, both keys: first in each list
        round(
            r#"{"include": ["a"], "modules-right": ["clock"]}"#,
            format!(r#"{{"include": [{e}, "a"], "modules-right": ["custom/pulse-limits", "clock"]}}"#),
            r#"{"include": ["a"], "modules-right": ["clock"]}"#,
        );
        // a string include becomes a list of both; off leaves the list, which Waybar reads the same
        round(
            r#"{"include": "one", "modules-right": []}"#,
            format!(r#"{{"include": [{e}, "one"], "modules-right": ["custom/pulse-limits"]}}"#),
            r#"{"include": ["one"], "modules-right": []}"#,
        );
        // neither key: two lines after the brace, whole lines gone again
        round(
            "{\n  \"layer\": \"top\"\n}\n",
            format!("{{\n  \"include\": [{e}],\n  \"modules-right\": [\"custom/pulse-limits\"],\n  \"layer\": \"top\"\n}}\n"),
            "{\n  \"layer\": \"top\"\n}\n",
        );
        // the module listed by hand elsewhere: only the include is added; off takes the module out wherever it is
        round(
            "{\n  \"modules-left\": [\"custom/pulse-limits\"],\n}\n",
            format!("{{\n  \"include\": [{e}],\n  \"modules-left\": [\"custom/pulse-limits\"],\n}}\n"),
            "{\n  \"modules-left\": [],\n}\n",
        );
        // an empty multi-line list, and a list with one entry per line
        round(
            "{\n  \"include\": [\n  ],\n  \"modules-right\": [\n    \"cpu\"\n  ]\n}\n",
            format!("{{\n  \"include\": [\n  {e}\n  ],\n  \"modules-right\": [\n    \"custom/pulse-limits\",\n    \"cpu\"\n  ]\n}}\n"),
            "{\n  \"include\": [\n  ],\n  \"modules-right\": [\n    \"cpu\"\n  ]\n}\n",
        );
        // a list of bars, nothing at all, or a value we do not edit: told what to add, nothing touched
        for text in ["[\n  { \"layer\": \"top\" }\n]\n", "", "// only a comment\n", "{\"include\": 5}\n", "{\"include\": \"open\n"] {
            fs::write(&cfg, text).unwrap();
            assert_eq!(on(&t.lib), 0);
            assert_eq!(fs::read_to_string(&cfg).unwrap(), text);
            assert!(!backup_path(&cfg).exists() && !css.exists(), "{text:?}");
            assert_eq!(off(&t.lib), 0);
            assert_eq!(fs::read_to_string(&cfg).unwrap(), text);
        }
        // a stylesheet without a final newline: it gets one, then the blank line, then the block
        fs::write(&css, "* { color: red; }").unwrap();
        fs::write(&cfg, "{\n}\n").unwrap();
        assert_eq!(on(&t.lib), 0);
        assert_eq!(fs::read_to_string(&css).unwrap(), format!("* {{ color: red; }}\n\n{}", css_block()));
        assert_eq!(fs::read_to_string(backup_path(&css)).unwrap(), "* { color: red; }");
        assert_eq!(off(&t.lib), 0);
        assert_eq!(fs::read_to_string(&css).unwrap(), "* { color: red; }\n", "the newline we added stays: it cannot be told from theirs");
        assert!(!backup_path(&css).exists());
        // an empty stylesheet that was there before is kept, empty: it had a backup
        fs::write(&css, "").unwrap();
        assert_eq!(on(&t.lib), 0);
        assert_eq!(fs::read_to_string(&css).unwrap(), css_block());
        assert_eq!(off(&t.lib), 0);
        assert_eq!(fs::read_to_string(&css).unwrap(), "");
    }

    #[test]
    fn nix_and_readonly_files_are_left_alone() {
        let t = linux("bar-wire-nix");
        assert!(in_nix_store(Path::new("/nix/store/abc-home-manager-files/.config/waybar/config")));
        assert!(!in_nix_store(&t.s.0.join("nix").join("store").join("abc")));
        let (cfg, css) = (waybar_dir().join("config.jsonc"), waybar_style());
        let entry = tilde(&waybar_module());
        // Home Manager's link into the store (dangling here: it is refused before it is read)
        let store = "/nix/store/abc-home-manager-files/.config/waybar/config.jsonc";
        symlink(store, &cfg).unwrap();
        fs::write(&css, "* {}\n").unwrap();
        assert_eq!(on(&t.lib), 0);
        assert!(waybar_module().is_file(), "the module file is still written");
        assert_eq!(fs::read_to_string(&css).unwrap(), "* {}\n");
        assert!(!backup_path(&css).exists());
        assert_eq!(wiring_status(), format!("config: {} is Nix-managed ({store})", cfg.display()));
        assert_eq!(status(&t.lib), 0);
        assert_eq!(off(&t.lib), 0);
        assert_eq!(fs::read_link(&cfg).unwrap(), Path::new(store));
        assert!(!waybar_module().exists());
        // the stylesheet in the store, the config not: refused the same way
        fs::remove_file(&cfg).unwrap();
        fs::write(&cfg, "{\n}\n").unwrap();
        fs::remove_file(&css).unwrap();
        symlink("/nix/store/abc-home-manager-files/.config/waybar/style.css", &css).unwrap();
        assert_eq!(on(&t.lib), 0);
        assert_eq!(fs::read_to_string(&cfg).unwrap(), "{\n}\n");
        assert_eq!(off(&t.lib), 0);
        assert!(css.is_symlink());
        fs::remove_file(&css).unwrap();
        // a link that points nowhere, or a file that is not text: nothing to edit, said so
        fs::remove_file(&cfg).unwrap();
        symlink(t.s.0.join("gone"), &cfg).unwrap();
        assert_eq!(on(&t.lib), 0);
        assert!(!css.exists());
        assert_eq!(off(&t.lib), 0);
        fs::remove_file(&cfg).unwrap();
        fs::write(&cfg, [0xff, 0xfe]).unwrap();
        assert_eq!(on(&t.lib), 0);
        assert_eq!(fs::read(&cfg).unwrap(), [0xff, 0xfe]);
        assert_eq!(off(&t.lib), 0);
        // a link elsewhere (a dotfiles checkout) is followed: the target edited, the link kept
        let dots = t.s.0.join("dotfiles");
        let real = dots.join("config.jsonc");
        fs::create_dir_all(&dots).unwrap();
        fs::write(&real, "{\n}\n").unwrap();
        fs::remove_file(&cfg).unwrap();
        symlink(&real, &cfg).unwrap();
        let wired = format!("{{\n  \"include\": [\"{entry}\"],\n  \"modules-right\": [\"custom/pulse-limits\"],\n}}\n");
        assert_eq!(on(&t.lib), 0);
        assert!(cfg.is_symlink());
        assert_eq!(fs::read_to_string(&real).unwrap(), wired);
        assert!(backup_path(&real).exists() && !backup_path(&cfg).exists(), "the backup sits by the real file");
        assert_eq!(off(&t.lib), 0);
        assert_eq!(fs::read_to_string(&real).unwrap(), "{\n}\n");
        assert!(!backup_path(&real).exists());
        // read-only config: refused, nothing written. Read-only stylesheet: the config is not touched either
        let (ro, rw) = (fs::Permissions::from_mode(0o444), fs::Permissions::from_mode(0o644));
        fs::set_permissions(&real, ro.clone()).unwrap();
        assert_eq!(on(&t.lib), 0);
        assert_eq!(fs::read_to_string(&real).unwrap(), "{\n}\n");
        assert!(!backup_path(&real).exists() && !css.exists());
        assert_eq!(off(&t.lib), 0, "off says so too");
        fs::set_permissions(&real, rw.clone()).unwrap();
        fs::write(&css, "* {}\n").unwrap();
        fs::set_permissions(&css, ro.clone()).unwrap();
        assert_eq!(on(&t.lib), 0);
        assert_eq!(fs::read_to_string(&real).unwrap(), "{\n}\n");
        assert_eq!(fs::read_to_string(&css).unwrap(), "* {}\n");
        fs::set_permissions(&css, rw.clone()).unwrap();
        // wired, then the stylesheet made read-only: off unwires the config, leaves the stylesheet and its backup
        assert_eq!(on(&t.lib), 0);
        fs::set_permissions(&css, ro).unwrap();
        assert_eq!(off(&t.lib), 0);
        assert_eq!(fs::read_to_string(&real).unwrap(), "{\n}\n");
        assert_eq!(fs::read_to_string(&css).unwrap(), format!("* {{}}\n\n{}", css_block()));
        assert!(backup_path(&css).exists() && !backup_path(&real).exists());
        fs::set_permissions(&css, rw).unwrap();
        assert_eq!(off(&t.lib), 0);
        assert_eq!(fs::read_to_string(&css).unwrap(), "* {}\n");
        assert!(!backup_path(&css).exists());
        // a folder that takes no new files: no backup can be made, so nothing is written
        fs::set_permissions(&dots, fs::Permissions::from_mode(0o555)).unwrap();
        assert_eq!(on(&t.lib), 1);
        assert_eq!(fs::read_to_string(&real).unwrap(), "{\n}\n");
        assert_eq!(fs::read_to_string(&css).unwrap(), "* {}\n");
        fs::set_permissions(&dots, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(on(&t.lib), 0);
        assert_eq!(fs::read_to_string(&real).unwrap(), wired);
        fs::set_permissions(&dots, fs::Permissions::from_mode(0o555)).unwrap();
        assert_eq!(off(&t.lib), 0, "the config cannot be written back: said, the stylesheet still unwired");
        assert_eq!(fs::read_to_string(&real).unwrap(), wired);
        assert_eq!(fs::read_to_string(&css).unwrap(), "* {}\n");
        fs::set_permissions(&dots, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(off(&t.lib), 0);
        assert_eq!(fs::read_to_string(&real).unwrap(), "{\n}\n");
        assert!(!backup_path(&real).exists());
    }

    #[test]
    fn config_text_edits() {
        assert_eq!(first_significant("  \n\t{"), Some(4));
        assert_eq!(first_significant("// c\n/* c\n c */ [1]"), Some(16));
        assert_eq!(first_significant("/* never closed"), None);
        assert_eq!(first_significant("// only\n"), None);
        assert_eq!(first_significant(""), None);
        assert_eq!(string_end("\"a\\\"b\" x", 0), Some(6));
        assert_eq!(string_end("\"open", 0), None);
        assert_eq!(string_end("\"tail\\", 0), None);
        // value_of: top-level keys only, comments and string contents stepped over
        let t = "{ \"a\": {\"include\": 1}, // \"include\": 2\n \"x\": \"\\\"include\\\": 3\", \"include\" : [ ] }";
        assert_eq!(value_of(t, "include").map(|v| &t[v..v + 3]), Some("[ ]"));
        assert_eq!(value_of("{\"include\"}", "include"), None, "no colon");
        assert_eq!(value_of("{\"include", "include"), None, "unterminated");
        assert_eq!(value_of("[{\"include\": []}]", "include"), None, "nested in a list of bars");
        assert_eq!(value_of("{/* \"include\": */ \"y\": 1}", "include"), None);
        // add_to_list: first in a list, a string made a list, a line when the key is missing
        let e = "~/x.jsonc";
        for (before, after) in [
            ("{\"include\": []}", "{\"include\": [\"~/x.jsonc\"]}"),
            ("{\"include\": [ ]}", "{\"include\": [\"~/x.jsonc\" ]}"),
            ("{\"include\": [\"a\"]}", "{\"include\": [\"~/x.jsonc\", \"a\"]}"),
            ("{\"include\":[\"a\",\"b\"]}", "{\"include\":[\"~/x.jsonc\", \"a\",\"b\"]}"),
            ("{\"include\": \"a\"}", "{\"include\": [\"~/x.jsonc\", \"a\"]}"),
            ("{\"include\": [\n\t\"a\"\n]}", "{\"include\": [\n\t\"~/x.jsonc\",\n\t\"a\"\n]}"),
            ("{}", "{\n  \"include\": [\"~/x.jsonc\"],}"),
        ] {
            assert_eq!(add_to_list(before, "include", e).unwrap(), after, "{before}");
        }
        assert_eq!(add_to_list("{\"include\": 5}", "include", e), Err("\"include\" is neither a list nor a string".into()));
        assert_eq!(add_to_list("{\"include\": \"open", "include", e), Err("\"include\" is an unterminated string".into()));
        // drop_element: first, middle, last, alone, on its own line anywhere; never a key
        for (text, after) in [
            ("[\"x\", \"a\"]", "[\"a\"]"),
            ("[\"x\",\"a\"]", "[\"a\"]"),
            ("[\"a\", \"x\", \"b\"]", "[\"a\", \"b\"]"),
            ("[\"a\", \"x\"]", "[\"a\"]"),
            ("[\"a\",\"x\"]", "[\"a\"]"),
            ("[\"x\"]", "[]"),
            ("[ \"x\" ]", "[  ]"),
            ("[\n  \"a\",\n  \"x\",\n  \"b\"\n]", "[\n  \"a\",\n  \"b\"\n]"),
            ("[\n  \"a\",\n  \"x\"\n]", "[\n  \"a\",\n]"),
            ("[\n  \"a\",\n  \"x\",\n]", "[\n  \"a\",\n]"),
            ("[\n  \"x\"\n]", "[\n]"),
            ("{\"x\": {}, \"m\": [\"x\"]}", "{\"x\": {}, \"m\": []}"),
        ] {
            assert_eq!(drop_element(text, "x").as_deref(), Some(after), "{text:?}");
        }
        assert_eq!(drop_element("{\"x\": {}}", "x"), None, "a key, not an element");
        assert_eq!(drop_element("\"x\"", "x"), None);
        assert_eq!(drop_element("[\"y\"]", "x"), None);
        // the two together, and the stylesheet
        let m = "~/w/pulse-limits.jsonc";
        assert_eq!(wire_config("[]", m), Err("it is not one bar object (a list of bars?)".into()));
        assert_eq!(wire_config(" // x\n", m), Err("it is empty".into()));
        let both = "{\"include\": [\"~/w/pulse-limits.jsonc\"], \"modules-right\": [\"custom/pulse-limits\"]}";
        assert_eq!(wire_config(both, m), Ok(None));
        assert_eq!(unwire_config("{}", m), None);
        assert_eq!(unwire_config(both, m).as_deref(), Some("{\"include\": [], \"modules-right\": []}"));
        assert_eq!(wire_css(""), Some(css_block()));
        assert_eq!(wire_css("a{}"), Some(format!("a{{}}\n\n{}", css_block())));
        assert_eq!(wire_css("a{}\n\n\n"), Some(format!("a{{}}\n\n\n{}", css_block())));
        assert_eq!(wire_css(&css_block()), None);
        assert_eq!(unwire_css(&css_block()).as_deref(), Some(""));
        assert_eq!(unwire_css(&format!("a{{}}\n\n{}b{{}}\n", css_block())).as_deref(), Some("a{}\nb{}\n"));
        assert_eq!(unwire_css(&format!("a{{}}\n{}", css_block().trim_end())).as_deref(), Some("a{}\n"));
        assert_eq!(unwire_css("a{}"), None);
        assert_eq!(unwire_css(CSS_START), None, "no end marker: left for a human");
    }
}
