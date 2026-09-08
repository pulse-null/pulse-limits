//! Shows or hides the monitor. SwiftBar runs this on a left-click of the menu bar item, Waybar
//! on a click of the module (`pulse-limits open` from a terminal on either).
//! macOS: the popover (bin/pulse-popover) stays resident for a while; we only launch it if it is
//! not, and toggle it with SIGUSR1 when it is. Linux: no popover yet; the page opens in the
//! browser through a one-line redirect file, because xdg-open drops the #fragment of a file://
//! URL, and the data travels in the fragment.

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::swiftbar::{POPOVER_H, POPOVER_W};
use crate::util::{cache_dir, is_executable, is_macos, now, read_trimmed, run_quiet};

/// Starts a program that outlives us, in its own process group, without a terminal.
fn spawn_detached(cmd: &mut Command) -> bool {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0).spawn().is_ok()
}

pub fn open(lib: &Path) -> i32 {
    let cache = cache_dir();
    let (stamp, pidfile, urlfile) = (cache.join("popover.closed"), cache.join("popover.pid"), cache.join("panel.url"));
    // The click that ran us may have just hidden the monitor (its outside-click monitor fires
    // before SwiftBar runs us). Do not bounce it straight back open.
    if let Some(s) = read_trimmed(&stamp) {
        let closed: i64 = s.split('.').next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let _ = fs::remove_file(&stamp);
        if now() - closed <= 1 {
            return 0;
        }
    }
    if let Some(pid) = read_trimmed(&pidfile).filter(|p| p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty()) {
        if run_quiet("kill", &["-0", &pid]) {
            run_quiet("kill", &["-USR1", &pid]); // resident: toggle
            return 0;
        }
    }
    let Some(url) = read_trimmed(&urlfile) else {
        eprintln!("no reading yet: run pulse-limits status first");
        return 0;
    };
    let popover = lib.join("bin").join("pulse-popover");
    if is_macos() && is_executable(&popover) {
        spawn_detached(Command::new(&popover).args([POPOVER_W.to_string(), POPOVER_H.to_string()]));
    // first launch shows itself
    } else if is_macos() {
        run_quiet("open", &[&url]); // helper not built: at least show the page
    } else {
        let opener = cache.join("open.html");
        let page = format!("<!doctype html><meta charset=\"utf-8\"><meta http-equiv=\"refresh\" content=\"0;url={url}\"><title>PulseLimits</title><a href=\"{url}\">PulseLimits</a>\n");
        if fs::write(&opener, page).is_err() {
            eprintln!("cannot write {}", opener.display());
            return 1;
        }
        spawn_detached(Command::new("xdg-open").arg(&opener));
    }
    0
}
