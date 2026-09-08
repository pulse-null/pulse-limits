//! Small shared helpers: clock, ISO-8601, the formats panel.html uses, XDG folders,
//! the lib folder (panel.html, shims, helpers) and atomic file writes.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

pub const THEMES: [&str; 5] = ["crt", "modern", "cyber", "synth", "analog"];

pub fn now() -> i64 {
    now_f() as i64
}

pub fn now_f() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// ISO-8601 ("2026-09-08T13:40:00.125007+00:00", "...Z", or no zone = UTC) -> epoch seconds
/// with the fraction. std has no date parsing.
pub fn epoch_of_f(ts: &str) -> Option<f64> {
    let b = ts.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |a: usize, z: usize| ts.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, s) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let mut rest = &ts[19..];
    let mut frac = 0.0;
    if let Some(r) = rest.strip_prefix('.') {
        let digits: String = r.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            return None;
        }
        let nanos: String = format!("{digits:0<9}").chars().take(9).collect();
        frac = nanos.parse::<f64>().ok()? / 1e9;
        rest = &r[digits.len()..];
    }
    let offset = match rest {
        "" | "Z" => 0,
        tz if tz.starts_with('+') || tz.starts_with('-') => {
            let digits: String = tz[1..].chars().filter(char::is_ascii_digit).collect();
            if digits.len() != 4 || tz[1..].chars().any(|c| !c.is_ascii_digit() && c != ':') {
                return None;
            }
            let sign = if tz.starts_with('+') { 1 } else { -1 };
            sign * (digits[..2].parse::<i64>().ok()? * 3600 + digits[2..].parse::<i64>().ok()? * 60)
        }
        _ => return None,
    };
    Some((days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + s - offset) as f64 + frac)
}

/// ISO-8601 -> whole epoch seconds, as the bash `epoch_of` read reset times.
pub fn epoch_of(ts: &str) -> Option<i64> {
    epoch_of_f(ts).map(|t| t.floor() as i64)
}

/// Howard Hinnant's algorithm: proleptic Gregorian date -> days since 1970-01-01.
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The inverse: days since 1970-01-01 -> (year, month, day).
pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// epoch -> "2026-09-08T13:35:06Z", as jq's `todate` writes it.
pub fn iso_utc(epoch: i64) -> String {
    let (y, m, d) = civil_from_days(epoch.div_euclid(86400));
    let s = epoch.rem_euclid(86400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3600, s % 3600 / 60, s % 60)
}

/// epoch -> "2026-09-08T13:41", the ISO minute prefix the transcript scan looks for.
pub fn iso_minute(epoch: i64) -> String {
    iso_utc(epoch)[..16].to_string()
}

/// Local UTC offset in seconds, from `date +%z`: std knows nothing about time zones.
pub fn local_offset() -> i64 {
    let s = command_output("date", &["+%z"]).unwrap_or_default();
    let digits: String = s.chars().filter(char::is_ascii_digit).collect();
    if digits.len() != 4 {
        return 0;
    }
    let sign = if s.starts_with('-') { -1 } else { 1 };
    sign * (digits[..2].parse::<i64>().unwrap_or(0) * 3600 + digits[2..].parse::<i64>().unwrap_or(0) * 60)
}

pub fn hhmm(epoch: i64, offset: i64) -> String {
    let t = epoch + offset;
    format!("{:02}:{:02}", t.rem_euclid(86400) / 3600, t.rem_euclid(3600) / 60)
}

pub fn hhmmss(epoch: i64, offset: i64) -> String {
    format!("{}:{:02}", hhmm(epoch, offset), (epoch + offset).rem_euclid(60))
}

/// "2026-09-08 16:07" in local time, for the doctor's header.
pub fn local_stamp(epoch: i64, offset: i64) -> String {
    let t = epoch + offset;
    let (y, m, d) = civil_from_days(t.div_euclid(86400));
    format!("{y:04}-{m:02}-{d:02} {}", hhmm(epoch, offset))
}

/// seconds until an epoch -> "2H 14M" / "4D 07H" / "38M"; never negative.
pub fn countdown(epoch: i64, now: i64) -> String {
    span(epoch - now)
}

pub fn span(secs: i64) -> String {
    let s = secs.max(0);
    let (d, h, m) = (s / 86400, s % 86400 / 3600, s % 3600 / 60);
    if d > 0 {
        format!("{d}D {h:02}H")
    } else if h > 0 {
        format!("{h}H {m:02}M")
    } else {
        format!("{m}M")
    }
}

/// seconds -> "45S" / "12M" / "3H" / "2D", as the panel writes ages.
pub fn short(secs: i64) -> String {
    let s = secs.max(0);
    if s < 60 {
        format!("{s}S")
    } else if s < 3600 {
        format!("{}M", s / 60)
    } else if s < 86400 {
        format!("{}H", s / 3600)
    } else {
        format!("{}D", s / 86400)
    }
}

/// tokens -> "850" / "3.4K" / "12K", as the panel writes rates (integer arithmetic, as the bash did).
pub fn fmt_k(n: i64) -> String {
    if n >= 10000 {
        format!("{}K", (n + 500) / 1000)
    } else if n >= 1000 {
        format!("{}.{}K", (n + 50) / 1000, (n + 50) % 1000 / 100)
    } else {
        format!("{n}")
    }
}

/// pct width -> "████░░░░"
pub fn bar(pct: i64, width: i64) -> String {
    let filled = ((pct * width + 50) / 100).clamp(0, width);
    let mut out = String::new();
    for i in 0..width {
        out.push(if i < filled { '█' } else { '░' });
    }
    out
}

/// jq's `. + 0.5 | floor`.
pub fn round_half_up(x: f64) -> i64 {
    (x + 0.5).floor() as i64
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    Green,
    Amber,
    Red,
}

pub fn tone(pct: i64) -> Tone {
    if pct >= 85 {
        Tone::Red
    } else if pct >= 60 {
        Tone::Amber
    } else {
        Tone::Green
    }
}

pub fn is_macos() -> bool {
    cfg!(target_os = "macos")
}

fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    match env::var_os(var) {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => home().join(fallback),
    }
    .join("pulse-limits")
}

pub fn home() -> PathBuf {
    PathBuf::from(env::var_os("HOME").unwrap_or_default())
}

pub fn cache_dir() -> PathBuf {
    xdg_dir("XDG_CACHE_HOME", ".cache")
}

pub fn config_dir() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config")
}

/// A non-empty environment variable.
pub fn env_path(name: &str) -> Option<PathBuf> {
    match env::var_os(name) {
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => None,
    }
}

/// The folder holding panel.html, the SwiftBar shims and bin/: the parent of the folder this
/// binary sits in. Looked up from the path we were invoked by before the resolved one, and
/// without resolving symlinks, so a Homebrew install is reached through opt/pulse-limits, a
/// path that stays valid across `brew upgrade` (the SwiftBar link points into it), and a
/// `~/.local/bin` symlink to a checkout falls through to the checkout itself. `PULSE_LIB`
/// overrides.
pub fn lib_dir() -> PathBuf {
    if let Some(d) = env_path("PULSE_LIB") {
        return d;
    }
    let has_panel = |d: &Path| d.join("panel.html").is_file();
    let mut candidates: Vec<PathBuf> = vec![];
    if let Some(argv0) = env::args_os().next().map(PathBuf::from) {
        if argv0.components().count() > 1 {
            candidates.push(if argv0.is_absolute() { argv0 } else { env::current_dir().unwrap_or_default().join(argv0) });
        } else if let Some(found) = which(&argv0.to_string_lossy()) {
            candidates.push(found);
        }
    }
    if let Ok(exe) = env::current_exe() {
        if let Ok(real) = exe.canonicalize() {
            candidates.push(real);
        }
        candidates.push(exe);
    }
    for exe in &candidates {
        let Some(here) = exe.parent() else { continue };
        let up = clean(&here.join(".."));
        if has_panel(&up) {
            return up;
        }
        let opt = clean(&here.join("..").join("opt").join("pulse-limits").join("libexec"));
        if has_panel(&opt) {
            return opt;
        }
    }
    candidates.first().and_then(|e| e.parent()).and_then(Path::parent).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
}

/// Lexical `..` and `.` removal, no symlink resolution.
fn clean(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

pub fn mtime(p: &Path) -> Option<i64> {
    let m = fs::metadata(p).ok()?.modified().ok()?;
    Some(m.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64)
}

pub fn mtime_f(p: &Path) -> Option<f64> {
    let m = fs::metadata(p).ok()?.modified().ok()?;
    Some(m.duration_since(UNIX_EPOCH).ok()?.as_secs_f64())
}

/// Writes through a sibling temp file and a rename, so a reader never sees half a file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("{}.tmp{}", path.extension().and_then(|e| e.to_str()).unwrap_or(""), std::process::id()));
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

/// A whole file as text without the trailing newline; None when it is not there.
pub fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

pub fn base64_encode(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    STANDARD.decode(clean).ok()
}

pub fn upper(s: &str) -> String {
    s.to_ascii_uppercase()
}

/// Whether a command on PATH exits 0 (`pgrep -x NAME`: is that process running?).
pub fn process_running(name: &str) -> bool {
    std::process::Command::new("pgrep")
        .args(["-x", name])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// stdout of a command, trimmed; None when it fails to run or exits non-zero.
pub fn command_output(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).stdin(std::process::Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Runs a command for its side effect, output discarded.
pub fn run_quiet(cmd: &str, args: &[&str]) -> bool {
    std::process::Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn which(cmd: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path).map(|d| d.join(cmd)).find(|p| is_executable(p))
}

pub fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// "~/.config/x" for a path under $HOME, as the bash `${x/#$HOME/\~}` printed it.
pub fn tilde(p: &Path) -> String {
    let s = p.display().to_string();
    let h = home().display().to_string();
    match s.strip_prefix(&h) {
        Some(rest) if !h.is_empty() => format!("~{rest}"),
        _ => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_parsing() {
        assert_eq!(epoch_of("2026-09-08T13:40:00.075883+00:00"), Some(1788874800));
        assert_eq!(epoch_of("2026-09-08T13:40:00Z"), Some(1788874800));
        assert_eq!(epoch_of("2026-09-08T13:40:00"), Some(1788874800));
        assert_eq!(epoch_of("2026-09-08T15:40:00+02:00"), Some(1788874800));
        assert_eq!(epoch_of("2026-09-08T15:40:00+0200"), Some(1788874800));
        assert_eq!(epoch_of_f("2026-09-08T13:41:24.643Z"), Some(1788874884.643));
        assert_eq!(epoch_of("-"), None);
        assert_eq!(epoch_of(""), None);
        assert_eq!(epoch_of("2026-13-08T13:40:00Z"), None);
        assert_eq!(epoch_of("2026-09-08T13:40:00X"), None);
        assert_eq!(epoch_of("2026-09-08T13:40:00+2"), None);
        assert_eq!(iso_utc(1788881706), "2026-09-08T15:35:06Z");
        assert_eq!(iso_minute(1788874884), "2026-09-08T13:41");
        for e in [0, 951782400, 1788881706, 4102444800, -86400] {
            assert_eq!(epoch_of(&iso_utc(e)), Some(e));
        }
    }

    #[test]
    fn formats() {
        assert_eq!(countdown(100, 0), "1M");
        assert_eq!(countdown(0, 100), "0M");
        assert_eq!(span(2 * 3600 + 14 * 60), "2H 14M");
        assert_eq!(span(4 * 86400 + 7 * 3600), "4D 07H");
        assert_eq!(short(45), "45S");
        assert_eq!(short(3599), "59M");
        assert_eq!(short(7200), "2H");
        assert_eq!(short(200000), "2D");
        assert_eq!(fmt_k(850), "850");
        assert_eq!(fmt_k(8098), "8.1K");
        assert_eq!(fmt_k(9960), "10.0K");
        assert_eq!(fmt_k(12345), "12K");
        assert_eq!(bar(13, 20), "███░░░░░░░░░░░░░░░░░");
        assert_eq!(bar(0, 4), "░░░░");
        assert_eq!(bar(100, 4), "████");
        assert_eq!(bar(120, 4), "████");
        assert_eq!(tone(59), Tone::Green);
        assert_eq!(tone(60), Tone::Amber);
        assert_eq!(tone(85), Tone::Red);
        assert_eq!(round_half_up(15.7), 16);
        assert_eq!(round_half_up(15.5), 16);
        assert_eq!(round_half_up(15.49), 15);
    }
}
