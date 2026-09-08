//! One reader per provider. A reader produces ONE document and never fails the caller:
//!   { "provider": "claude", "plan": "MAX 20X", "source": "LIVE" | "CACHE" | "", "fetched": <epoch, 0 if none>,
//!     "status": "", "hint": "", "windows": [ { "label": "SESSION", "pct": 17, "resets": "<iso8601>" | null }, ... ],
//!     "credits": { "used": 1.5, "currency": "EUR" } | null, "history": [ [epoch, session_pct], ... ] }
//! The window labelled SESSION is the short one the bar shows, WEEK the 7-day one; anything
//! else is a per-model cap. Errors go in status (a short name) and hint (what to do about it),
//! so one broken provider cannot take the others down.
//!
//! Per provider, in the cache dir: usage-<name>.json (last good reply), last-reply-<name>.json
//! (last reply of any kind), backoff-<name> (no calls until this epoch, written after a 429),
//! history-<name>.tsv (trend rows). The shared plumbing lives in `Store`.

pub mod claude;
pub mod codex;

use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};

use crate::util::{cache_dir, config_dir, mtime, now, read_trimmed, round_half_up, write_atomic};

pub const KNOWN: [&str; 2] = ["claude", "codex"]; // the name is also the CLI's process name
pub const HISTORY_HOURS: i64 = 12; // trend strip depth
pub const BACKOFF_SECS: i64 = 180; // after a 429: the quotas are small and shared across machines

#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub label: String,
    pub pct: Value, // the number as the reply wrote it (13 stays 13, 28.0 stays 28.0)
    pub resets: Option<String>,
}

impl Window {
    pub fn to_json(&self) -> Value {
        json!({ "label": self.label, "pct": self.pct, "resets": self.resets })
    }

    pub fn pct_f(&self) -> f64 {
        self.pct.as_f64().unwrap_or(0.0)
    }
}

#[derive(Clone, Debug)]
pub struct Doc {
    pub provider: String,
    pub plan: String,
    pub source: String,
    pub fetched: i64,
    pub status: String,
    pub hint: String,
    pub windows: Vec<Window>,
    pub credits: Value,
    pub history: Vec<(i64, i64)>,
}

impl Doc {
    /// The entry the payload's providers[] carries for every provider.
    pub fn summary(&self) -> Value {
        json!({ "name": self.provider, "plan": self.plan, "status": self.status, "hint": self.hint, "fetched": self.fetched,
                "windows": self.windows.iter().map(Window::to_json).collect::<Vec<_>>() })
    }

    pub fn window(&self, label: &str) -> Option<&Window> {
        self.windows.iter().find(|w| w.label == label)
    }

    /// The window the bar shows: SESSION, or the first one for a provider with no session window.
    pub fn session(&self) -> Option<&Window> {
        self.window("SESSION").or(self.windows.first())
    }

    fn failed(name: &str) -> Doc {
        Doc {
            provider: name.into(),
            plan: String::new(),
            source: String::new(),
            fetched: 0,
            status: "READER FAILED".into(),
            hint: format!("THE {} READER PRINTED NO DOCUMENT. RUN: pulse-limits doctor", name.to_ascii_uppercase()),
            windows: vec![],
            credits: Value::Null,
            history: vec![],
        }
    }

    /// What the payload shows when no provider is enabled.
    pub fn none() -> Doc {
        Doc {
            provider: String::new(),
            plan: String::new(),
            source: String::new(),
            fetched: 0,
            status: "NO PROVIDER".into(),
            hint: "ENABLE ONE: RIGHT-CLICK THE MENU BAR ITEM, PROVIDERS".into(),
            windows: vec![],
            credits: Value::Null,
            history: vec![],
        }
    }
}

pub fn known(name: &str) -> bool {
    KNOWN.contains(&name)
}

/// Enabled providers from ~/.config/pulse-limits/providers, one per line, first = the bar's
/// default; unknown names are ignored; no file means claude.
pub fn enabled() -> Vec<String> {
    let file = config_dir().join("providers");
    match fs::read_to_string(&file) {
        Ok(text) => text.split_whitespace().filter(|p| known(p)).map(str::to_string).collect(),
        Err(_) => vec!["claude".into()],
    }
}

/// Every name in the file, known or not, as the bash toggle kept them.
fn listed() -> Vec<String> {
    match fs::read_to_string(config_dir().join("providers")) {
        Ok(text) => text.split_whitespace().map(str::to_string).collect(),
        Err(_) => vec!["claude".into()],
    }
}

/// Enables or disables a provider; a newly enabled one goes last in priority.
pub fn toggle(name: &str) -> Result<Vec<String>, String> {
    if !known(name) {
        return Err(format!("unknown provider: {name} (one of: {})", KNOWN.join(" ")));
    }
    let mut list = listed();
    if list.iter().any(|p| p == name) {
        list.retain(|p| p != name);
    } else {
        list.push(name.into());
    }
    let mut text: String = list.iter().map(|p| format!("{p}\n")).collect();
    if text.is_empty() {
        text.push('\n');
    }
    write_atomic(&config_dir().join("providers"), text.as_bytes()).map_err(|e| e.to_string())?;
    Ok(list)
}

/// One provider's document. A reader that panics is reported, not fatal.
pub fn run(name: &str, min_interval: i64) -> Doc {
    let r = catch_unwind(AssertUnwindSafe(|| match name {
        "claude" => claude::run(min_interval),
        "codex" => codex::run(min_interval),
        _ => Doc::failed(name),
    }));
    r.unwrap_or_else(|_| Doc::failed(name))
}

/// Doctor lines (ok/PROBLEM) about a provider's credentials and last reply, never the token.
pub fn doctor(name: &str, pstatus: &str) {
    match name {
        "claude" => claude::doctor(pstatus),
        "codex" => codex::doctor(pstatus),
        _ => {}
    }
}

pub fn ok(msg: &str) {
    println!("  ok       {msg}");
}

pub fn bad(msg: &str) {
    println!("  PROBLEM  {msg}");
}

/// Drops every cache and backoff file, so the next run asks live (`pulse-limits reset`).
pub fn reset() {
    let dir = cache_dir();
    if let Ok(rd) = fs::read_dir(&dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if (name.starts_with("usage") && name.ends_with(".json")) || name.starts_with("backoff") {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}

/// The per-provider cache files and the shared plumbing: throttle, backoff, history, emit.
pub struct Store {
    pub name: String,
    pub now: i64,
    pub cache: PathBuf,
    pub last_reply: PathBuf,
    pub backoff: PathBuf,
    pub history: PathBuf,
    pub cache_age: i64,
    pub status: String,
    pub hint: String,
    pub source: String,
}

impl Store {
    pub fn new(name: &str) -> Store {
        let dir = cache_dir();
        let _ = fs::create_dir_all(&dir);
        // Installs from before the providers split kept Claude's files without a suffix: adopt them once.
        if name == "claude" {
            for (old, new) in [("usage.json", "usage-claude.json"), ("last-reply.json", "last-reply-claude.json"), ("backoff", "backoff-claude"), ("history.tsv", "history-claude.tsv")] {
                if dir.join(old).is_file() && !dir.join(new).exists() {
                    let _ = fs::rename(dir.join(old), dir.join(new));
                }
            }
        }
        let now = now();
        let cache = dir.join(format!("usage-{name}.json"));
        let cache_age = mtime(&cache).map(|m| now - m).unwrap_or(999_999);
        Store {
            name: name.into(),
            last_reply: dir.join(format!("last-reply-{name}.json")),
            backoff: dir.join(format!("backoff-{name}")),
            history: dir.join(format!("history-{name}.tsv")),
            now,
            cache,
            cache_age,
            status: String::new(),
            hint: String::new(),
            source: "CACHE".into(),
        }
    }

    pub fn backoff_until(&self) -> i64 {
        read_trimmed(&self.backoff).and_then(|s| s.parse().ok()).unwrap_or(0)
    }

    /// A live call is due when the cache is older than the interval and no backoff is running.
    pub fn due(&self, min_interval: i64) -> bool {
        self.cache_age > min_interval && self.now >= self.backoff_until()
    }

    pub fn backing_off(&self) -> bool {
        self.now < self.backoff_until()
    }

    pub fn set(&mut self, status: &str, hint: &str) {
        self.status = status.into();
        self.hint = hint.into();
    }

    /// One GET. Returns the HTTP code (0 = no answer) and the body, and keeps a copy in
    /// last-reply-<name>.json so `pulse-limits raw` can show a failure, not only a success.
    pub fn get(&self, url: &str, headers: &[(&str, &str)]) -> (u16, Vec<u8>) {
        let (code, body) = http_get(url, headers);
        let body_json = match serde_json::from_slice::<Value>(&body) {
            Ok(v) => serde_json::to_string(&v).unwrap_or_default(),
            Err(_) => match std::str::from_utf8(&body) {
                Ok(s) => serde_json::to_string(s).unwrap_or_default(),
                Err(_) => "\"\"".into(),
            },
        };
        let _ = write_atomic(&self.last_reply, format!("{{\"http\": {code}, \"at\": {}, \"body\": {body_json}}}\n", self.now).as_bytes());
        (code, body)
    }

    pub fn accept(&mut self, body: &[u8]) {
        let _ = write_atomic(&self.cache, body);
        self.source = "LIVE".into();
        self.cache_age = 0;
    }

    pub fn set_backoff(&self, secs: i64) {
        let _ = write_atomic(&self.backoff, format!("{}\n", self.now + secs).as_bytes());
    }

    pub fn cached(&self) -> Option<Value> {
        serde_json::from_slice(&fs::read(&self.cache).ok()?).ok()
    }

    /// Trend: one row per live reading (epoch, session %, week %), pruned to HISTORY_HOURS.
    fn history_add(&self, windows: &[Window]) {
        // the session column is what the trend strip draws: SESSION, or the first window of a provider without one
        let pct = |w: Option<&Window>| w.map(|w| round_half_up(w.pct_f())).unwrap_or(0);
        let session = windows.iter().find(|w| w.label == "SESSION").or(windows.first());
        let mut text = fs::read_to_string(&self.history).unwrap_or_default();
        text.push_str(&format!("{}\t{}\t{}\n", self.now, pct(session), pct(windows.iter().find(|w| w.label == "WEEK"))));
        let cut = self.now - HISTORY_HOURS * 3600;
        let kept: String = text
            .lines()
            .filter(|l| l.split('\t').next().and_then(|t| t.parse::<i64>().ok()).is_some_and(|t| t >= cut))
            .map(|l| format!("{l}\n"))
            .collect();
        let _ = write_atomic(&self.history, kept.as_bytes());
    }

    pub fn history_rows(&self) -> Vec<(i64, i64)> {
        fs::read_to_string(&self.history)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let mut f = l.split('\t');
                Some((f.next()?.parse().ok()?, f.next()?.parse().ok()?))
            })
            .collect()
    }

    /// The document, from status/hint/source and the windows read from the last good reply.
    pub fn emit(&mut self, plan: &str, windows: Vec<Window>, credits: Value) -> Doc {
        let fetched = if self.cache.is_file() {
            if self.cache_age == 0 { self.now } else { mtime(&self.cache).unwrap_or(self.now) }
        } else {
            self.source.clear();
            0
        };
        // nothing cached and a backoff running: say so instead of a bare NO DATA
        if self.status.is_empty() && !self.cache.is_file() && self.backing_off() {
            self.set("RATE LIMITED", "BACKING OFF AFTER A 429, RETRYING IN A FEW MINUTES");
        }
        if self.source == "LIVE" {
            self.history_add(&windows);
        }
        let empty = windows.is_empty();
        let status = if empty && self.status.is_empty() {
            if self.source.is_empty() { "NO DATA".to_string() } else { "NO LIMITS IN REPLY".to_string() }
        } else {
            self.status.clone()
        };
        let hint = if empty && self.hint.is_empty() && !self.source.is_empty() {
            "THE USAGE REPLY HAD NO WINDOWS. RUN: pulse-limits doctor".to_string()
        } else {
            self.hint.clone()
        };
        Doc { provider: self.name.clone(), plan: plan.into(), source: self.source.clone(), fetched, status, hint, windows, credits, history: self.history_rows() }
    }

    /// Doctor lines about the last attempt and the cached reply.
    pub fn doctor_digests(&self, digest: impl Fn(&Value) -> Value) {
        if let Some(v) = fs::read(&self.last_reply).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
            let body = v.get("body").cloned().unwrap_or(Value::Null);
            let body_digest = match &body {
                Value::Object(m) => json!({ "keys": m.keys().cloned().collect::<Vec<_>>().join(","), "error": body.get("error").cloned().unwrap_or(Value::Null) }),
                other => Value::String(other.to_string().chars().take(200).collect()),
            };
            let at = v.get("at").and_then(Value::as_i64).map(crate::util::iso_utc).unwrap_or_default();
            ok(&format!("last attempt: {}", json!({ "http": v.get("http"), "at": at, "body": body_digest })));
        }
        match self.cached() {
            Some(c) => ok(&format!("cached reply ({} min old): {}", self.cache_age / 60, digest(&c))),
            None if self.cache.is_file() => ok("cached reply: not JSON"),
            None => bad("no reply cached yet"),
        }
    }
}

/// One GET with a 15 s deadline. 0 means no answer (refused, timed out, no DNS, no TLS).
pub fn http_get(url: &str, headers: &[(&str, &str)]) -> (u16, Vec<u8>) {
    let cfg = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(15)))
        .user_agent("pulse-limits")
        .build();
    let agent = ureq::Agent::new_with_config(cfg);
    let mut req = agent.get(url);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    match req.call() {
        Ok(mut resp) => {
            let code = resp.status().as_u16();
            let body = resp.body_mut().read_to_vec().unwrap_or_default();
            (code, body)
        }
        Err(_) => (0, vec![]),
    }
}

#[cfg(test)]
pub mod testing {
    //! A one-shot HTTP server on 127.0.0.1 for the provider tests: no network, no Python.
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    /// Serves `body` with `code` to the next request and returns the base URL; `refused` gives
    /// a URL nothing listens on.
    pub fn serve(code: u16, body: &'static str) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        thread::spawn(move || {
            if let Ok((mut s, _)) = l.accept() {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let _ = write!(s, "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        url
    }

    pub fn refused() -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        drop(l);
        url
    }

    /// Serial tests that share the process environment take this.
    pub static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A scratch XDG cache/config for one test (removed when dropped).
    pub struct Scratch(pub std::path::PathBuf);

    impl Scratch {
        pub fn new(tag: &str) -> Scratch {
            let d = std::env::temp_dir().join(format!("pl-rust-test-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(d.join("cache").join("pulse-limits")).unwrap();
            std::fs::create_dir_all(d.join("config")).unwrap();
            std::env::set_var("XDG_CACHE_HOME", d.join("cache"));
            std::env::set_var("XDG_CONFIG_HOME", d.join("config"));
            Scratch(d)
        }

        pub fn cache(&self) -> std::path::PathBuf {
            self.0.join("cache").join("pulse-limits")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_maps_transport_and_status() {
        let (code, body) = http_get(&format!("{}/x", testing::serve(429, "{\"detail\":\"slow down\"}")), &[]);
        assert_eq!(code, 429);
        assert_eq!(body, b"{\"detail\":\"slow down\"}");
        let (code, _) = http_get(&format!("{}/x", testing::refused()), &[("Authorization", "Bearer x")]);
        assert_eq!(code, 0);
    }

    #[test]
    fn store_files_and_emit() {
        let _g = testing::ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = testing::Scratch::new("store");
        // legacy adoption
        fs::write(s.cache().join("usage.json"), b"{\"five_hour\":{\"utilization\":1}}").unwrap();
        fs::write(s.cache().join("history.tsv"), b"1\t2\t3\n").unwrap();
        let mut st = Store::new("claude");
        assert!(s.cache().join("usage-claude.json").is_file());
        assert!(!s.cache().join("usage.json").exists());
        assert_eq!(st.history_rows(), vec![(1, 2)]);
        assert!(!st.due(270)); // just written: the cache is fresh
        st.cache_age = 1000;
        assert!(st.due(270));
        st.set_backoff(180);
        assert!(!st.due(270));
        assert!(st.backing_off());
        // no windows, cached reply -> NO LIMITS IN REPLY
        let d = st.emit("P", vec![], Value::Null);
        assert_eq!((d.status.as_str(), d.hint.as_str(), d.source.as_str()), ("NO LIMITS IN REPLY", "THE USAGE REPLY HAD NO WINDOWS. RUN: pulse-limits doctor", "CACHE"));
        // a live accept writes the trend and prunes the old row
        let w = vec![Window { label: "SESSION".into(), pct: json!(12.6), resets: None }, Window { label: "WEEK".into(), pct: json!(3), resets: None }];
        st.accept(b"{}");
        let d = st.emit("P", w, Value::Null);
        assert_eq!(d.source, "LIVE");
        assert_eq!(d.fetched, st.now);
        assert_eq!(d.history, vec![(st.now, 13)]);
        assert_eq!(fs::read_to_string(&st.history).unwrap(), format!("{}\t13\t3\n", st.now));
        // nothing cached and backing off
        fs::remove_file(&st.cache).unwrap();
        let mut st = Store::new("codex");
        st.set_backoff(180);
        let d = st.emit("", vec![], Value::Null);
        assert_eq!((d.status.as_str(), d.source.as_str(), d.fetched), ("RATE LIMITED", "", 0));
        let mut st = Store::new("codex");
        fs::remove_file(&st.backoff).unwrap();
        let d = st.emit("", vec![], Value::Null);
        assert_eq!((d.status.as_str(), d.hint.as_str()), ("NO DATA", ""));
    }

    #[test]
    fn last_reply_shapes() {
        let _g = testing::ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = testing::Scratch::new("reply");
        let st = Store::new("codex");
        st.get(&format!("{}/x", testing::serve(200, "{\"a\": 1}")), &[]);
        assert_eq!(fs::read_to_string(&st.last_reply).unwrap(), format!("{{\"http\": 200, \"at\": {}, \"body\": {{\"a\":1}}}}\n", st.now));
        st.get(&format!("{}/x", testing::serve(502, "<html>bad gateway</html>")), &[]);
        assert_eq!(fs::read_to_string(&st.last_reply).unwrap(), format!("{{\"http\": 502, \"at\": {}, \"body\": \"<html>bad gateway</html>\"}}\n", st.now));
        drop(s);
    }

    #[test]
    fn providers_list_and_toggle() {
        let _g = testing::ENV.lock().unwrap_or_else(|e| e.into_inner());
        let _s = testing::Scratch::new("toggle");
        assert_eq!(enabled(), vec!["claude"]);
        assert_eq!(toggle("codex").unwrap(), vec!["claude", "codex"]);
        assert_eq!(enabled(), vec!["claude", "codex"]);
        assert_eq!(toggle("claude").unwrap(), vec!["codex"]);
        assert_eq!(toggle("codex").unwrap(), Vec::<String>::new());
        assert_eq!(fs::read_to_string(config_dir().join("providers")).unwrap(), "\n");
        assert!(enabled().is_empty());
        assert!(toggle("gemini").is_err());
    }
}
