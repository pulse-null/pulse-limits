//! Where the claude.ai login lives. macOS: the Keychain item Claude Code writes, keyed to
//! CLAUDE_CONFIG_DIR, so a Mac can hold several "Claude Code-credentials…" entries and only
//! some carry a claude.ai login; SwiftBar does not see shell variables, so we scan for them and
//! a pinned service name (`pulse-limits keychain NAME`) wins. Linux, and the fallback on macOS:
//! `${CLAUDE_CONFIG_DIR:-~/.claude}/.credentials.json`, the file Claude Code writes there.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::util::{config_dir, env_path, home, is_macos, read_trimmed};

pub const SERVICE: &str = "Claude Code-credentials";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub service: String,
    pub account: String,
    pub modified: String, // "20260908100000Z" or empty
}

/// The pinned service name, if any.
pub fn pin() -> Option<String> {
    read_trimmed(&config_dir().join("keychain")).filter(|s| !s.is_empty())
}

pub fn has_login(v: &Value) -> bool {
    v.get("claudeAiOauth").and_then(|o| o.get("accessToken")).and_then(Value::as_str).is_some_and(|t| !t.is_empty())
}

/// Every Keychain item whose service or label mentions Claude, from `security dump-keychain`.
pub fn dump() -> Vec<Item> {
    if !is_macos() {
        return vec![];
    }
    let out = Command::new("security").arg("dump-keychain").stdin(Stdio::null()).stderr(Stdio::null()).output();
    let text = match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(_) => return vec![],
    };
    parse_dump(&text)
}

/// The awk of the bash plugin: attributes come alphabetically, so svce closes each item.
pub fn parse_dump(text: &str) -> Vec<Item> {
    let mut items = vec![];
    let (mut acct, mut labl, mut mdat) = (String::new(), String::new(), String::new());
    let quoted = |line: &str| line.split('"').nth(3).unwrap_or("").to_string();
    for line in text.lines() {
        if line.starts_with("keychain:") {
            acct.clear();
            labl.clear();
            mdat.clear();
        } else if line.contains("\"acct\"<blob>=") {
            acct = quoted(line);
        } else if line.contains("\"labl\"<blob>=") {
            labl = quoted(line);
        } else if line.contains("\"mdat\"<timedate>=") {
            mdat = quoted(line).chars().take(15).collect();
        } else if line.contains("\"svce\"<blob>=") {
            let svc = quoted(line);
            if svc.to_lowercase().contains("claude") || labl.to_lowercase().contains("claude") {
                items.push(Item { service: svc, account: acct.clone(), modified: mdat.clone() });
            }
        }
    }
    items
}

/// The item's secret (account may be empty: first match).
pub fn read_item(service: &str, account: &str) -> Option<String> {
    if !is_macos() {
        return None;
    }
    let mut cmd = Command::new("security");
    cmd.args(["find-generic-password", "-s", service]);
    if !account.is_empty() {
        cmd.args(["-a", account]);
    }
    let out = cmd.arg("-w").stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim_end_matches(['\n', '\r']).to_string())
}

/// Pinned first, then the default name (no dump needed when it works), then the scan; no repeats.
pub fn candidates() -> Vec<Item> {
    let mut list = vec![];
    if let Some(p) = pin() {
        list.push(Item { service: p, account: String::new(), modified: String::new() });
    }
    list.push(Item { service: SERVICE.into(), account: String::new(), modified: String::new() });
    list.extend(dump());
    dedup(list)
}

pub fn dedup(list: Vec<Item>) -> Vec<Item> {
    let mut seen = std::collections::HashSet::new();
    list.into_iter().filter(|i| !i.service.is_empty() && seen.insert((i.service.clone(), i.account.clone()))).collect()
}

/// Claude Code's credentials files: the configured dir first, then the default; no repeats.
pub fn credential_files() -> Vec<PathBuf> {
    let mut files = vec![];
    if let Some(d) = env_path("CLAUDE_CONFIG_DIR") {
        files.push(d.join(".credentials.json"));
    }
    let default = home().join(".claude").join(".credentials.json");
    if !files.contains(&default) {
        files.push(default);
    }
    files
}

pub fn parse(json: &str) -> Option<Value> {
    serde_json::from_str::<Value>(json).ok().filter(Value::is_object)
}

/// The first credentials document with a claude.ai login: Keychain items, then the files.
pub fn find_login() -> Option<Value> {
    for item in candidates() {
        if let Some(v) = read_item(&item.service, &item.account).and_then(|s| parse(&s)) {
            if has_login(&v) {
                return Some(v);
            }
        }
    }
    for f in credential_files() {
        if let Some(v) = std::fs::read_to_string(&f).ok().and_then(|s| parse(&s)) {
            if has_login(&v) {
                return Some(v);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_parsing() {
        let text = r#"keychain: "/Users/x/Library/Keychains/login.keychain-db"
version: 512
class: "genp"
attributes:
    0x00000007 <blob>="Claude Code-credentials"
    "acct"<blob>="user"
    "labl"<blob>="Claude Code-credentials"
    "mdat"<timedate>=0x32303236303930383130303030305A00  "20260908100000Z\000"
    "svce"<blob>="Claude Code-credentials"
keychain: "/Users/x/Library/Keychains/login.keychain-db"
class: "genp"
attributes:
    "acct"<blob>=<NULL>
    "labl"<blob>="Something else"
    "svce"<blob>="other-service"
keychain: "/Users/x/Library/Keychains/login.keychain-db"
class: "genp"
attributes:
    "acct"<blob>="work"
    "labl"<blob>="claude work"
    "svce"<blob>="Claude Code-credentials-work"
"#;
        let items = parse_dump(text);
        assert_eq!(
            items,
            vec![
                Item { service: "Claude Code-credentials".into(), account: "user".into(), modified: "20260908100000Z".into() },
                Item { service: "Claude Code-credentials-work".into(), account: "work".into(), modified: String::new() },
            ]
        );
        assert!(has_login(&serde_json::json!({"claudeAiOauth": {"accessToken": "t"}})));
        assert!(!has_login(&serde_json::json!({"claudeAiOauth": {"accessToken": ""}})));
        assert!(!has_login(&serde_json::json!({"other": 1})));
    }
}
