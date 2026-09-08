//! Codex: the ChatGPT login the OpenAI Codex CLI keeps in ~/.codex/auth.json (CODEX_HOME moves
//! that folder) and the usage endpoint the CLI itself reads its rate limits from. Both are
//! undocumented; the file and reply shapes follow what CodexBar (MIT) reads. The only thing
//! that leaves this machine is one GET to chatgpt.com.

use std::path::PathBuf;

use base64::alphabet::STANDARD;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use base64::Engine;
use serde_json::{json, Value};

use crate::providers::{bad, ok, Doc, Store, Window, BACKOFF_SECS};
use crate::util::{env_path, hhmmss, home, is_macos, iso_utc, local_offset, upper, which};

pub const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

pub fn codex_dir() -> PathBuf {
    env_path("CODEX_HOME").unwrap_or_else(|| home().join(".codex"))
}

/// `chatgpt_base_url` in config.toml sends the CLI through a proxy; follow it. A base without
/// /backend-api serves the same document under /api/codex/usage.
pub fn usage_url(config: &str) -> String {
    let base = config
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("chatgpt_base_url").and_then(|r| r.trim_start().strip_prefix('=')))
        .map(|r| r.split('#').next().unwrap_or("").chars().filter(|c| !c.is_whitespace() && *c != '"' && *c != '\'').collect::<String>())
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_default();
    if base.is_empty() {
        return USAGE_URL.into();
    }
    let base = if base == "https://chatgpt.com" || base == "https://chat.openai.com" { format!("{base}/backend-api") } else { base };
    if base.contains("/backend-api") {
        format!("{base}/wham/usage")
    } else {
        format!("{base}/api/codex/usage")
    }
}

/// A JWT's payload as JSON, nothing if it is not a JWT.
pub fn jwt_claims(token: &str) -> Option<Value> {
    let part = token.split('.').nth(1)?;
    let std: String = part
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            c => c,
        })
        .filter(|c| *c != '=')
        .collect();
    let engine = GeneralPurpose::new(&STANDARD, GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent));
    let bytes = engine.decode(std).ok()?;
    serde_json::from_slice::<Value>(&bytes).ok().filter(Value::is_object)
}

fn str_of(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| v.get(*k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string))
}

/// What auth.json says: a usable token (with its account id and plan claim), or why not.
#[derive(Default, Debug)]
pub struct Auth {
    pub token: String,
    pub account: String,
    pub plan: String, // "?" when unknown
    pub expires: i64,
    pub last_refresh: String,
    pub status: String,
    pub hint: String,
    pub file: PathBuf,
}

/// An OAuth login has tokens.access_token (+ id_token with the plan, account_id); an API-key
/// login has only OPENAI_API_KEY and cannot read plan limits. The access token is a JWT whose
/// exp claim says when it dies; the CLI refreshes it while it runs, and rotating it from here
/// could log the CLI out, so an expired one is reported, not refreshed.
pub fn read_auth(file: &PathBuf, now: i64) -> Auth {
    let mut a = Auth { plan: "?".into(), file: file.clone(), ..Default::default() };
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(_) => {
            a.status = "NO LOGIN".into();
            a.hint = format!("NO CODEX LOGIN ON THIS {}. RUN: codex login", if is_macos() { "MAC" } else { "MACHINE" });
            return a;
        }
    };
    let auth = match serde_json::from_str::<Value>(&text).ok().filter(Value::is_object) {
        Some(v) => v,
        None => {
            a.status = "BAD AUTH FILE".into();
            a.hint = format!("{} IS NOT JSON. RUN: codex login", file.display());
            return a;
        }
    };
    let tokens = auth.get("tokens").cloned().unwrap_or(Value::Null);
    let token = str_of(&tokens, &["access_token", "accessToken"]).unwrap_or_default();
    if token.is_empty() {
        if str_of(&auth, &["OPENAI_API_KEY"]).is_some() {
            a.status = "NO PLAN ACCESS".into();
            a.hint = "CODEX IS LOGGED IN WITH AN API KEY, NOT A CHATGPT PLAN. RUN: codex login".into();
        } else {
            a.status = "NO LOGIN".into();
            a.hint = "AUTH FILE HAS NO TOKENS. RUN: codex login".into();
        }
        return a;
    }
    let claims = str_of(&tokens, &["id_token", "idToken"]).and_then(|t| jwt_claims(&t)).unwrap_or(json!({}));
    let scoped = claims.get("https://api.openai.com/auth").cloned().unwrap_or(Value::Null);
    a.plan = str_of(&scoped, &["chatgpt_plan_type"]).or_else(|| str_of(&claims, &["chatgpt_plan_type"])).unwrap_or_else(|| "?".into());
    a.account = str_of(&tokens, &["account_id", "accountId"])
        .or_else(|| str_of(&scoped, &["chatgpt_account_id"]))
        .or_else(|| str_of(&claims, &["chatgpt_account_id"]))
        .unwrap_or_default();
    a.expires = jwt_claims(&token).and_then(|c| c.get("exp").and_then(Value::as_f64)).map(|e| e as i64).unwrap_or(0);
    a.last_refresh = str_of(&auth, &["last_refresh"]).unwrap_or_else(|| "?".into());
    if a.expires > 0 && a.expires <= now {
        a.status = "TOKEN EXPIRED".into();
        a.hint = "OPEN CODEX ONCE, IT REFRESHES THE TOKEN".into();
    } else {
        a.token = token;
    }
    a
}

/// "PLUS" from plan_type; "?" becomes nothing.
pub fn plan_label(plan: &str) -> String {
    if plan == "?" {
        String::new()
    } else {
        upper(&plan.replace('_', " "))
    }
}

fn window_name(secs: i64) -> String {
    match secs {
        18000 => "SESSION".into(),
        604800 => "WEEK".into(),
        s if s >= 86400 => format!("{}D", s / 86400),
        s => format!("{}H", s / 3600),
    }
}

fn win(w: Option<&Value>, label: String) -> Option<Window> {
    let w = w.filter(|w| w.is_object())?;
    let pct = w.get("used_percent").filter(|p| !p.is_null() && **p != Value::Bool(false)).cloned().unwrap_or(json!(0));
    let reset = w.get("reset_at").and_then(Value::as_f64).unwrap_or(0.0);
    Some(Window { label, pct, resets: if reset > 0.0 { Some(iso_utc(reset as i64)) } else { None } })
}

/// primary_window is normally the 5-hour window and secondary_window the week, but the CLI
/// tells them apart by limit_window_seconds, so do we. additional_rate_limits[] are per-model
/// caps (e.g. GPT-5.3-Codex-Spark); their primary window carries the utilisation; the last
/// word of the name is the ring label, so it fits next to the others. credits is a prepaid
/// balance, not spend, so it is not shown as CREDITS.
pub fn windows(c: &Value) -> Vec<Window> {
    let rl = c.get("rate_limit").filter(|r| r.is_object());
    let secs = |k: &str, d: i64| rl.and_then(|r| r.get(k)).and_then(|w| w.get("limit_window_seconds")).and_then(Value::as_i64).unwrap_or(d);
    let mut out: Vec<Window> = [
        win(rl.and_then(|r| r.get("primary_window")), window_name(secs("primary_window", 18000))),
        win(rl.and_then(|r| r.get("secondary_window")), window_name(secs("secondary_window", 604800))),
    ]
    .into_iter()
    .flatten()
    .collect();
    for extra in c.get("additional_rate_limits").and_then(Value::as_array).into_iter().flatten().filter(|e| e.is_object()) {
        let name = str_of(extra, &["limit_name", "metered_feature"]).unwrap_or_else(|| "EXTRA".into());
        let last = name.split('-').next_back().unwrap_or("").split(' ').next_back().unwrap_or("");
        let label: String = upper(last).chars().take(8).collect();
        let r = extra.get("rate_limit");
        let w = r.and_then(|r| r.get("primary_window")).filter(|w| !w.is_null()).or_else(|| r.and_then(|r| r.get("secondary_window")));
        out.extend(win(w, label));
    }
    out
}

/// `.rate_limit | (.primary_window // .secondary_window) != null`
pub fn reply_ok(v: &Value) -> bool {
    let rl = v.get("rate_limit").filter(|r| r.is_object());
    let present = |k: &str| rl.and_then(|r| r.get(k)).is_some_and(|w| !w.is_null() && *w != Value::Bool(false));
    present("primary_window") || present("secondary_window")
}

pub fn fetch(st: &mut Store, url: &str, token: &str, account: &str) {
    let auth = format!("Bearer {token}");
    let mut headers = vec![("Authorization", auth.as_str()), ("Accept", "application/json")];
    if !account.is_empty() {
        headers.push(("ChatGPT-Account-Id", account));
    }
    let (code, body) = st.get(url, &headers);
    match code {
        200 => {
            if serde_json::from_slice::<Value>(&body).ok().is_some_and(|v| reply_ok(&v)) {
                st.accept(&body);
            } else {
                st.set("BAD RESPONSE", "THE USAGE ENDPOINT CHANGED SHAPE");
            }
        }
        401 => st.set("TOKEN EXPIRED", "OPEN CODEX ONCE, IT REFRESHES THE TOKEN"),
        403 => st.set("NO PLAN ACCESS", "THIS LOGIN CANNOT READ PLAN LIMITS. LOG IN TO CODEX WITH A CHATGPT PLAN"),
        404 => st.set("NO USAGE DATA", "THIS ACCOUNT HAS NO PLAN LIMITS TO SHOW"),
        429 => {
            st.set_backoff(BACKOFF_SECS);
            if st.cache_age > 300 {
                st.set("RATE LIMITED", "TOO MANY USAGE CALLS FOR THIS ACCOUNT, RETRYING IN 3 MIN");
            }
        }
        0 => st.set("NETWORK", "COULD NOT REACH CHATGPT.COM"),
        c => st.set(&format!("HTTP {c}"), "UNEXPECTED ANSWER FROM CHATGPT.COM"),
    }
}

/// The plan the reply knows beats the claim in the token.
fn plan_from_cache(st: &Store, claim: &str) -> String {
    match st.cached() {
        Some(c) => c.get("plan_type").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| claim.to_string()),
        None if st.cache.is_file() => String::new(),
        None => claim.to_string(),
    }
}

pub fn run(min_interval: i64) -> Doc {
    let mut st = Store::new("codex");
    let dir = codex_dir();
    let auth = read_auth(&dir.join("auth.json"), st.now);
    if !auth.status.is_empty() {
        st.set(&auth.status, &auth.hint);
    }
    let plan = plan_from_cache(&st, &auth.plan);
    if !auth.token.is_empty() && st.due(min_interval) {
        let url = usage_url(&std::fs::read_to_string(dir.join("config.toml")).unwrap_or_default());
        fetch(&mut st, &url, &auth.token, &auth.account);
    }
    let windows = st.cached().map(|c| windows(&c)).unwrap_or_default();
    st.emit(&plan_label(&plan), windows, Value::Null)
}

pub fn doctor(pstatus: &str) {
    let st = Store::new("codex");
    let dir = codex_dir();
    let auth = read_auth(&dir.join("auth.json"), st.now);
    let plan = plan_from_cache(&st, &auth.plan);
    let url = usage_url(&std::fs::read_to_string(dir.join("config.toml")).unwrap_or_default());
    if let Some(d) = env_path("CODEX_HOME") {
        ok(&format!("CODEX_HOME={} in this shell ({} does not see shell variables)", d.display(), if is_macos() { "SwiftBar" } else { "the bar" }));
    }
    if !auth.file.is_file() {
        bad(&format!("no {}: run 'codex login' (or set CODEX_HOME to where the CLI keeps it)", auth.file.display()));
    } else if !auth.token.is_empty() {
        let expires = if auth.expires > 0 { iso_utc(auth.expires) } else { "?".into() };
        ok(&format!(
            "{}: ChatGPT login, plan {plan}, account id {}, token expires {expires}, last refresh {}",
            auth.file.display(),
            if auth.account.is_empty() { "missing" } else { "present" },
            auth.last_refresh
        ));
    } else {
        bad(&format!("{}: {} ({})", auth.file.display(), auth.status, auth.hint));
    }
    match which("codex") {
        Some(p) => ok(&format!("codex CLI: {}", p.display())),
        None => ok("codex CLI not on PATH (only needed to log in)"),
    }
    ok(&format!("usage url: {url}"));
    println!("  usage api");
    if auth.token.is_empty() {
        bad("skipped (no usable token)");
    } else if pstatus.is_empty() {
        ok("reached through the plugin (not probed again: keep the calls rare)");
    } else if st.backing_off() {
        ok(&format!("not probed: backing off after a 429 until {}", hhmmss(st.backoff_until(), local_offset())));
    } else {
        std::thread::sleep(std::time::Duration::from_secs(6));
        let bearer = format!("Bearer {}", auth.token);
        let mut headers = vec![("Authorization", bearer.as_str()), ("Accept", "application/json")];
        if !auth.account.is_empty() {
            headers.push(("ChatGPT-Account-Id", auth.account.as_str()));
        }
        let (code, body) = crate::providers::http_get(&url, &headers);
        let text = String::from_utf8_lossy(&body).replace('\n', " ");
        match code {
            200 => match serde_json::from_slice::<Value>(&body) {
                Ok(v) => ok(&format!(
                    "HTTP 200: {}",
                    json!({ "plan_type": v["plan_type"], "primary": v["rate_limit"]["primary_window"]["used_percent"], "secondary": v["rate_limit"]["secondary_window"]["used_percent"] })
                )),
                Err(_) => ok("HTTP 200: unexpected JSON shape"),
            },
            401 => bad("HTTP 401: the token is expired or revoked. Open the Codex CLI once, or run 'codex login'."),
            429 => bad("HTTP 429 rate limited: too many usage calls recently. It recovers by itself; wait a few minutes."),
            0 => bad("no answer from chatgpt.com"),
            c => bad(&format!("HTTP {c}: {}", text.chars().take(240).collect::<String>())),
        }
    }
    println!("  last reply (shape digest)");
    st.doctor_digests(|c| {
        let extra: Vec<Value> =
            c.get("additional_rate_limits").and_then(Value::as_array).map(|a| a.iter().map(|e| e["limit_name"].clone()).collect()).unwrap_or_default();
        json!({ "keys": c.as_object().map(|m| m.keys().cloned().collect::<Vec<_>>().join(",")).unwrap_or_default(), "plan_type": c["plan_type"],
                "primary": c["rate_limit"]["primary_window"], "secondary": c["rate_limit"]["secondary_window"], "extra": extra })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::testing::{refused, serve, Scratch, ENV};

    // the fixtures of PR #4: an unsigned JWT with exp 1788870066 (past) / 1788877266, plan plus, account acct_fixture
    const ID_TOKEN: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJlbWFpbCI6ImZpeHR1cmVAZXhhbXBsZS5jb20iLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9wbGFuX3R5cGUiOiJwbHVzIiwiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjdF9maXh0dXJlIn19.sig";
    const EXPIRED: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJleHAiOjE3ODg4NzAwNjZ9.sig";
    const VALID: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJleHAiOjE3ODg4NzcyNjYsImh0dHBzOi8vYXBpLm9wZW5haS5jb20vYXV0aCI6eyJjaGF0Z3B0X2FjY291bnRfaWQiOiJhY2N0X2ZpeHR1cmUifX0.sig";
    pub const REPLY: &str = r#"{"plan_type":"plus",
        "rate_limit":{"primary_window":{"used_percent":17,"reset_at":1788881706,"limit_window_seconds":18000},
                      "secondary_window":{"used_percent":42,"reset_at":1789244466,"limit_window_seconds":604800}},
        "credits":{"has_credits":true,"unlimited":false,"balance":12.5},
        "additional_rate_limits":[{"limit_name":"GPT-5.3-Codex-Spark","rate_limit":{"primary_window":{"used_percent":8,"reset_at":1788881706,"limit_window_seconds":18000}}}]}"#;

    fn auth_file(dir: &std::path::Path, text: &str) -> PathBuf {
        let f = dir.join("auth.json");
        std::fs::write(&f, text).unwrap();
        f
    }

    #[test]
    fn six_auth_cases() {
        let d = std::env::temp_dir().join(format!("pl-rust-codex-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let now = 1788873000;
        // 1. no auth file
        let a = read_auth(&d.join("auth.json"), now);
        assert_eq!((a.status.as_str(), a.token.as_str()), ("NO LOGIN", ""));
        assert!(a.hint.starts_with("NO CODEX LOGIN ON THIS"));
        // 2. tokens but none in it
        let a = read_auth(&auth_file(&d, r#"{"tokens": {}}"#), now);
        assert_eq!((a.status.as_str(), a.hint.as_str()), ("NO LOGIN", "AUTH FILE HAS NO TOKENS. RUN: codex login"));
        // 3. API-key only
        let a = read_auth(&auth_file(&d, r#"{"OPENAI_API_KEY": "sk-test-not-a-real-key"}"#), now);
        assert_eq!((a.status.as_str(), a.hint.as_str()), ("NO PLAN ACCESS", "CODEX IS LOGGED IN WITH AN API KEY, NOT A CHATGPT PLAN. RUN: codex login"));
        // 4. not JSON
        let a = read_auth(&auth_file(&d, "this is not json"), now);
        assert_eq!(a.status, "BAD AUTH FILE");
        assert_eq!(a.hint, format!("{} IS NOT JSON. RUN: codex login", d.join("auth.json").display()));
        // 5. expired JWT: reported, no token to call with
        let a = read_auth(
            &auth_file(
                &d,
                &format!(
                    r#"{{"tokens": {{"access_token": "{EXPIRED}", "refresh_token": "rt", "id_token": "{ID_TOKEN}"}}, "last_refresh": "2026-08-01T10:00:00.000Z"}}"#
                ),
            ),
            now,
        );
        assert_eq!((a.status.as_str(), a.hint.as_str(), a.token.as_str()), ("TOKEN EXPIRED", "OPEN CODEX ONCE, IT REFRESHES THE TOKEN", ""));
        assert_eq!((a.plan.as_str(), a.account.as_str(), a.expires), ("plus", "acct_fixture", 1788870066));
        // 6. valid JWT
        let a = read_auth(
            &auth_file(
                &d,
                &format!(
                    r#"{{"tokens": {{"access_token": "{VALID}", "refresh_token": "rt", "id_token": "{ID_TOKEN}", "account_id": "acct_fixture"}}, "last_refresh": "2026-09-08T10:00:00.000Z"}}"#
                ),
            ),
            now,
        );
        assert_eq!((a.status.as_str(), a.token, a.plan.as_str(), a.account.as_str(), a.expires), ("", VALID.to_string(), "plus", "acct_fixture", 1788877266));
        assert_eq!(a.last_refresh, "2026-09-08T10:00:00.000Z");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn reply_windows() {
        let c: Value = serde_json::from_str(REPLY).unwrap();
        let w = windows(&c);
        let got: Vec<(String, String, Option<String>)> = w.iter().map(|w| (w.label.clone(), w.pct.to_string(), w.resets.clone())).collect();
        assert_eq!(
            got,
            vec![
                ("SESSION".into(), "17".into(), Some("2026-09-08T15:35:06Z".into())),
                ("WEEK".into(), "42".into(), Some("2026-09-12T20:21:06Z".into())),
                ("SPARK".into(), "8".into(), Some("2026-09-08T15:35:06Z".into())),
            ]
        );
        assert!(reply_ok(&c));
        assert!(!reply_ok(&serde_json::from_str("{\"unexpected\":true}").unwrap()));
        let odd: Value = serde_json::from_str(r#"{"rate_limit":{"primary_window":{"used_percent":1,"limit_window_seconds":3600,"reset_at":0},"secondary_window":{"limit_window_seconds":172800}},
            "additional_rate_limits":[{"metered_feature":"long name here","rate_limit":{"secondary_window":{"used_percent":3}}}, "junk", {"limit_name":"x","rate_limit":null}]}"#).unwrap();
        let w = windows(&odd);
        let got: Vec<(String, String, Option<String>)> = w.iter().map(|w| (w.label.clone(), w.pct.to_string(), w.resets.clone())).collect();
        assert_eq!(got, vec![("1H".into(), "1".into(), None), ("2D".into(), "0".into(), None), ("HERE".into(), "3".into(), None)]);
        assert_eq!(plan_label("plus"), "PLUS");
        assert_eq!(plan_label("team_pro"), "TEAM PRO");
        assert_eq!(plan_label("?"), "");
    }

    #[test]
    fn base_url_from_config() {
        assert_eq!(usage_url(""), USAGE_URL);
        assert_eq!(usage_url("model = \"gpt-5\"\n# chatgpt_base_url = \"x\"\n"), USAGE_URL);
        assert_eq!(usage_url("chatgpt_base_url = \"http://127.0.0.1:48612/ok/backend-api/\"   # proxy\n"), "http://127.0.0.1:48612/ok/backend-api/wham/usage");
        assert_eq!(usage_url("  chatgpt_base_url='http://127.0.0.1:48612/alt'\n"), "http://127.0.0.1:48612/alt/api/codex/usage");
        assert_eq!(usage_url("chatgpt_base_url = \"https://chatgpt.com/\"\n"), USAGE_URL);
        assert_eq!(usage_url("chatgpt_base_url = \"https://chat.openai.com\"\n"), "https://chat.openai.com/backend-api/wham/usage");
        assert_eq!(jwt_claims("not.a-jwt"), None);
        assert_eq!(jwt_claims("nodots"), None);
        assert_eq!(jwt_claims(EXPIRED).unwrap()["exp"], json!(1788870066));
    }

    #[test]
    fn http_paths() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("codex-http");
        let mut st = Store::new("codex");
        fetch(&mut st, &serve(200, REPLY), "tok", "acct");
        assert_eq!((st.status.as_str(), st.source.as_str()), ("", "LIVE"));
        assert_eq!(plan_from_cache(&st, "?"), "plus");
        let d = st.emit(&plan_label(&plan_from_cache(&st, "?")), windows(&st.cached().unwrap()), Value::Null);
        assert_eq!((d.plan.as_str(), d.windows.len(), d.history.len()), ("PLUS", 3, 1));
        assert_eq!(d.history[0].1, 17);
        let mut st = Store::new("codex");
        st.cache_age = 1000;
        fetch(&mut st, &serve(401, "{\"detail\":\"Unauthorized\"}"), "tok", "");
        assert_eq!((st.status.as_str(), st.hint.as_str()), ("TOKEN EXPIRED", "OPEN CODEX ONCE, IT REFRESHES THE TOKEN"));
        let mut st = Store::new("codex");
        st.cache_age = 1000;
        fetch(&mut st, &serve(429, "{\"detail\":\"Too many requests\"}"), "tok", "");
        assert_eq!(st.status, "RATE LIMITED");
        assert!(st.backing_off());
        let mut st = Store::new("codex");
        fetch(&mut st, &serve(200, "{\"unexpected\": true}"), "tok", "");
        assert_eq!(st.status, "BAD RESPONSE");
        let mut st = Store::new("codex");
        fetch(&mut st, &refused(), "tok", "");
        assert_eq!((st.status.as_str(), st.hint.as_str()), ("NETWORK", "COULD NOT REACH CHATGPT.COM"));
        let mut st = Store::new("codex");
        fetch(&mut st, &serve(403, "{}"), "tok", "");
        assert_eq!(st.hint, "THIS LOGIN CANNOT READ PLAN LIMITS. LOG IN TO CODEX WITH A CHATGPT PLAN");
        drop(s);
    }
}
