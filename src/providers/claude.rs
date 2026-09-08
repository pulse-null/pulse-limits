//! Claude: the claude.ai login Claude Code keeps (see keychain.rs) and Anthropic's
//! (undocumented) usage endpoint, the call behind /usage in Claude Code. The only thing that
//! leaves this machine is one GET to api.anthropic.com.

use serde_json::{json, Value};

use crate::keychain;
use crate::providers::{bad, ok, say, Doc, Store, Window, BACKOFF_SECS, PROBE_DELAY_SECS};
use crate::util::{env_path, hhmmss, is_macos, iso_utc, local_offset, upper};

pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// The endpoint, or the one PULSE_CLAUDE_USAGE_URL names (the tests' local server).
fn usage_url() -> String {
    match std::env::var("PULSE_CLAUDE_USAGE_URL") {
        Ok(u) if !u.trim().is_empty() => u.trim().to_string(),
        _ => USAGE_URL.into(),
    }
}

pub struct Login {
    pub token: String,
    pub plan: String, // subscriptionType
    pub tier: String, // rateLimitTier, e.g. default_claude_max_20x
}

fn str_or<'a>(v: Option<&'a Value>, d: &'a str) -> &'a str {
    v.and_then(Value::as_str).unwrap_or(d)
}

pub fn login_of(creds: &Value) -> Login {
    let o = creds.get("claudeAiOauth");
    Login {
        token: str_or(o.and_then(|o| o.get("accessToken")), "").to_string(),
        plan: str_or(o.and_then(|o| o.get("subscriptionType")), "?").to_string(),
        tier: str_or(o.and_then(|o| o.get("rateLimitTier")), "?").to_string(),
    }
}

/// "MAX 20X" from the tier, else the plan name; "?" becomes nothing.
pub fn plan_label(login: Option<&Login>) -> String {
    let (plan, tier) = login.map(|l| (l.plan.as_str(), l.tier.as_str())).unwrap_or(("?", "?"));
    if tier == "?" {
        if plan == "?" {
            String::new()
        } else {
            upper(plan)
        }
    } else {
        upper(&tier.strip_prefix("default_claude_").unwrap_or(tier).replace('_', " "))
    }
}

/// jq's `//`: a value that is neither missing, null nor false.
fn present(v: Option<&Value>) -> Option<&Value> {
    v.filter(|v| !v.is_null() && *v != &Value::Bool(false))
}

fn string(v: Option<&Value>) -> Option<String> {
    present(v).and_then(Value::as_str).map(str::to_string)
}

/// Newer replies carry a limits[] array (kind: session / weekly_all / weekly_scoped); older
/// ones the five_hour / seven_day blocks. Read whichever is there, limits[] first.
pub fn windows(c: &Value) -> Vec<Window> {
    let limits: Vec<&Value> = c.get("limits").and_then(Value::as_array).map(|a| a.iter().filter(|l| l.is_object()).collect()).unwrap_or_default();
    let lim = |k: &str| limits.iter().find(|l| l.get("kind").and_then(Value::as_str) == Some(k)).copied();
    let win = |name: &str, k: &str, legacy: Option<&Value>| -> Option<Window> {
        let legacy = present(legacy).filter(|v| v.is_object());
        if let Some(l) = lim(k) {
            let pct = present(l.get("percent")).or_else(|| legacy.and_then(|g| present(g.get("utilization")))).cloned().unwrap_or(json!(0));
            let resets = string(l.get("resets_at")).or_else(|| legacy.and_then(|g| string(g.get("resets_at"))));
            Some(Window { label: name.into(), pct, resets })
        } else {
            legacy.map(|g| Window { label: name.into(), pct: present(g.get("utilization")).cloned().unwrap_or(json!(0)), resets: string(g.get("resets_at")) })
        }
    };
    let mut out: Vec<Window> = [win("SESSION", "session", c.get("five_hour")), win("WEEK", "weekly_all", c.get("seven_day"))].into_iter().flatten().collect();
    for l in limits.iter().filter(|l| l.get("kind").and_then(Value::as_str) == Some("weekly_scoped")) {
        let scope = l.get("scope");
        let label = string(scope.and_then(|s| s.get("model")).and_then(|m| m.get("display_name")))
            .or_else(|| string(scope.and_then(|s| s.get("surface"))))
            .unwrap_or_else(|| "SCOPED".into());
        out.push(Window { label: upper(&label), pct: present(l.get("percent")).cloned().unwrap_or(json!(0)), resets: string(l.get("resets_at")) });
    }
    out
}

/// Extra-usage spend, shown as CREDITS when enabled and non-zero.
pub fn credits(c: &Value) -> Value {
    let e = c.get("extra_usage");
    let enabled = e.and_then(|e| e.get("is_enabled")) == Some(&Value::Bool(true));
    let used = e.and_then(|e| present(e.get("used_credits")));
    match (enabled, used) {
        (true, Some(u)) if u.as_f64().unwrap_or(0.0) > 0.0 => json!({ "used": u, "currency": string(e.and_then(|e| e.get("currency"))).unwrap_or_default() }),
        _ => Value::Null,
    }
}

/// `(.five_hour != null) or ((.limits // []) | length > 0)`
pub fn reply_ok(v: &Value) -> bool {
    if v.get("five_hour").is_some_and(|f| !f.is_null()) {
        return true;
    }
    match present(v.get("limits")) {
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0).abs() > 0.0,
        _ => false,
    }
}

/// One live call and what it means.
pub fn fetch(st: &mut Store, url: &str, token: &str) {
    let auth = format!("Bearer {token}");
    let (code, body) = st.get(url, &[("Authorization", &auth), ("anthropic-beta", "oauth-2025-04-20")]);
    match code {
        200 => {
            if serde_json::from_slice::<Value>(&body).ok().is_some_and(|v| reply_ok(&v)) {
                st.accept(&body);
            } else {
                st.set("BAD RESPONSE", "THE USAGE ENDPOINT CHANGED SHAPE");
            }
        }
        401 => st.set("TOKEN EXPIRED", "OPEN CLAUDE CODE ONCE, IT REFRESHES THE TOKEN"),
        403 => st.set("NO PLAN ACCESS", "LOG IN TO CLAUDE CODE WITH A CLAUDE.AI PLAN, NOT AN API KEY"),
        404 => st.set("NO USAGE DATA", "THIS ACCOUNT HAS NO PLAN LIMITS TO SHOW"),
        429 => {
            st.set_backoff(BACKOFF_SECS);
            if st.cache_age > 300 {
                st.set("RATE LIMITED", "TOO MANY USAGE CALLS FOR THIS ACCOUNT, RETRYING IN 3 MIN");
            }
        }
        0 => st.set("NETWORK", "COULD NOT REACH API.ANTHROPIC.COM"),
        c => st.set(&format!("HTTP {c}"), "UNEXPECTED ANSWER FROM API.ANTHROPIC.COM"),
    }
}

/// The document from the cache and, when due, one live call.
pub fn run(min_interval: i64) -> Doc {
    let mut st = Store::new("claude");
    let login = keychain::find_login().map(|c| login_of(&c));
    let token = login.as_ref().map(|l| l.token.clone()).unwrap_or_default();
    if token.is_empty() {
        st.set("NO LOGIN", "NO CLAUDE.AI LOGIN FOUND. RUN: pulse-limits doctor");
    }
    let label = plan_label(login.as_ref());
    if !token.is_empty() && st.due(min_interval) {
        fetch(&mut st, &usage_url(), &token);
    }
    let (windows, credits) = st.cached().map(|c| (windows(&c), credits(&c))).unwrap_or((vec![], Value::Null));
    st.emit(&label, windows, credits)
}

/// Every candidate login and the last reply, never the token. `pstatus` is what the plugin
/// run just reported for this provider.
pub fn doctor(pstatus: &str) {
    let st = Store::new("claude");
    let bar = if is_macos() { "SwiftBar" } else { "the bar" };
    if let Some(d) = env_path("CLAUDE_CONFIG_DIR") {
        ok(&format!("CLAUDE_CONFIG_DIR={} in this shell ({bar} does not see shell variables)", d.display()));
    }
    let mut tok = String::new();
    if is_macos() {
        let pin = keychain::pin();
        if let Some(p) = &pin {
            ok(&format!("pinned Keychain service: {p}"));
        }
        let mut items = vec![];
        if let Some(p) = pin {
            items.push(keychain::Item { service: p, account: String::new(), modified: String::new() });
        }
        let dump = keychain::dump();
        if dump.is_empty() {
            items.push(keychain::Item { service: keychain::SERVICE.into(), account: String::new(), modified: String::new() });
        } else {
            items.extend(dump);
        }
        let items = keychain::dedup(items);
        for it in &items {
            let mdat = if it.modified.is_empty() { String::new() } else { format!(" (modified {})", it.modified) };
            match keychain::read_item(&it.service, &it.account).filter(|s| !s.is_empty()) {
                Some(json) => match keychain::parse(&json) {
                    Some(v) if keychain::has_login(&v) => {
                        let o = &v["claudeAiOauth"];
                        let expires = o.get("expiresAt").and_then(Value::as_f64).map(|ms| iso_utc((ms / 1000.0) as i64)).unwrap_or_else(|| "?".into());
                        ok(&format!(
                            "Keychain '{}' / account '{}'{mdat}: claude.ai login, plan {}, tier {}, expires {expires}",
                            it.service,
                            it.account,
                            str_or(o.get("subscriptionType"), "?"),
                            str_or(o.get("rateLimitTier"), "?")
                        ));
                        if tok.is_empty() {
                            tok = o["accessToken"].as_str().unwrap_or("").to_string();
                        }
                    }
                    Some(v) => {
                        let keys = v.as_object().map(|m| m.keys().cloned().collect::<Vec<_>>().join(",")).unwrap_or_default();
                        bad(&format!("Keychain '{}' / account '{}'{mdat}: no claude.ai login in it (keys: {keys})", it.service, it.account));
                    }
                    None => bad(&format!(
                        "Keychain '{}' / account '{}'{mdat}: no claude.ai login in it (keys: not JSON, {} chars)",
                        it.service,
                        it.account,
                        json.chars().count()
                    )),
                },
                None => bad(&format!("Keychain '{}' / account '{}': listed but not readable from here", it.service, it.account)),
            }
        }
        ok(&format!("{} Keychain item(s) mention Claude", items.len()));
    }
    for f in keychain::credential_files() {
        if !f.is_file() {
            continue;
        }
        match std::fs::read_to_string(&f).ok().and_then(|s| keychain::parse(&s)) {
            Some(v) if keychain::has_login(&v) => {
                ok(&format!("file {}: claude.ai login", f.display()));
                if tok.is_empty() {
                    tok = v["claudeAiOauth"]["accessToken"].as_str().unwrap_or("").to_string();
                }
            }
            _ => bad(&format!("file {}: no claude.ai login in it", f.display())),
        }
    }
    if tok.is_empty() {
        bad(&format!("no claude.ai login found anywhere on this {}.", if is_macos() { "Mac" } else { "machine" }));
        say("           In Claude Code run /status: it names the login method and, if set, the config dir.");
        if is_macos() {
            say("           If Claude Code uses CLAUDE_CONFIG_DIR, its Keychain entry has a different name; list them with:");
            say("             security dump-keychain | grep -o '\"Claude Code-credentials[^\"]*\"' | sort -u");
            say("           and pin the right one:  pulse-limits keychain 'Claude Code-credentials-...'");
        } else {
            let d = env_path("CLAUDE_CONFIG_DIR").unwrap_or_else(|| crate::util::home().join(".claude"));
            say(&format!("           On Linux Claude Code writes {}/.credentials.json when you log in", d.display()));
            say("           with a claude.ai account: run 'claude' once. An API-key login writes no usable token.");
        }
    }
    say("  usage api");
    if tok.is_empty() {
        bad("skipped (no token)");
    } else if pstatus.is_empty() {
        ok("reached through the plugin (not probed again: the endpoint has a small per-account quota)");
    } else if st.backing_off() {
        ok(&format!("not probed: backing off after a 429 until {}", hhmmss(st.backoff_until(), local_offset())));
    } else {
        std::thread::sleep(std::time::Duration::from_secs(PROBE_DELAY_SECS));
        let auth = format!("Bearer {tok}");
        let (code, body) = crate::providers::http_get(&usage_url(), &[("Authorization", &auth), ("anthropic-beta", "oauth-2025-04-20")]);
        let text = String::from_utf8_lossy(&body).replace('\n', " ");
        match code {
            200 => match serde_json::from_slice::<Value>(&body) {
                Ok(v) => ok(&format!("HTTP 200: {}", json!({ "five_hour": v["five_hour"]["utilization"], "seven_day": v["seven_day"]["utilization"] }))),
                Err(_) => ok("HTTP 200: unexpected JSON shape"),
            },
            429 => bad("HTTP 429 rate limited: the account made too many usage calls recently (another machine with the panel open counts). It recovers by itself; wait a few minutes."),
            0 => bad("no answer from api.anthropic.com"),
            c => bad(&format!("HTTP {c}: {}", text.chars().take(240).collect::<String>())),
        }
    }
    say("  last reply (shape digest)");
    st.doctor_digests(|c| {
        let limits: Vec<Value> = c
            .get("limits")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|l| json!({ "kind": l["kind"], "percent": l["percent"], "model": l["scope"]["model"]["display_name"] })).collect())
            .unwrap_or_default();
        json!({ "keys": c.as_object().map(|m| m.keys().cloned().collect::<Vec<_>>().join(",")).unwrap_or_default(),
                "five_hour": c["five_hour"]["utilization"], "seven_day": c["seven_day"]["utilization"], "limits": limits })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::testing::{capture, refused, serve, Sandbox, Scratch, ENV};

    const LEGACY: &str = r#"{"five_hour":{"utilization":28.0,"resets_at":"2026-09-08T13:40:00.300893+00:00"},
        "seven_day":{"utilization":12.0,"resets_at":"2026-09-09T06:00:00.300975+00:00"},
        "extra_usage":{"is_enabled":true,"used_credits":1.5,"currency":"EUR"}}"#;
    const LIMITS: &str = r#"{"five_hour":null,"seven_day":null,
        "limits":[{"kind":"session","percent":13,"resets_at":"2026-09-08T18:30:00Z","scope":null},
                  {"kind":"weekly_all","percent":17,"resets_at":"2026-09-09T06:00:00Z"},
                  {"kind":"weekly_scoped","percent":30,"resets_at":"2026-09-09T06:00:00Z","scope":{"model":{"id":null,"display_name":"Fable"},"surface":null}},
                  {"kind":"weekly_scoped","percent":2,"resets_at":null,"scope":{"model":null,"surface":"cowork"}},
                  {"kind":"weekly_scoped","percent":null,"scope":null}],
        "extra_usage":{"is_enabled":true,"used_credits":0.0,"currency":"EUR"}}"#;
    const BOTH: &str = r#"{"five_hour":{"utilization":28.0,"resets_at":"2026-09-08T13:40:00Z"},
        "limits":[{"kind":"session","percent":null,"resets_at":null}]}"#;

    fn v(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn legacy_blocks() {
        let w = windows(&v(LEGACY));
        assert_eq!(w.len(), 2);
        assert_eq!((w[0].label.as_str(), w[0].pct.to_string().as_str(), w[0].resets.as_deref()), ("SESSION", "28.0", Some("2026-09-08T13:40:00.300893+00:00")));
        assert_eq!((w[1].label.as_str(), w[1].pct.to_string().as_str()), ("WEEK", "12.0"));
        assert_eq!(credits(&v(LEGACY)), json!({ "used": 1.5, "currency": "EUR" }));
        assert!(reply_ok(&v(LEGACY)));
    }

    #[test]
    fn limits_only() {
        let w = windows(&v(LIMITS));
        let got: Vec<(String, String, Option<String>)> = w.iter().map(|w| (w.label.clone(), w.pct.to_string(), w.resets.clone())).collect();
        assert_eq!(
            got,
            vec![
                ("SESSION".into(), "13".into(), Some("2026-09-08T18:30:00Z".into())),
                ("WEEK".into(), "17".into(), Some("2026-09-09T06:00:00Z".into())),
                ("FABLE".into(), "30".into(), Some("2026-09-09T06:00:00Z".into())),
                ("COWORK".into(), "2".into(), None),
                ("SCOPED".into(), "0".into(), None),
            ]
        );
        assert_eq!(credits(&v(LIMITS)), Value::Null); // enabled but nothing used
        assert!(reply_ok(&v(LIMITS)));
        // limits[] wins, its nulls fall back to the legacy block
        let w = windows(&v(BOTH));
        assert_eq!((w[0].pct.to_string().as_str(), w[0].resets.as_deref()), ("28.0", Some("2026-09-08T13:40:00Z")));
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn unusable_replies() {
        assert!(windows(&v("{}")).is_empty());
        assert!(!reply_ok(&v("{}")));
        assert!(!reply_ok(&v(r#"{"five_hour":null,"limits":[]}"#)));
        assert!(!reply_ok(&v(r#"{"error":{"type":"not_found"}}"#)));
        assert!(windows(&v(r#"{"limits":"nope"}"#)).is_empty());
        assert_eq!(credits(&v(r#"{"extra_usage":null}"#)), Value::Null);
        assert_eq!(credits(&v(r#"{"extra_usage":{"is_enabled":false,"used_credits":3}}"#)), Value::Null);
    }

    #[test]
    fn plan_labels() {
        let l = |plan: &str, tier: &str| Login { token: "t".into(), plan: plan.into(), tier: tier.into() };
        assert_eq!(plan_label(Some(&l("max", "default_claude_max_20x"))), "MAX 20X");
        assert_eq!(plan_label(Some(&l("pro", "?"))), "PRO");
        assert_eq!(plan_label(Some(&l("?", "?"))), "");
        assert_eq!(plan_label(None), "");
        let lg = login_of(&v(r#"{"claudeAiOauth":{"accessToken":"abc","subscriptionType":"max","rateLimitTier":"default_claude_max_5x"}}"#));
        assert_eq!((lg.token.as_str(), lg.plan.as_str(), lg.tier.as_str()), ("abc", "max", "default_claude_max_5x"));
    }

    #[test]
    fn http_paths() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("claude-http");
        let mut st = Store::new("claude");
        fetch(&mut st, &serve(200, LIMITS), "tok");
        assert_eq!((st.status.as_str(), st.source.as_str(), st.cache_age), ("", "LIVE", 0));
        assert!(st.cache.is_file());
        let d = st.emit("MAX 20X", windows(&st.cached().unwrap()), Value::Null);
        assert_eq!(d.windows.len(), 5);
        assert_eq!(d.history.len(), 1);

        let mut st = Store::new("claude");
        st.cache_age = 1000;
        fetch(&mut st, &serve(401, "{\"error\":\"x\"}"), "tok");
        assert_eq!((st.status.as_str(), st.hint.as_str()), ("TOKEN EXPIRED", "OPEN CLAUDE CODE ONCE, IT REFRESHES THE TOKEN"));
        assert!(st.cached().is_some(), "a failed call keeps the last good reply");

        let mut st = Store::new("claude");
        st.cache_age = 100;
        fetch(&mut st, &serve(429, "{}"), "tok");
        assert_eq!(st.status, "", "silent under 5 min of cache");
        assert!(st.backing_off());
        assert!(st.backoff_until() > st.now + 100);
        st.cache_age = 1000;
        fetch(&mut st, &serve(429, "{}"), "tok");
        assert_eq!(st.status, "RATE LIMITED");

        let mut st = Store::new("claude");
        fetch(&mut st, &serve(200, "{\"unexpected\":true}"), "tok");
        assert_eq!((st.status.as_str(), st.hint.as_str()), ("BAD RESPONSE", "THE USAGE ENDPOINT CHANGED SHAPE"));
        let mut st = Store::new("claude");
        fetch(&mut st, &serve(200, "not json"), "tok");
        assert_eq!(st.status, "BAD RESPONSE");
        let mut st = Store::new("claude");
        fetch(&mut st, &refused(), "tok");
        assert_eq!((st.status.as_str(), st.hint.as_str()), ("NETWORK", "COULD NOT REACH API.ANTHROPIC.COM"));
        let mut st = Store::new("claude");
        fetch(&mut st, &serve(403, "{}"), "tok");
        assert_eq!(st.status, "NO PLAN ACCESS");
        let mut st = Store::new("claude");
        fetch(&mut st, &serve(404, "{}"), "tok");
        assert_eq!(st.status, "NO USAGE DATA");
        let mut st = Store::new("claude");
        fetch(&mut st, &serve(503, "down"), "tok");
        assert_eq!((st.status.as_str(), st.hint.as_str()), ("HTTP 503", "UNEXPECTED ANSWER FROM API.ANTHROPIC.COM"));
        drop(s);
    }

    #[test]
    fn run_from_the_credentials_file() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Sandbox::new("claude-run");
        // no login anywhere (the Keychain is unreachable in the sandbox): NO LOGIN, no call
        let d = run(270);
        assert_eq!(
            (d.status.as_str(), d.hint.as_str(), d.source.as_str(), d.fetched, d.plan.as_str()),
            ("NO LOGIN", "NO CLAUDE.AI LOGIN FOUND. RUN: pulse-limits doctor", "", 0, "")
        );
        assert!(!s.scratch.cache().join("last-reply-claude.json").exists());
        // the file login: one live call, to the local server
        let f = s.claude_login();
        std::env::set_var("PULSE_CLAUDE_USAGE_URL", serve(200, LIMITS));
        let d = run(270);
        assert_eq!((d.status.as_str(), d.source.as_str(), d.plan.as_str(), d.windows.len(), d.history.len()), ("", "LIVE", "MAX 20X", 5, 1));
        assert_eq!(d.credits, Value::Null);
        // a fresh cache: no call (a call would say NETWORK)
        std::env::set_var("PULSE_CLAUDE_USAGE_URL", refused());
        let d = run(270);
        assert_eq!((d.status.as_str(), d.source.as_str(), d.windows.len()), ("", "CACHE", 5));
        let d = run(-1);
        assert_eq!((d.status.as_str(), d.hint.as_str(), d.source.as_str()), ("NETWORK", "COULD NOT REACH API.ANTHROPIC.COM", "CACHE"));
        // the legacy shape carries credits
        std::env::set_var("PULSE_CLAUDE_USAGE_URL", serve(200, LEGACY));
        let d = run(-1);
        assert_eq!((d.windows.len(), d.credits), (2, json!({ "used": 1.5, "currency": "EUR" })));
        // a file without a token
        std::fs::write(&f, r#"{"claudeAiOauth":{"accessToken":"","subscriptionType":"pro"}}"#).unwrap();
        let d = run(-1);
        assert_eq!((d.status.as_str(), d.plan.as_str(), d.source.as_str()), ("NO LOGIN", "", "CACHE"));
        assert_eq!(usage_url(), std::env::var("PULSE_CLAUDE_USAGE_URL").unwrap());
        std::env::set_var("PULSE_CLAUDE_USAGE_URL", "  ");
        assert_eq!(usage_url(), USAGE_URL);
        drop(s);
    }

    #[test]
    fn doctor_without_a_login() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Sandbox::new("claude-doc-none");
        let cfg = s.home().join("claude-config");
        let out = capture(|| doctor(""));
        assert!(out.contains(&format!("  ok       CLAUDE_CONFIG_DIR={} in this shell", cfg.display())), "{out}");
        if is_macos() {
            assert!(out.contains("  PROBLEM  Keychain 'Claude Code-credentials' / account '': listed but not readable from here\n"), "{out}");
            assert!(out.contains("  ok       1 Keychain item(s) mention Claude\n"), "{out}");
            assert!(out.contains("  PROBLEM  no claude.ai login found anywhere on this Mac.\n"), "{out}");
            assert!(out.contains("security dump-keychain | grep -o"), "{out}");
        } else {
            assert!(out.contains("  PROBLEM  no claude.ai login found anywhere on this machine.\n"), "{out}");
            assert!(out.contains(&format!("On Linux Claude Code writes {}/.credentials.json", cfg.display())), "{out}");
        }
        assert!(out.ends_with("  usage api\n  PROBLEM  skipped (no token)\n  last reply (shape digest)\n  PROBLEM  no reply cached yet\n"), "{out}");
        // a pinned service, a Keychain that answers with something that is not JSON, a file with no login
        crate::util::write_atomic(&crate::util::config_dir().join("keychain"), b"Claude Code-credentials-work\n").unwrap();
        s.shim("security", "echo");
        let f = cfg.join(".credentials.json");
        crate::util::write_atomic(&f, b"{\"other\": 1}").unwrap();
        let out = capture(|| doctor(""));
        if is_macos() {
            assert!(out.contains("  ok       pinned Keychain service: Claude Code-credentials-work\n"), "{out}");
            assert!(out.contains("  PROBLEM  Keychain 'Claude Code-credentials-work' / account '': no claude.ai login in it (keys: not JSON, "), "{out}");
            assert!(out.contains("  PROBLEM  Keychain 'Claude Code-credentials' / account '': no claude.ai login in it (keys: not JSON, "), "{out}");
            assert!(out.contains("  ok       2 Keychain item(s) mention Claude\n"), "{out}");
        }
        assert!(out.contains(&format!("  PROBLEM  file {}: no claude.ai login in it\n", f.display())), "{out}");
        assert!(out.contains("  PROBLEM  skipped (no token)\n"), "{out}");
        drop(s);
    }

    #[test]
    fn doctor_with_a_login_probes_only_after_a_failed_plugin_run() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Sandbox::new("claude-doc-login");
        let f = s.claude_login();
        let out = capture(|| doctor(""));
        assert!(out.contains(&format!("  ok       file {}: claude.ai login\n", f.display())), "{out}");
        assert!(out.contains("  usage api\n  ok       reached through the plugin (not probed again: the endpoint has a small per-account quota)\n"), "{out}");
        // backing off: not probed either
        let st = Store::new("claude");
        st.set_backoff(180);
        let out = capture(|| doctor("RATE LIMITED"));
        assert!(out.contains("  ok       not probed: backing off after a 429 until "), "{out}");
        std::fs::remove_file(&st.backoff).unwrap();
        // the probe, one answer per call
        for (url, line) in [
            (serve(200, LIMITS), "  ok       HTTP 200: {\"five_hour\":null,\"seven_day\":null}\n"),
            (serve(200, LEGACY), "  ok       HTTP 200: {\"five_hour\":28.0,\"seven_day\":12.0}\n"),
            (serve(200, "nope"), "  ok       HTTP 200: unexpected JSON shape\n"),
            (serve(429, "{}"), "  PROBLEM  HTTP 429 rate limited: the account made too many usage calls recently"),
            (refused(), "  PROBLEM  no answer from api.anthropic.com\n"),
            (serve(503, "down\nfor now"), "  PROBLEM  HTTP 503: down for now\n"),
        ] {
            std::env::set_var("PULSE_CLAUDE_USAGE_URL", url);
            let out = capture(|| doctor("HTTP 503"));
            assert!(out.contains(line), "{out}");
        }
        // the digest of a cached limits[] reply
        let mut st = Store::new("claude");
        fetch(&mut st, &serve(200, LIMITS), "tok");
        let out = capture(|| doctor(""));
        assert!(
            out.contains("  ok       cached reply (0 min old): {\"keys\":\"five_hour,seven_day,limits,extra_usage\",\"five_hour\":null,\"seven_day\":null,\"limits\":[{\"kind\":\"session\",\"percent\":13,\"model\":null},"),
            "{out}"
        );
        assert!(out.contains("{\"kind\":\"weekly_scoped\",\"percent\":30,\"model\":\"Fable\"}"), "{out}");
        drop(s);
    }
}
