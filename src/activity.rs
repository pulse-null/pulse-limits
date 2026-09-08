//! What Claude Code is doing right now, from the transcripts it writes under
//! ~/.claude/projects (CLAUDE_PROJECTS_DIR moves that): output tokens in the last minute
//! (deduplicated by message id, since a streaming reply is written several times), seconds
//! since any transcript was last touched, and how many were touched in the last five minutes.
//! Same numbers as the Swift helper this replaces.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::util::{env_path, epoch_of_f, home, iso_minute, now_f};

pub fn projects_dir() -> PathBuf {
    env_path("CLAUDE_PROJECTS_DIR").unwrap_or_else(|| home().join(".claude").join("projects"))
}

/// (path, mtime) of every *.jsonl under the projects dir; symlinks are not followed.
pub fn transcripts(root: &Path) -> Vec<(PathBuf, f64)> {
    let mut out = vec![];
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() && e.file_name().to_string_lossy().ends_with(".jsonl") {
                if let Some(m) = crate::util::mtime_f(&e.path()) {
                    out.push((e.path(), m));
                }
            }
        }
    }
    out
}

/// Output tokens of assistant turns stamped in (from, to], across every transcript touched
/// since `from`, deduplicated by message id.
pub fn output_tokens(root: &Path, from: f64, to: f64) -> i64 {
    let mut per_message: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    // ISO minute prefixes covering the window, so big lines can be skipped before a JSON parse
    let mut prefixes = vec![];
    let mut t = from - 60.0;
    while t <= to + 60.0 {
        prefixes.push(iso_minute(t as i64));
        t += 60.0;
    }
    let span = to - from;
    let tail: u64 = if span <= 120.0 {
        131_072
    } else if span <= 900.0 {
        1_048_576
    } else {
        4_194_304
    };
    for (path, mtime) in transcripts(root) {
        if mtime < from {
            continue;
        }
        let Ok(mut fh) = fs::File::open(&path) else { continue };
        let Ok(size) = fh.seek(SeekFrom::End(0)) else { continue };
        let start = size.saturating_sub(tail);
        if fh.seek(SeekFrom::Start(start)).is_err() {
            continue;
        }
        let mut data = Vec::with_capacity((size - start) as usize);
        if fh.read_to_end(&mut data).is_err() {
            continue;
        }
        let text = String::from_utf8_lossy(&data); // the tail may start mid-character, in the line we drop anyway
        let mut lines = text.split('\n').filter(|l| !l.is_empty());
        if start > 0 {
            lines.next(); // a partial line
        }
        for line in lines {
            if !line.contains("\"type\":\"assistant\"") || !line.contains("output_tokens") || !prefixes.iter().any(|p| line.contains(p.as_str())) {
                continue;
            }
            let Ok(obj) = serde_json::from_str::<Value>(line) else { continue };
            let Some(when) = obj.get("timestamp").and_then(Value::as_str).and_then(epoch_of_f) else { continue };
            if !(when > from && when <= to) {
                continue;
            }
            let Some(msg) = obj.get("message").filter(|m| m.is_object()) else { continue };
            let (Some(id), Some(out)) = (msg.get("id").and_then(Value::as_str), msg.get("usage").and_then(|u| u.get("output_tokens")).and_then(as_int)) else {
                continue;
            };
            let e = per_message.entry(id.to_string()).or_insert(0);
            *e = (*e).max(out);
        }
    }
    per_message.values().sum()
}

/// An integer, also when JSON wrote it as 12.0; never a bool.
fn as_int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64)),
        _ => None,
    }
}

pub struct Activity {
    pub tok_per_min: i64,
    pub idle_s: i64,
    pub sessions: i64,
}

impl Activity {
    pub fn to_json(&self) -> Value {
        json!({ "tok_per_min": self.tok_per_min, "idle_s": self.idle_s, "sessions": self.sessions })
    }
}

pub fn measure_in(root: &Path) -> Activity {
    let now = now_f();
    let mut newest: Option<f64> = None;
    let mut sessions = 0;
    for (_, m) in transcripts(root) {
        if newest.is_none_or(|n| m > n) {
            newest = Some(m);
        }
        if now - m <= 300.0 {
            sessions += 1;
        }
    }
    let idle = newest.map(|n| (now - n) as i64).unwrap_or(86_400 * 365);
    Activity { tok_per_min: output_tokens(root, now - 60.0, now), idle_s: idle, sessions }
}

pub fn measure() -> Activity {
    measure_in(&projects_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::testing::{Scratch, ENV};
    use crate::util::testing::Vars;
    use crate::util::{iso_utc, now};

    fn fixture(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pl-rust-act-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("proj-a")).unwrap();
        fs::create_dir_all(d.join("proj-b")).unwrap();
        d
    }

    fn line(ts: &str, id: &str, out: &str) -> String {
        format!("{{\"type\":\"assistant\",\"timestamp\":\"{ts}\",\"message\":{{\"id\":\"{id}\",\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"caf\\u00e9\"}}],\"usage\":{{\"input_tokens\":3,\"output_tokens\":{out}}}}}}}\n")
    }

    #[test]
    fn dedup_window_and_prefilter() {
        let d = fixture("tokens");
        let base = 1788874800.0; // 2026-09-08T13:40:00Z
        let mut s = String::new();
        s.push_str("{\"type\":\"user\",\"timestamp\":\"2026-09-08T13:41:14.643Z\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n");
        s.push_str(&line("2026-09-08T13:41:24.643Z", "msg_3", "800"));
        s.push_str(&line("2026-09-08T13:42:14.643Z", "msg_2", "500"));
        s.push_str(&line("2026-09-08T13:44:14.643Z", "msg_1", "40")); // streamed twice: the larger count wins
        s.push_str(&line("2026-09-08T13:44:14.643Z", "msg_1", "100"));
        s.push_str(&line("2026-09-08T13:44:14.643Z", "msg_4", "12.0")); // a float that is whole still counts
        s.push_str(&line("2026-09-08T13:44:14.643Z", "msg_5", "true")); // not a number
        s.push_str("{\"type\":\"assistant\",\"timestamp\":\"2026-09-08T13:44:14.643Z\",\"message\":\"output_tokens\"}\n");
        s.push_str("not json output_tokens \"type\":\"assistant\" 2026-09-08T13:44\n");
        fs::write(d.join("proj-a").join("s1.jsonl"), &s).unwrap();
        fs::write(d.join("proj-b").join("old.jsonl"), line("2026-09-08T13:43:30Z", "msg_old", "9999")).unwrap();
        // (13:41:00, 13:45:00]: everything in s1 that parses
        assert_eq!(output_tokens(&d, base + 60.0, base + 300.0), 800 + 500 + 100 + 12 + 9999);
        // (13:42:00, 13:43:00]: only msg_2 (13:41:24 is out, 13:44 is out)
        assert_eq!(output_tokens(&d, base + 120.0, base + 180.0), 500);
        // a file older than the window is skipped by mtime
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        let f = fs::File::options().write(true).open(d.join("proj-b").join("old.jsonl")).unwrap();
        f.set_modified(old).unwrap();
        assert_eq!(output_tokens(&d, base + 60.0, base + 300.0), 800 + 500 + 100 + 12);
        // nothing in a window with no lines, and nothing from a missing dir
        assert_eq!(output_tokens(&d, base - 3600.0, base - 3000.0), 0);
        assert_eq!(output_tokens(&d.join("nope"), base, base + 60.0), 0);
        let a = measure_in(&d.join("nope"));
        assert_eq!((a.tok_per_min, a.idle_s, a.sessions), (0, 86_400 * 365, 0));
        let a = measure_in(&d);
        assert_eq!((a.tok_per_min, a.sessions), (0, 1)); // s1 was just written, old.jsonl is ancient
        assert!(a.idle_s <= 5);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn tail_drops_the_partial_first_line() {
        let d = fixture("tail");
        let mut s = String::new();
        // a first line far bigger than the 128 KiB tail: its cut-off remainder must be dropped, the rest counted
        s.push_str(&format!("{{\"type\":\"assistant\",\"timestamp\":\"2026-09-08T13:41:24Z\",\"pad\":\"{}\",\"message\":{{\"id\":\"big\",\"usage\":{{\"output_tokens\":7}}}}}}\n", "x".repeat(200_000)));
        s.push_str(&line("2026-09-08T13:41:30Z", "small", "5"));
        fs::write(d.join("proj-a").join("s.jsonl"), &s).unwrap();
        assert_eq!(output_tokens(&d, 1788874800.0, 1788874900.0), 5);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn measure_and_dirs() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("activity");
        let mut vars = Vars::default();
        vars.set("HOME", &s.0).unset("CLAUDE_PROJECTS_DIR");
        assert_eq!(projects_dir(), s.0.join(".claude").join("projects"));
        let a = measure();
        assert_eq!((a.tok_per_min, a.idle_s, a.sessions), (0, 86_400 * 365, 0));
        assert_eq!(a.to_json().to_string(), "{\"tok_per_min\":0,\"idle_s\":31536000,\"sessions\":0}");
        let p = s.0.join("p");
        vars.set("CLAUDE_PROJECTS_DIR", &p);
        assert_eq!(projects_dir(), p);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("s.jsonl"), line(&iso_utc(now() - 10), "m", "300")).unwrap();
        let a = measure();
        assert_eq!((a.tok_per_min, a.sessions), (300, 1));
        assert!(a.idle_s <= 5);
        // wider windows read a deeper tail
        assert_eq!(output_tokens(&p, now_f() - 600.0, now_f()), 300);
        assert_eq!(output_tokens(&p, now_f() - 3600.0, now_f()), 300);
    }
}
