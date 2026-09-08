//! What each CLI is doing right now, from the session logs it writes locally: output tokens in
//! the last minute, seconds since any log was last touched, and how many were touched in the
//! last five minutes. Claude Code's transcripts under ~/.claude/projects (CLAUDE_PROJECTS_DIR
//! moves that; the same numbers as the Swift helper this replaced), the Grok CLI's sessions
//! under ~/.grok/sessions (GROK_HOME) and the Codex CLI's rollouts under ~/.codex/sessions
//! (CODEX_HOME). Nothing is kept from the files but the counts.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::providers::{codex, grok};
use crate::util::{env_path, epoch_of_f, home, iso_minute, mtime_f, now_f};

pub fn projects_dir() -> PathBuf {
    env_path("CLAUDE_PROJECTS_DIR").unwrap_or_else(|| home().join(".claude").join("projects"))
}

/// (path, mtime) of every file under `root` whose name `keep` accepts; symlinks are not followed.
fn files(root: &Path, keep: fn(&str) -> bool) -> Vec<(PathBuf, f64)> {
    let mut out = vec![];
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() && keep(&e.file_name().to_string_lossy()) {
                if let Some(m) = mtime_f(&e.path()) {
                    out.push((e.path(), m));
                }
            }
        }
    }
    out
}

/// Claude Code: every *.jsonl under the projects dir.
pub fn transcripts(root: &Path) -> Vec<(PathBuf, f64)> {
    files(root, |n| n.ends_with(".jsonl"))
}

/// Grok: sessions/<cwd>/<session>/updates.jsonl, one per session. The other files there
/// (chat_history, events, prompt_history) are never opened.
fn grok_updates(root: &Path) -> Vec<(PathBuf, f64)> {
    files(root, |n| n == "updates.jsonl")
}

/// Codex: sessions/YYYY/MM/DD/rollout-<time>-<thread>.jsonl, one per thread.
fn codex_rollouts(root: &Path) -> Vec<(PathBuf, f64)> {
    files(root, |n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
}

/// How much of a file's end to read for a window this wide.
fn tail_for(span: f64) -> u64 {
    if span <= 120.0 {
        131_072
    } else if span <= 900.0 {
        1_048_576
    } else {
        4_194_304
    }
}

/// The last `bytes` of a file as text; the line cut at the front goes with it.
fn tail(path: &Path, bytes: u64) -> Option<String> {
    let mut fh = fs::File::open(path).ok()?;
    let size = fh.seek(SeekFrom::End(0)).ok()?;
    let start = size.saturating_sub(bytes);
    fh.seek(SeekFrom::Start(start)).ok()?;
    let mut data = Vec::with_capacity((size - start) as usize);
    fh.read_to_end(&mut data).ok()?;
    let mut text = String::from_utf8_lossy(&data).into_owned(); // the tail may start mid-character, in the line we drop anyway
    if start > 0 {
        text.drain(..text.find('\n').map(|i| i + 1).unwrap_or(text.len()));
    }
    Some(text)
}

/// ISO minute prefixes covering the window, so big lines can be skipped before a JSON parse.
fn minute_prefixes(from: f64, to: f64) -> Vec<String> {
    let mut prefixes = vec![];
    let mut t = from - 60.0;
    while t <= to + 60.0 {
        prefixes.push(iso_minute(t as i64));
        t += 60.0;
    }
    prefixes
}

/// Output tokens of assistant turns stamped in (from, to], across every transcript touched
/// since `from`, deduplicated by message id.
pub fn output_tokens(root: &Path, from: f64, to: f64) -> i64 {
    let mut per_message: HashMap<String, i64> = HashMap::new();
    let prefixes = minute_prefixes(from, to);
    let bytes = tail_for(to - from);
    for (path, mtime) in transcripts(root) {
        if mtime < from {
            continue;
        }
        let Some(text) = tail(&path, bytes) else { continue };
        for line in text.split('\n').filter(|l| !l.is_empty()) {
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

/// A Grok update's time: `timestamp` as the CLI writes it (epoch seconds; milliseconds and
/// ISO-8601 are read too), else `_meta.agentTimestampMs`.
fn grok_when(obj: &Value) -> Option<f64> {
    match obj.get("timestamp") {
        Some(Value::Number(n)) => n.as_f64().map(|t| if t > 1e11 { t / 1000.0 } else { t }),
        Some(Value::String(s)) => epoch_of_f(s),
        _ => obj.pointer("/params/_meta/agentTimestampMs").and_then(as_int).map(|ms| ms as f64 / 1000.0),
    }
}

/// `n` spread evenly over [start, end]: the share inside (from, to]. A span with no length
/// counts whole when its instant is inside.
fn share(n: f64, start: f64, end: f64, from: f64, to: f64) -> f64 {
    if end <= start {
        return if end > from && end <= to { n } else { 0.0 };
    }
    let overlap = end.min(to) - start.max(from);
    if overlap > 0.0 {
        n * overlap / (end - start)
    } else {
        0.0
    }
}

/// Output tokens of the Grok CLI in (from, to], from every session's updates.jsonl touched since
/// `from`. A finished turn has real numbers, `usage.outputTokens + usage.reasoningTokens` on its
/// `turn_completed` line, spread evenly from the turn's start (`_meta.turnStartMs` on its lines)
/// to that line, so a long turn reads as its average rate and not as a spike when it ends. The
/// turn still running (the lines after the last `turn_completed`) has no usage yet: its live
/// number is the growth of `_meta.totalTokens` between consecutive lines of one model call (the
/// same `_meta.streamStartMs`). That total is the call's context as the CLI counts it, which
/// grows with what the model writes and with what its tools return, so it runs above the real
/// output while tools work; the usage takes over when the turn ends. The first line of a call
/// is its input context and not a step, and a total is never carried across calls or sessions.
pub fn grok_tokens(root: &Path, from: f64, to: f64) -> i64 {
    let bytes = tail_for(to - from).max(1 << 20); // tool results make long lines
    let mut total = 0.0;
    for (path, mtime) in grok_updates(root) {
        if mtime < from {
            continue;
        }
        let Some(text) = tail(&path, bytes) else { continue };
        let (mut turn_start, mut first_seen, mut prev, mut growth) = (None::<f64>, None::<f64>, None::<(i64, i64)>, 0.0);
        for line in text.split('\n').filter(|l| !l.is_empty()) {
            if !line.contains("totalTokens") && !line.contains("turn_completed") {
                continue;
            }
            let Ok(obj) = serde_json::from_str::<Value>(line) else { continue };
            let Some(when) = grok_when(&obj) else { continue };
            let Some(params) = obj.get("params") else { continue };
            if params.pointer("/update/sessionUpdate").and_then(Value::as_str) == Some("turn_completed") {
                if let Some(u) = params.pointer("/update/usage") {
                    let out = u.get("outputTokens").and_then(as_int).unwrap_or(0) + u.get("reasoningTokens").and_then(as_int).unwrap_or(0);
                    total += share(out as f64, turn_start.or(first_seen).unwrap_or(when), when, from, to);
                }
                (turn_start, first_seen, prev, growth) = (None, None, None, 0.0);
                continue;
            }
            let Some(meta) = params.get("_meta") else { continue };
            let Some(t) = meta.get("totalTokens").and_then(as_int) else { continue };
            first_seen.get_or_insert(when);
            if turn_start.is_none() {
                turn_start = meta.get("turnStartMs").and_then(as_int).map(|ms| ms as f64 / 1000.0);
            }
            let stream = meta.get("streamStartMs").and_then(as_int).unwrap_or(0);
            if let Some((s, p)) = prev {
                if s == stream && t > p && when > from && when <= to {
                    growth += (t - p) as f64;
                }
            }
            prev = Some((stream, t));
        }
        total += growth;
    }
    total.round() as i64
}

/// Output tokens of the Codex CLI in (from, to]: the `token_count` events in every rollout
/// touched since `from`, `payload.info.last_token_usage.output_tokens` (the response that just
/// finished; `total_token_usage` is the thread's running total and would count it again, and
/// `output_tokens` holds the reasoning already, as the Responses API counts it). Built from the
/// Codex source, not checked against a real install: codex-rs/history/src/lib.rs (`RolloutLine`:
/// timestamp, type, payload) and rollout_payload.rs (`type` event_msg), codex-rs/protocol/src/
/// protocol.rs (`EventMsg` tagged `type` token_count, `TokenCountEvent { info: { total_token_usage,
/// last_token_usage: TokenUsage { input_tokens, cached_input_tokens, output_tokens,
/// reasoning_output_tokens, total_tokens } } }`), codex-rs/rollout/src/recorder.rs and
/// rollout_file_name.rs (sessions/YYYY/MM/DD/rollout-<local time>-<thread>.jsonl, UTC stamps
/// with milliseconds and a Z). The newer `token_usage_record` lines are the same responses
/// again and are not counted.
pub fn codex_tokens(root: &Path, from: f64, to: f64) -> i64 {
    let prefixes = minute_prefixes(from, to);
    let bytes = tail_for(to - from);
    let mut total = 0;
    for (path, mtime) in codex_rollouts(root) {
        if mtime < from {
            continue;
        }
        let Some(text) = tail(&path, bytes) else { continue };
        for line in text.split('\n').filter(|l| !l.is_empty()) {
            if !line.contains("\"token_count\"") || !prefixes.iter().any(|p| line.contains(p.as_str())) {
                continue;
            }
            let Ok(obj) = serde_json::from_str::<Value>(line) else { continue };
            let Some(when) = obj.get("timestamp").and_then(Value::as_str).and_then(epoch_of_f) else { continue };
            if !(when > from && when <= to) || obj.pointer("/payload/type").and_then(Value::as_str) != Some("token_count") {
                continue;
            }
            if let Some(n) = obj.pointer("/payload/info/last_token_usage/output_tokens").and_then(as_int) {
                total += n;
            }
        }
    }
    total
}

pub struct Activity {
    pub tok_per_min: i64,
    pub idle_s: i64,
    pub sessions: i64,
}

impl Default for Activity {
    /// Nothing seen: no tokens, idle for a year, no sessions.
    fn default() -> Activity {
        Activity { tok_per_min: 0, idle_s: 86_400 * 365, sessions: 0 }
    }
}

impl Activity {
    pub fn to_json(&self) -> Value {
        json!({ "tok_per_min": self.tok_per_min, "idle_s": self.idle_s, "sessions": self.sessions })
    }
}

/// One provider's local source: where its files are, how to list them and how to count them.
struct Reader {
    root: PathBuf,
    list: fn(&Path) -> Vec<(PathBuf, f64)>,
    tokens: fn(&Path, f64, f64) -> i64,
}

fn reader(provider: &str) -> Option<Reader> {
    Some(match provider {
        "claude" => Reader { root: projects_dir(), list: transcripts, tokens: output_tokens },
        "grok" => Reader { root: grok::grok_dir().join("sessions"), list: grok_updates, tokens: grok_tokens },
        "codex" => Reader { root: codex::codex_dir().join("sessions"), list: codex_rollouts, tokens: codex_tokens },
        _ => return None,
    })
}

fn measure_with(r: &Reader) -> Activity {
    let now = now_f();
    let mut a = Activity::default();
    let mut newest: Option<f64> = None;
    for (_, m) in (r.list)(&r.root) {
        if newest.is_none_or(|n| m > n) {
            newest = Some(m);
        }
        if now - m <= 300.0 {
            a.sessions += 1;
        }
    }
    if let Some(n) = newest {
        a.idle_s = (now - n) as i64;
    }
    a.tok_per_min = (r.tokens)(&r.root, now - 60.0, now);
    a
}

/// Claude Code's reading from a projects dir; zeros and a year idle when it is not there.
pub fn measure_in(root: &Path) -> Activity {
    measure_with(&Reader { root: root.into(), list: transcripts, tokens: output_tokens })
}

pub fn measure() -> Activity {
    measure_in(&projects_dir())
}

/// A provider's reading; None for a name that is not a provider, or one whose files are not on
/// this machine.
pub fn measure_for(provider: &str) -> Option<Activity> {
    reader(provider).filter(|r| r.root.is_dir()).map(|r| measure_with(&r))
}

/// Where a provider's reading comes from, for the doctor: the root, how many files it holds,
/// and when the newest was touched. None as `measure_for` says None.
pub struct Source {
    pub root: PathBuf,
    pub files: usize,
    pub newest: Option<i64>,
}

pub fn source(provider: &str) -> Option<Source> {
    let r = reader(provider).filter(|r| r.root.is_dir())?;
    let list = (r.list)(&r.root);
    Some(Source { root: r.root, files: list.len(), newest: list.iter().map(|(_, m)| *m as i64).max() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::testing::{Sandbox, Scratch, ENV};
    use crate::util::testing::Vars;
    use crate::util::{iso_utc, now};

    const GROK_UPDATES: &str = include_str!("../tests/fixtures/activity/grok-updates.synthetic.jsonl");
    const CODEX_ROLLOUT: &str = include_str!("../tests/fixtures/activity/codex-rollout.synthetic.jsonl");
    const BASE: f64 = 1788874800.0; // 2026-09-08T13:40:00Z, the fixtures' first minute

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

    /// A Grok update with a running total; `ts` is the JSON of the timestamp field.
    fn gline(ts: &str, turn_start_ms: i64, stream_ms: i64, total: i64) -> String {
        format!("{{\"timestamp\":{ts},\"method\":\"_x.ai/session/update\",\"params\":{{\"_meta\":{{\"eventId\":\"e\",\"agentTimestampMs\":{turn_start_ms},\"updateType\":\"ToolCallUpdate\",\"turnStartMs\":{turn_start_ms},\"streamStartMs\":{stream_ms},\"totalTokens\":{total}}},\"sessionId\":\"s\",\"update\":{{\"sessionUpdate\":\"tool_call_update\",\"toolCallId\":\"t\",\"status\":\"completed\"}}}}}}\n")
    }

    fn gdone(ts: &str, out: i64, reasoning: i64) -> String {
        format!("{{\"timestamp\":{ts},\"method\":\"_x.ai/session/update\",\"params\":{{\"_meta\":{{\"eventId\":\"e\",\"agentTimestampMs\":0}},\"sessionId\":\"s\",\"update\":{{\"sessionUpdate\":\"turn_completed\",\"usage\":{{\"inputTokens\":100,\"outputTokens\":{out},\"totalTokens\":{},\"cachedReadTokens\":0,\"cacheCreationTokens\":0,\"reasoningTokens\":{reasoning},\"modelCalls\":1,\"apiDurationMs\":1000,\"costUsdTicks\":1,\"modelUsage\":{{}},\"numTurns\":1}}}}}}}}\n", 100 + out)
    }

    fn cline(ts: &str, out: &str) -> String {
        format!("{{\"timestamp\":\"{ts}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"total_token_usage\":{{\"input_tokens\":9,\"cached_input_tokens\":0,\"output_tokens\":9,\"reasoning_output_tokens\":0,\"total_tokens\":18}},\"last_token_usage\":{{\"input_tokens\":9,\"cached_input_tokens\":0,\"output_tokens\":{out},\"reasoning_output_tokens\":0,\"total_tokens\":18}},\"model_context_window\":272000}},\"rate_limits\":null}}}}\n")
    }

    fn make_old(path: &Path) {
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        fs::File::options().write(true).open(path).unwrap().set_modified(old).unwrap();
    }

    #[test]
    fn dedup_window_and_prefilter() {
        let d = fixture("tokens");
        let base = BASE;
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
        make_old(&d.join("proj-b").join("old.jsonl"));
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
        assert_eq!(output_tokens(&d, BASE, BASE + 100.0), 5);
        // a tail that is one unfinished line: nothing survives, nothing panics
        fs::write(d.join("proj-a").join("s.jsonl"), format!("{{\"pad\":\"{}\"", "y".repeat(200_000))).unwrap();
        assert_eq!(output_tokens(&d, BASE, BASE + 100.0), 0);
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
        assert!(measure_for("claude").is_none(), "no projects dir: no source");
        assert!(source("claude").is_none());
        let p = s.0.join("p");
        vars.set("CLAUDE_PROJECTS_DIR", &p);
        assert_eq!(projects_dir(), p);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("s.jsonl"), line(&iso_utc(now() - 10), "m", "300")).unwrap();
        let a = measure();
        assert_eq!((a.tok_per_min, a.sessions), (300, 1));
        assert!(a.idle_s <= 5);
        let a = measure_for("claude").unwrap();
        assert_eq!((a.tok_per_min, a.sessions), (300, 1));
        let src = source("claude").unwrap();
        assert_eq!((src.root, src.files), (p.clone(), 1));
        assert!(now() - src.newest.unwrap() <= 5);
        // wider windows read a deeper tail
        assert_eq!(output_tokens(&p, now_f() - 600.0, now_f()), 300);
        assert_eq!(output_tokens(&p, now_f() - 3600.0, now_f()), 300);
        // not a provider
        assert!(measure_for("gemini").is_none());
        assert!(source("gemini").is_none());
    }

    #[test]
    fn grok_turns_finished_and_running() {
        let d = fixture("grok");
        let s1 = d.join("cwd-a").join("session-1");
        fs::create_dir_all(&s1).unwrap();
        fs::write(s1.join("updates.jsonl"), GROK_UPDATES).unwrap();
        // the other files of a session are never read: a turn in them would be counted otherwise
        fs::write(s1.join("chat_history.jsonl"), gdone("1788875000", 99999, 0)).unwrap();
        fs::write(s1.join("events.jsonl"), gline("1788875000", 0, 1, 99999)).unwrap();
        fs::write(d.join("cwd-a").join("prompt_history.jsonl"), gdone("1788875000", 99999, 0)).unwrap();
        // (13:41:30, 13:42:30]: the finished turn (600 over 13:41:00-13:42:00) gives its second half
        assert_eq!(grok_tokens(&d, BASE + 90.0, BASE + 150.0), 300);
        // (13:40:30, 13:42:30]: the whole turn; the totals inside it are not counted on top
        assert_eq!(grok_tokens(&d, BASE + 30.0, BASE + 150.0), 600);
        // (13:41:40, 13:41:50]: a sixth of the turn, whatever the totals did in that slice
        assert_eq!(grok_tokens(&d, BASE + 100.0, BASE + 110.0), 100);
        // (13:43:00, 13:44:30]: the turn in progress, from its totals: 250 + 0 + 100; the step into the new stream is not one
        assert_eq!(grok_tokens(&d, BASE + 180.0, BASE + 270.0), 350);
        // (13:41:30, 13:44:30]: both
        assert_eq!(grok_tokens(&d, BASE + 90.0, BASE + 270.0), 650);
        // (13:44:10, 13:44:30]: the line stamped only in agentTimestampMs
        assert_eq!(grok_tokens(&d, BASE + 250.0, BASE + 270.0), 100);
        // idle: a window after the last line
        assert_eq!(grok_tokens(&d, BASE + 300.0, BASE + 360.0), 0);
        // a second session: its totals are its own (100 here is no step from the first's 9100), and a
        // turn whose lines carry no turnStartMs starts at its first line; a third with only the
        // turn_completed line is an instant
        let s2 = d.join("cwd-b").join("session-2");
        fs::create_dir_all(&s2).unwrap();
        fs::write(s2.join("updates.jsonl"), format!("{}{}", gline("1788875000", 0, 5, 100).replace(",\"turnStartMs\":0", ""), gdone("1788875005", 50, 0)))
            .unwrap();
        let s3 = d.join("cwd-b").join("session-3");
        fs::create_dir_all(&s3).unwrap();
        fs::write(s3.join("updates.jsonl"), gdone("1788875040", 7, 0)).unwrap();
        assert_eq!(grok_tokens(&d, BASE + 180.0, BASE + 270.0), 350 + 50 + 7);
        assert_eq!(grok_tokens(&d, BASE + 250.0, BASE + 270.0), 100); // 13:44:00 is before that window
        assert_eq!(grok_tokens(&d, BASE + 200.0, BASE + 202.0), 50 * 2 / 5); // (13:43:20, 13:43:22]: two of the five seconds
                                                                             // a session older than the window is skipped by mtime
        make_old(&s1.join("updates.jsonl"));
        assert_eq!(grok_tokens(&d, BASE + 180.0, BASE + 270.0), 57);
        assert_eq!(grok_tokens(&d.join("nope"), BASE, BASE + 60.0), 0);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn grok_tail_and_stamps() {
        let d = fixture("grok-tail");
        let s = d.join("c").join("s");
        fs::create_dir_all(&s).unwrap();
        // a first line bigger than the 1 MiB tail: its remainder is dropped, the steps after it count
        let mut text =
            format!("{{\"timestamp\":1788874860,\"pad\":\"{}\",\"params\":{{\"_meta\":{{\"streamStartMs\":1,\"totalTokens\":1}}}}}}\n", "x".repeat(1_200_000));
        text.push_str(&gline("1788874870", 0, 1, 500));
        text.push_str(&gline("1788874880", 0, 1, 900));
        // milliseconds and ISO-8601 stamps are read too; a stamp that is neither is skipped
        text.push_str(&gline("1788874890000", 0, 1, 950));
        text.push_str(&gline("\"2026-09-08T13:41:40.500Z\"", 0, 1, 1000));
        text.push_str(&gline("\"yesterday\"", 0, 1, 5000));
        text.push_str(&gline("true", 0, 1, 5000));
        fs::write(s.join("updates.jsonl"), text).unwrap();
        assert_eq!(grok_tokens(&d, BASE + 60.0, BASE + 120.0), 400 + 50 + 50);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn grok_measure_and_source() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let sb = Sandbox::new("activity-grok");
        let sessions = sb.home().join(".grok").join("sessions");
        assert!(measure_for("grok").is_none(), "no sessions dir: no source");
        assert!(source("grok").is_none());
        let s1 = sessions.join("cwd").join("s1");
        fs::create_dir_all(&s1).unwrap();
        // a turn running now: 300 tokens of growth in the last minute
        let t = now_f() - 20.0;
        let ms = (t * 1000.0) as i64;
        fs::write(s1.join("updates.jsonl"), format!("{}{}", gline(&format!("{}", t as i64), ms, ms, 100), gline(&format!("{}", t as i64 + 10), ms, ms, 400)))
            .unwrap();
        // a session from long ago
        let s2 = sessions.join("cwd").join("s2");
        fs::create_dir_all(&s2).unwrap();
        fs::write(s2.join("updates.jsonl"), gdone("1000000000", 9, 0)).unwrap();
        make_old(&s2.join("updates.jsonl"));
        let a = measure_for("grok").unwrap();
        assert_eq!((a.tok_per_min, a.sessions), (300, 1));
        assert!(a.idle_s <= 5, "{}", a.idle_s);
        let src = source("grok").unwrap();
        assert_eq!((src.root, src.files), (sessions, 2));
        assert!(now() - src.newest.unwrap() <= 5);
        drop(sb);
    }

    #[test]
    fn codex_rollouts_rate_and_measure() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let sb = Sandbox::new("activity-codex");
        let sessions = sb.home().join(".codex").join("sessions");
        assert!(measure_for("codex").is_none(), "no sessions dir: no source");
        assert!(source("codex").is_none());
        let day = sessions.join("2026").join("09").join("08");
        fs::create_dir_all(&day).unwrap();
        fs::write(day.join("rollout-2026-09-08T15-41-00-0f0e0d0c-0b0a-4908-8706-050403020100.jsonl"), CODEX_ROLLOUT).unwrap();
        // not a rollout: never read
        fs::write(day.join("notes.jsonl"), cline("2026-09-08T13:41:30.000Z", "99999")).unwrap();
        fs::write(day.join("rollout-x.txt"), cline("2026-09-08T13:41:30.000Z", "99999")).unwrap();
        // (13:41:00, 13:45:00]: the two responses; the rate-limit-only event, the record line, a string and a fraction are skipped
        assert_eq!(codex_tokens(&sessions, BASE + 60.0, BASE + 300.0), 800);
        assert_eq!(codex_tokens(&sessions, BASE + 120.0, BASE + 180.0), 500);
        assert_eq!(codex_tokens(&sessions, BASE + 240.0, BASE + 300.0), 0);
        assert_eq!(codex_tokens(&sessions.join("nope"), BASE, BASE + 60.0), 0);
        // the reading now: a response 10 s ago in a second thread, the fixture's file is a session but its lines are old
        let live = day.join("rollout-2026-09-08T15-42-00-1f1e1d1c-1b1a-4918-9716-151413121110.jsonl");
        fs::write(&live, cline(&iso_utc(now() - 10), "42")).unwrap();
        let a = measure_for("codex").unwrap();
        assert_eq!((a.tok_per_min, a.sessions), (42, 2));
        assert!(a.idle_s <= 5, "{}", a.idle_s);
        make_old(&day.join("rollout-2026-09-08T15-41-00-0f0e0d0c-0b0a-4908-8706-050403020100.jsonl"));
        let a = measure_for("codex").unwrap();
        assert_eq!((a.tok_per_min, a.sessions), (42, 1));
        assert_eq!(codex_tokens(&sessions, BASE + 60.0, BASE + 300.0), 0); // the old file is skipped by mtime
        let src = source("codex").unwrap();
        assert_eq!((src.root, src.files), (sessions, 2));
        assert!(now() - src.newest.unwrap() <= 5);
        drop(sb);
    }
}
