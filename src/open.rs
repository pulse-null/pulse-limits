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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::testing::{Scratch, ENV};
    use crate::util::testing::{calls, fake_bin, pretend, wait_for, Vars};
    use std::time::Duration;

    #[test]
    fn stamp_pid_and_launch() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("open");
        let bin = s.0.join("bin");
        let lib = s.0.join("lib");
        let mut vars = Vars::default();
        // `kill` is the real one, on a process of ours; everything else is a fake
        vars.set("PATH", format!("{}:/usr/bin:/bin", bin.display()));
        let open_log = fake_bin(&bin, "open", "");
        let xdg_log = fake_bin(&bin, "xdg-open", "");
        let cache = s.cache();
        let (stamp, pidfile, urlfile) = (cache.join("popover.closed"), cache.join("popover.pid"), cache.join("panel.url"));
        let _mac = pretend(true);
        // no reading yet: a message, nothing launched
        assert_eq!(open(&lib), 0);
        assert!(!open_log.exists());
        // the click that just closed the popover: swallowed, the stamp consumed
        fs::write(&urlfile, "file:///x/panel.html#abc\n").unwrap();
        fs::write(&stamp, format!("{}.123\n", now())).unwrap();
        assert_eq!(open(&lib), 0);
        assert!(!stamp.exists());
        assert!(!open_log.exists());
        // an old stamp, or an unreadable one: consumed and ignored; no helper built: the page opens in the browser
        fs::write(&stamp, format!("{}", now() - 5)).unwrap();
        assert_eq!(open(&lib), 0);
        assert!(!stamp.exists());
        assert_eq!(calls(&open_log), vec!["file:///x/panel.html#abc"]);
        fs::write(&stamp, "garbage").unwrap();
        assert_eq!(open(&lib), 0);
        assert_eq!(calls(&open_log).len(), 2);
        // a pid file that is not a pid, or a dead pid: launch as if there were none
        fs::write(&pidfile, "abc").unwrap();
        assert_eq!(open(&lib), 0);
        assert_eq!(calls(&open_log).len(), 3);
        let mut gone = Command::new("/bin/sh").arg("-c").arg("exit 0").spawn().unwrap();
        gone.wait().unwrap();
        fs::write(&pidfile, gone.id().to_string()).unwrap();
        assert_eq!(open(&lib), 0);
        assert_eq!(calls(&open_log).len(), 4);
        // the helper is built: launched detached with the popover size
        let pop_log = fake_bin(&lib.join("bin"), "pulse-popover", "");
        assert_eq!(open(&lib), 0);
        assert!(wait_for(&pop_log));
        assert_eq!(calls(&pop_log), vec![format!("{POPOVER_W} {POPOVER_H}")]);
        assert_eq!(calls(&open_log).len(), 4, "the browser is not opened when the helper runs");
        // resident: a live pid gets USR1 and nothing is launched
        let (ready, marker) = (s.0.join("ready"), s.0.join("usr1"));
        fake_bin(
            &lib.join("bin"),
            "pulse-popover",
            &format!(
                "trap 'printf usr1 > \"{}\"; exit 0' USR1\nprintf ok > \"{}\"\ni=0\nwhile [ $i -lt 100 ]; do sleep 0.1; i=$((i+1)); done",
                marker.display(),
                ready.display()
            ),
        );
        let mut child = Command::new(lib.join("bin").join("pulse-popover")).stdout(Stdio::null()).spawn().unwrap();
        fs::write(&pidfile, child.id().to_string()).unwrap();
        assert!(wait_for(&ready), "the fake installed its trap");
        assert_eq!(open(&lib), 0);
        assert!(wait_for(&marker), "USR1 reached the resident popover");
        child.wait().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(calls(&pop_log).len(), 2, "the second run is the resident one, not a launch");
        // Linux: a redirect page in the cache and xdg-open on it
        drop(_mac);
        let _linux = pretend(false);
        fs::remove_file(&pidfile).unwrap();
        assert_eq!(open(&lib), 0);
        let opener = cache.join("open.html");
        assert!(wait_for(&xdg_log));
        assert_eq!(calls(&xdg_log), vec![opener.display().to_string()]);
        assert_eq!(
            fs::read_to_string(&opener).unwrap(),
            "<!doctype html><meta charset=\"utf-8\"><meta http-equiv=\"refresh\" content=\"0;url=file:///x/panel.html#abc\"><title>PulseLimits</title><a href=\"file:///x/panel.html#abc\">PulseLimits</a>\n"
        );
        // the page cannot be written
        fs::remove_file(&opener).unwrap();
        fs::create_dir(&opener).unwrap();
        assert_eq!(open(&lib), 1);
        assert_eq!(calls(&xdg_log).len(), 1);
    }
}
