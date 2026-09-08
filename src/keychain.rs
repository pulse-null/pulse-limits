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
    use crate::providers::testing::{Scratch, ENV};
    use crate::util::testing::{calls, fake_bin, pretend, Vars};
    use std::fs;
    use std::path::Path;

    const DUMP: &str = r#"keychain: "/Users/x/Library/Keychains/login.keychain-db"
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

    /// A `security` that dumps `dir/dump.txt` and answers `find-generic-password -s S [-a A] -w`
    /// from `dir/secrets/S[@A]`; anything else fails.
    fn fake_security(dir: &Path) -> PathBuf {
        fake_bin(
            dir,
            "security",
            &format!(
                r#"case "$1" in
  dump-keychain) cat '{d}/dump.txt' ;;
  find-generic-password) shift; svc=; acct=
    while [ $# -gt 0 ]; do case "$1" in -s) svc="$2"; shift ;; -a) acct="$2"; shift ;; esac; shift; done
    f='{d}/secrets/'"$svc${{acct:+@$acct}}"
    [ -f "$f" ] || exit 44
    cat "$f" ;;
  *) exit 1 ;;
esac"#,
                d = dir.display()
            ),
        )
    }

    fn token(v: &Value) -> String {
        v["claudeAiOauth"]["accessToken"].as_str().unwrap_or("").to_string()
    }

    fn login(t: &str) -> String {
        format!("{{\"claudeAiOauth\":{{\"accessToken\":\"{t}\"}}}}")
    }

    #[test]
    fn dump_parsing() {
        let items = parse_dump(DUMP);
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
        assert_eq!(parse("[1]"), None, "a document is an object");
        assert_eq!(parse("{"), None);
        assert_eq!(dedup(vec![Item { service: String::new(), account: "x".into(), modified: String::new() }]), vec![]);
    }

    #[test]
    fn keychain_commands() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("keychain-cmd");
        let bin = s.0.join("bin");
        let mut vars = Vars::default();
        vars.set("PATH", &bin).set("HOME", &s.0);
        {
            let _linux = pretend(false);
            assert_eq!(dump(), vec![]);
            assert_eq!(read_item(SERVICE, ""), None);
        }
        let _mac = pretend(true);
        assert_eq!(dump(), vec![], "no security on PATH");
        let log = fake_security(&bin);
        fs::write(bin.join("dump.txt"), DUMP).unwrap();
        assert_eq!(dump().len(), 2);
        fs::create_dir_all(bin.join("secrets")).unwrap();
        fs::write(bin.join("secrets").join("Claude Code-credentials@user"), format!("{}\r\n", login("t"))).unwrap();
        assert_eq!(read_item(SERVICE, "user"), Some(login("t")));
        assert_eq!(read_item(SERVICE, ""), None, "no such item");
        assert_eq!(
            calls(&log),
            vec!["dump-keychain", "find-generic-password -s Claude Code-credentials -a user -w", "find-generic-password -s Claude Code-credentials -w"]
        );
    }

    #[test]
    fn login_search_order() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("keychain-login");
        let bin = s.0.join("bin");
        let mut vars = Vars::default();
        vars.set("PATH", &bin).set("HOME", &s.0).unset("CLAUDE_CONFIG_DIR");
        let _mac = pretend(true);
        let log = fake_security(&bin);
        fs::write(bin.join("dump.txt"), DUMP).unwrap();
        let secrets = bin.join("secrets");
        fs::create_dir_all(&secrets).unwrap();
        fs::create_dir_all(config_dir()).unwrap();
        assert_eq!(pin(), None);
        fs::write(config_dir().join("keychain"), "\n").unwrap();
        assert_eq!(pin(), None);
        fs::write(config_dir().join("keychain"), "Pinned\n").unwrap();
        assert_eq!(pin(), Some("Pinned".into()));
        let names = |items: Vec<Item>| items.iter().map(|i| format!("{}@{}", i.service, i.account)).collect::<Vec<_>>();
        assert_eq!(names(candidates()), vec!["Pinned@", "Claude Code-credentials@", "Claude Code-credentials@user", "Claude Code-credentials-work@work"]);
        assert_eq!(find_login(), None, "nothing readable anywhere");
        // the pinned item is unreadable, the default has no login, a scanned one is not JSON, the last has it
        fs::write(secrets.join("Claude Code-credentials"), login("")).unwrap();
        fs::write(secrets.join("Claude Code-credentials@user"), "not json").unwrap();
        fs::write(secrets.join("Claude Code-credentials-work@work"), login("work")).unwrap();
        assert_eq!(token(&find_login().unwrap()), "work");
        let all = calls(&log);
        assert_eq!(
            all[all.len() - 5..].to_vec(),
            vec![
                "dump-keychain",
                "find-generic-password -s Pinned -w",
                "find-generic-password -s Claude Code-credentials -w",
                "find-generic-password -s Claude Code-credentials -a user -w",
                "find-generic-password -s Claude Code-credentials-work -a work -w",
            ]
        );
        // the pin wins once it reads
        fs::write(secrets.join("Pinned"), login("pinned")).unwrap();
        assert_eq!(token(&find_login().unwrap()), "pinned");
        // a pin equal to the default name is listed once
        fs::write(config_dir().join("keychain"), format!("{SERVICE}\n")).unwrap();
        assert_eq!(names(candidates()).len(), 3);
        // the files come after the Keychain: the configured folder first, then ~/.claude
        fs::remove_dir_all(&secrets).unwrap();
        fs::create_dir_all(&secrets).unwrap();
        fs::write(bin.join("dump.txt"), "").unwrap();
        assert_eq!(find_login(), None);
        let cfg = s.0.join("cfg");
        fs::create_dir_all(&cfg).unwrap();
        fs::create_dir_all(s.0.join(".claude")).unwrap();
        let default = s.0.join(".claude").join(".credentials.json");
        assert_eq!(credential_files(), vec![default.clone()]);
        fs::write(&default, login("home")).unwrap();
        assert_eq!(token(&find_login().unwrap()), "home");
        vars.set("CLAUDE_CONFIG_DIR", &cfg);
        assert_eq!(credential_files(), vec![cfg.join(".credentials.json"), default.clone()]);
        assert_eq!(token(&find_login().unwrap()), "home", "no file in the configured folder yet");
        fs::write(cfg.join(".credentials.json"), login("")).unwrap();
        assert_eq!(token(&find_login().unwrap()), "home", "a configured file without a login falls through");
        fs::write(cfg.join(".credentials.json"), login("cfg")).unwrap();
        assert_eq!(token(&find_login().unwrap()), "cfg");
        vars.set("CLAUDE_CONFIG_DIR", s.0.join(".claude"));
        assert_eq!(credential_files(), vec![default.clone()], "the configured folder is the default one");
        vars.set("CLAUDE_CONFIG_DIR", "");
        assert_eq!(credential_files(), vec![default], "empty is unset");
        // Linux: the Keychain is never asked
        drop(_mac);
        let _linux = pretend(false);
        let before = calls(&log).len();
        assert_eq!(token(&find_login().unwrap()), "home");
        assert_eq!(calls(&log).len(), before);
    }
}
