//! Grok: the xAI login the Grok CLI keeps in ~/.grok/auth.json (GROK_HOME moves that folder)
//! and the billing endpoint the CLI itself reads its weekly credit pool from, behind the
//! cli-chat-proxy. Neither is documented; the file and reply shapes follow what the CLI and
//! CodexBar (MIT) read. The only things that leave this machine are one GET to
//! cli-chat-proxy.grok.com for the pool and, at most hourly, one more for the plan name.
//!
//! The token is read, never refreshed: the CLI holds the file under its own lock, rewrites it
//! whole, and deletes it when its refresh token dies, so an outside refresh would race it or
//! log it out. The `grok` binary is never spawned either: any run may self-update, sync its
//! config, or rewrite the login.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::providers::codex::jwt_claims;
use crate::providers::{bad, ok, say, Doc, Store, Window, BACKOFF_SECS, PROBE_DELAY_SECS};
use crate::util::{
    cache_dir, env_path, epoch_of, hhmmss, home, is_executable, is_macos, iso_utc, local_offset, mtime, read_trimmed, upper, which, write_atomic,
};

pub const BASE_URL: &str = "https://cli-chat-proxy.grok.com/v1";
pub const TOKEN_AUTH: &str = "xai-grok-cli"; // X-XAI-Token-Auth: "validate as a CLI session token"
pub const EARLY_SECS: i64 = 300; // the CLI's own GROK_AUTH_EARLY_INVALIDATION_SECS
pub const PLAN_TTL: i64 = 3600; // the plan name is asked at most hourly

pub fn grok_dir() -> PathBuf {
    env_path("GROK_HOME").unwrap_or_else(|| home().join(".grok"))
}

/// The CLI's own proxy override, without a trailing slash.
pub fn base_url() -> String {
    match std::env::var("GROK_CLI_CHAT_PROXY_BASE_URL") {
        Ok(v) if !v.trim().is_empty() => v.trim().trim_end_matches('/').to_string(),
        _ => BASE_URL.into(),
    }
}

pub fn billing_url(base: &str) -> String {
    format!("{base}/billing?format=credits")
}

pub fn settings_url(base: &str) -> String {
    format!("{base}/settings")
}

fn headers(bearer: &str) -> [(&str, &str); 3] {
    [("Authorization", bearer), ("X-XAI-Token-Auth", TOKEN_AUTH), ("Accept", "application/json")]
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// What auth.json says: a usable token (with what the doctor shows about it), or why not.
#[derive(Default, Debug)]
pub struct Auth {
    pub token: String,
    pub kind: String,      // oidc (https://auth.x.ai::<client id>), legacy (…/sign-in) or sso (<issuer>::<client id>)
    pub principal: String, // principal_type: User or Team; "?" when absent
    pub expires: i64,      // epoch; 0 when unknown
    pub created: String,   // create_time as written; "?"
    pub has_refresh: bool,
    pub status: String,
    pub hint: String,
    pub file: PathBuf,
}

/// The entry to use, out of the scope-keyed object: an OIDC login first, then a legacy session
/// login, then any issuer::client scope (enterprise SSO); only entries that carry a key count.
pub fn pick_entry(auth: &Value) -> Option<(String, Value)> {
    let entries: Vec<(&String, &Value)> = auth.as_object()?.iter().filter(|(_, v)| str_of(v, "key").is_some()).collect();
    let find = |f: &dyn Fn(&str) -> bool| entries.iter().find(|(k, _)| f(k)).map(|(k, v)| ((*k).clone(), (*v).clone()));
    find(&|k| k.starts_with("https://auth.x.ai::")).or_else(|| find(&|k| k.contains("/sign-in"))).or_else(|| find(&|k| k.contains("::")))
}

fn scope_kind(scope: &str) -> &'static str {
    if scope.starts_with("https://auth.x.ai::") {
        "oidc"
    } else if scope.contains("/sign-in") {
        "legacy"
    } else {
        "sso"
    }
}

/// An `xai-…` API key or a browser cookie jar in the key slot: neither can read plan limits.
pub fn not_a_login(key: &str) -> bool {
    key.starts_with("xai-") || key.contains("sso=") || key.contains("sso-rw=") || (key.contains('=') && key.contains(';'))
}

/// The file, read again after 200 ms when it is missing: the CLI replaces it atomically and
/// deletes it when a refresh fails for good, so a first miss may be a rewrite in progress.
fn read_twice(file: &Path) -> Option<String> {
    std::fs::read_to_string(file).ok().or_else(|| {
        std::thread::sleep(Duration::from_millis(200));
        std::fs::read_to_string(file).ok()
    })
}

/// A token is expired 5 min early, as the CLI itself treats it, so a call never races a token
/// that dies mid-request; `expires_at` says when, the JWT's exp claim when the file does not.
pub fn read_auth(file: &Path, now: i64) -> Auth {
    let mut a = Auth { principal: "?".into(), created: "?".into(), file: file.to_path_buf(), ..Default::default() };
    let text = match read_twice(file) {
        Some(t) => t,
        None => {
            a.status = "NO LOGIN".into();
            a.hint = format!("NO GROK LOGIN ON THIS {}. RUN: grok login", if is_macos() { "MAC" } else { "MACHINE" });
            return a;
        }
    };
    let auth = match serde_json::from_str::<Value>(&text).ok().filter(Value::is_object) {
        Some(v) => v,
        None => {
            a.status = "BAD AUTH FILE".into();
            a.hint = format!("{} IS NOT JSON. RUN: grok login", file.display());
            return a;
        }
    };
    let Some((scope, entry)) = pick_entry(&auth) else {
        a.status = "NO LOGIN".into();
        a.hint = "AUTH FILE HAS NO TOKENS. RUN: grok login".into();
        return a;
    };
    a.kind = scope_kind(&scope).into();
    a.principal = str_of(&entry, "principal_type").unwrap_or_else(|| "?".into());
    a.created = str_of(&entry, "create_time").unwrap_or_else(|| "?".into());
    a.has_refresh = str_of(&entry, "refresh_token").is_some();
    let key = str_of(&entry, "key").unwrap_or_default();
    if not_a_login(&key) {
        a.status = "NO PLAN ACCESS".into();
        a.hint = "GROK IS LOGGED IN WITH AN API KEY, NOT A SUBSCRIPTION. RUN: grok login".into();
        return a;
    }
    a.expires = str_of(&entry, "expires_at")
        .and_then(|s| epoch_of(&s))
        .or_else(|| jwt_claims(&key).and_then(|c| c.get("exp").and_then(Value::as_f64)).map(|e| e as i64))
        .unwrap_or(0);
    if a.expires > 0 && now >= a.expires - EARLY_SECS {
        a.status = "TOKEN EXPIRED".into();
        a.hint = "OPEN GROK ONCE, IT REFRESHES THE TOKEN".into();
    } else {
        a.token = key;
    }
    a
}

/// "X PREMIUM+" / "SUPERGROK" / "SUPERGROK HEAVY": the display name as CodexBar normalises it
/// (heavy → SuperGrok Heavy, supergrok → SuperGrok, else verbatim), upper-cased like the other readers.
pub fn plan_label(raw: &str) -> String {
    let t = raw.trim();
    let l = t.to_ascii_lowercase();
    if l.is_empty() {
        String::new()
    } else if l.contains("heavy") {
        "SUPERGROK HEAVY".into()
    } else if l.contains("supergrok") {
        "SUPERGROK".into()
    } else {
        upper(&t.replace('_', " "))
    }
}

/// The tier a billing reply may carry (SuperGrok accounts): config.subscriptionTier or top-level.
pub fn plan_of_reply(c: &Value) -> String {
    let tier = c.get("config").and_then(|c| c.get("subscriptionTier")).or_else(|| c.get("subscriptionTier")).and_then(Value::as_str).unwrap_or("");
    plan_label(tier)
}

/// The plan name /settings gives; auth_mode is never used for it (X Premium+ logs in the same way as SuperGrok).
pub fn plan_of_settings(v: &Value) -> String {
    plan_label(v.get("subscription_tier_display").and_then(Value::as_str).unwrap_or(""))
}

fn plan_file() -> PathBuf {
    cache_dir().join("plan-grok")
}

/// The label from the last settings lookup; None when never looked up (empty: looked up, no tier named).
pub fn plan_cached() -> Option<String> {
    read_trimmed(&plan_file())
}

pub fn plan_stale(now: i64) -> bool {
    mtime(&plan_file()).is_none_or(|m| now - m > PLAN_TTL)
}

/// One GET to /settings for the plan name. A 200 replaces the label; any other answer keeps
/// it and postpones the next lookup; no answer leaves the file alone so the next run retries.
pub fn lookup_plan(base: &str, token: &str) {
    let bearer = format!("Bearer {token}");
    let (code, body) = crate::providers::http_get(&settings_url(base), &headers(&bearer));
    let label = match code {
        200 => serde_json::from_slice::<Value>(&body).map(|v| plan_of_settings(&v)).unwrap_or_default(),
        0 => return,
        _ => plan_cached().unwrap_or_default(),
    };
    let _ = write_atomic(&plan_file(), format!("{label}\n").as_bytes());
}

/// 0..100 as the reply wrote it, clamped at the ends (the CLI says "You hit your weekly limit" at the cap).
fn clamp(p: &Value) -> Value {
    match p.as_f64() {
        Some(x) if x < 0.0 => json!(0),
        Some(x) if x > 100.0 => json!(100),
        Some(_) => p.clone(),
        None => json!(0),
    }
}

/// WEEK / MONTH from the period type, else from its length; a pool that says neither is the weekly one.
pub fn period_label(kind: &str, start: Option<i64>, end: Option<i64>) -> String {
    let k = upper(kind);
    if k.contains("WEEKLY") {
        return "WEEK".into();
    }
    if k.contains("MONTHLY") {
        return "MONTH".into();
    }
    match (start, end) {
        (Some(s), Some(e)) if e > s => match e - s {
            secs if (561_600..=648_000).contains(&secs) => "WEEK".into(), // 6.5 to 7.5 days
            secs if (28 * 86400..=31 * 86400).contains(&secs) => "MONTH".into(),
            secs if secs >= 86400 => format!("{}D", secs / 86400),
            secs => format!("{}H", secs / 3600),
        },
        _ => "WEEK".into(),
    }
}

/// One window for the credit pool (the only limit Grok has), ONDEMAND when an on-demand cap is
/// set, and per-product windows only when they say something the pool does not. The reply is
/// protobuf JSON, which drops scalars at their default: a missing creditUsagePercent is 0 while
/// the period is current and unknown otherwise (then no window: the reply is stale, not empty).
pub fn windows(c: &Value, now: i64) -> Vec<Window> {
    let Some(cfg) = c.get("config").filter(|c| c.is_object()) else { return vec![] };
    let period = cfg.get("currentPeriod").filter(|p| p.is_object());
    let field = |k: &str| period.and_then(|p| p.get(k)).and_then(Value::as_str);
    let start_s = field("start").or_else(|| cfg.get("billingPeriodStart").and_then(Value::as_str));
    let end_s = field("end").or_else(|| cfg.get("billingPeriodEnd").and_then(Value::as_str));
    let (start, end) = (start_s.and_then(epoch_of), end_s.and_then(epoch_of));
    let resets = end_s.map(str::to_string);
    let pct = match cfg.get("creditUsagePercent").filter(|p| p.is_number()) {
        Some(p) => clamp(p),
        None => match (start, end) {
            (Some(s), Some(e)) if s <= now && now < e => json!(0),
            _ => return vec![],
        },
    };
    let mut out = vec![Window { label: period_label(field("type").unwrap_or(""), start, end), pct: pct.clone(), resets: resets.clone() }];
    let money = |k: &str| cfg.get(k).and_then(|m| m.get("val")).and_then(Value::as_f64).unwrap_or(0.0);
    let cap = money("onDemandCap");
    if cap > 0.0 {
        out.push(Window { label: "ONDEMAND".into(), pct: clamp(&json!(money("onDemandUsed") / cap * 100.0)), resets: resets.clone() });
    }
    let products: Vec<&Value> = cfg.get("productUsage").and_then(Value::as_array).map(|a| a.iter().filter(|p| p.is_object()).collect()).unwrap_or_default();
    let zero = json!(0);
    let aggregate = pct.as_f64().unwrap_or(0.0);
    let usage = |p: &&Value| p.get("usagePercent").filter(|u| u.is_number()).unwrap_or(&zero).clone();
    if products.len() > 1 || products.iter().any(|p| usage(p).as_f64().unwrap_or(0.0) != aggregate) {
        for p in &products {
            let name = p.get("product").and_then(Value::as_str).filter(|n| !n.is_empty()).unwrap_or("PRODUCT");
            let short = name.strip_prefix("Grok").filter(|s| !s.is_empty()).unwrap_or(name);
            out.push(Window { label: upper(short).chars().take(8).collect(), pct: clamp(&usage(p)), resets: resets.clone() });
        }
    }
    out
}

/// The reply has the config block the pool lives in.
pub fn reply_ok(v: &Value) -> bool {
    v.get("config").is_some_and(Value::is_object)
}

/// One live call and what it means.
pub fn fetch(st: &mut Store, url: &str, token: &str) {
    let bearer = format!("Bearer {token}");
    let (code, body) = st.get(url, &headers(&bearer));
    match code {
        200 => {
            if serde_json::from_slice::<Value>(&body).ok().is_some_and(|v| reply_ok(&v)) {
                st.accept(&body);
            } else {
                st.set("BAD RESPONSE", "THE USAGE ENDPOINT CHANGED SHAPE");
            }
        }
        401 => st.set("TOKEN EXPIRED", "OPEN GROK ONCE, IT REFRESHES THE TOKEN"),
        403 => st.set("NO PLAN ACCESS", "THIS LOGIN CANNOT READ PLAN LIMITS. LOG IN TO GROK WITH A SUBSCRIPTION"),
        404 => st.set("NO USAGE DATA", "THIS ACCOUNT HAS NO PLAN LIMITS TO SHOW"),
        429 => {
            st.set_backoff(BACKOFF_SECS);
            if st.cache_age > 300 {
                st.set("RATE LIMITED", "TOO MANY USAGE CALLS FOR THIS ACCOUNT, RETRYING IN 3 MIN");
            }
        }
        0 => st.set("NETWORK", "COULD NOT REACH CLI-CHAT-PROXY.GROK.COM"),
        c => st.set(&format!("HTTP {c}"), "UNEXPECTED ANSWER FROM GROK.COM"),
    }
}

/// The label to show: the settings lookup first, then the tier a billing reply may carry.
fn plan_of(cached: Option<&Value>) -> String {
    plan_cached().filter(|p| !p.is_empty()).unwrap_or_else(|| cached.map(plan_of_reply).unwrap_or_default())
}

/// The document from the cache and, when due, one live call (plus the hourly plan lookup).
pub fn run(min_interval: i64) -> Doc {
    let mut st = Store::new("grok");
    let auth = read_auth(&grok_dir().join("auth.json"), st.now);
    if !auth.status.is_empty() {
        st.set(&auth.status, &auth.hint);
    }
    let base = base_url();
    if !auth.token.is_empty() && st.due(min_interval) {
        fetch(&mut st, &billing_url(&base), &auth.token);
        // the plan name is not in the billing reply: ask /settings after a good one, at most hourly
        if st.source == "LIVE" && plan_stale(st.now) {
            lookup_plan(&base, &auth.token);
        }
    }
    let cached = st.cached();
    let windows = cached.as_ref().map(|c| windows(c, st.now)).unwrap_or_default();
    st.emit(&plan_of(cached.as_ref()), windows, Value::Null)
}

/// {keys, period type, percent, on-demand cap, error}: the shape of a reply, never the balance.
fn digest(c: &Value) -> Value {
    json!({ "keys": c.as_object().map(|m| m.keys().cloned().collect::<Vec<_>>().join(",")).unwrap_or_default(),
            "period": c["config"]["currentPeriod"]["type"], "creditUsagePercent": c["config"]["creditUsagePercent"],
            "onDemandCap": c["config"]["onDemandCap"]["val"], "error": c["error"] })
}

/// The login file, the cached plan, the endpoint and the last reply, never the token. `pstatus`
/// is what the plugin run just reported for this provider.
pub fn doctor(pstatus: &str) {
    let st = Store::new("grok");
    let auth = read_auth(&grok_dir().join("auth.json"), st.now);
    let base = base_url();
    let bar = if is_macos() { "SwiftBar" } else { "the bar" };
    if let Some(d) = env_path("GROK_HOME") {
        ok(&format!("GROK_HOME={} in this shell ({bar} does not see shell variables)", d.display()));
    }
    if base != BASE_URL {
        ok(&format!("GROK_CLI_CHAT_PROXY_BASE_URL={base} in this shell ({bar} does not see shell variables)"));
    }
    if !auth.file.is_file() {
        bad(&format!("no {}: run 'grok login' (or set GROK_HOME to where the CLI keeps it)", auth.file.display()));
    } else if !auth.token.is_empty() {
        let expires = if auth.expires > 0 { iso_utc(auth.expires) } else { "?".into() };
        ok(&format!(
            "{}: {} login, principal {}, token expires {expires}, created {}, refresh token {}",
            auth.file.display(),
            auth.kind,
            auth.principal,
            auth.created,
            if auth.has_refresh { "present" } else { "missing" }
        ));
    } else {
        bad(&format!("{}: {} ({})", auth.file.display(), auth.status, auth.hint));
    }
    match (plan_cached(), mtime(&plan_file())) {
        (Some(p), Some(m)) if !p.is_empty() => ok(&format!("plan: {p} (from {}, {} min old)", settings_url(&base), (st.now - m) / 60)),
        (Some(_), _) => ok("plan: the settings reply named no tier"),
        _ => ok("plan: not looked up yet (asked after the first good billing reply, then hourly)"),
    }
    // the CLI installs itself under GROK_HOME/bin, which the bar's PATH does not carry
    match which("grok").or_else(|| Some(grok_dir().join("bin").join("grok")).filter(|p| is_executable(p))) {
        Some(p) => ok(&format!("grok CLI: {} (never run from here: it may self-update or rewrite the login)", p.display())),
        None => ok("grok CLI not found (only needed to log in)"),
    }
    ok(&format!("usage url: {}", billing_url(&base)));
    say("  usage api");
    if auth.token.is_empty() {
        bad("skipped (no usable token)");
    } else if pstatus.is_empty() {
        ok("reached through the plugin (not probed again: keep the calls rare)");
    } else if st.backing_off() {
        ok(&format!("not probed: backing off after a 429 until {}", hhmmss(st.backoff_until(), local_offset())));
    } else {
        std::thread::sleep(Duration::from_secs(PROBE_DELAY_SECS));
        let bearer = format!("Bearer {}", auth.token);
        let (code, body) = crate::providers::http_get(&billing_url(&base), &headers(&bearer));
        let text = String::from_utf8_lossy(&body).replace('\n', " ");
        match code {
            200 => match serde_json::from_slice::<Value>(&body) {
                Ok(v) => ok(&format!("HTTP 200: {}", digest(&v))),
                Err(_) => ok("HTTP 200: unexpected JSON shape"),
            },
            401 => bad("HTTP 401: the token is expired or revoked. Open the Grok CLI once, or run 'grok login'."),
            403 => bad("HTTP 403: this login cannot read plan limits (an API key, or a plan without a credit pool)."),
            429 => bad("HTTP 429 rate limited: too many usage calls recently. It recovers by itself; wait a few minutes."),
            0 => bad("no answer from cli-chat-proxy.grok.com"),
            c => bad(&format!("HTTP {c}: {}", text.chars().take(240).collect::<String>())),
        }
    }
    say("  last reply (shape digest)");
    st.doctor_digests(digest);
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::*;
    use crate::providers::testing::{capture, refused, serve, Sandbox, Scratch, ENV};

    // the fixtures of issue #8: a real reply (redacted), the same reply as the CLI logs it on a
    // fresh period, the settings envelope, the 401, and an auth.json with a dummy token
    const REPLY: &str = include_str!("../../tests/fixtures/grok/fixture-200.json");
    const ZERO: &str = include_str!("../../tests/fixtures/grok/fixture-200-zero-usage.synthetic.json");
    const SETTINGS: &str = include_str!("../../tests/fixtures/grok/fixture-settings-200.json");
    const DENIED: &str = include_str!("../../tests/fixtures/grok/fixture-401.json");
    const AUTH: &str = include_str!("../../tests/fixtures/grok/auth-fixture.json");

    const CAPTURED: i64 = 1788877617; // 2026-09-08T14:26:57Z, inside the fixture's period and token life
    const PERIOD_START: i64 = 1788802218; // 2026-09-07T17:30:18Z
    const PERIOD_END: i64 = 1789407018; // 2026-09-14T17:30:18Z
    const EXPIRES: i64 = 1788898855; // the fixture's expires_at, 2026-09-08T20:20:55Z
    const RESET: &str = "2026-09-14T17:30:18.071364+00:00";

    fn v(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    fn fixture_key() -> String {
        v(AUTH).as_object().unwrap().values().next().unwrap()["key"].as_str().unwrap().to_string()
    }

    /// The 401 body as the proxy sent it (the fixture wraps it with the headers).
    fn denied_body() -> &'static str {
        Box::leak(v(DENIED)["body"].to_string().into_boxed_str())
    }

    fn rows(w: &[Window]) -> Vec<(String, String, Option<String>)> {
        w.iter().map(|w| (w.label.clone(), w.pct.to_string(), w.resets.clone())).collect()
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pl-rust-grok-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn auth_file(dir: &Path, text: &str) -> PathBuf {
        let f = dir.join("auth.json");
        std::fs::write(&f, text).unwrap();
        f
    }

    /// The fixture entry with another expires_at (a run() test uses the real clock).
    fn auth_expiring(at: &str) -> String {
        let mut a = v(AUTH);
        a.as_object_mut().unwrap().values_mut().next().unwrap()["expires_at"] = json!(at);
        a.to_string()
    }

    /// GROK_HOME + the proxy override for a run() test; both are removed when dropped.
    struct Env;

    impl Env {
        fn new(home: &Path, base: &str) -> Env {
            std::env::set_var("GROK_HOME", home);
            std::env::set_var("GROK_CLI_CHAT_PROXY_BASE_URL", base);
            Env
        }
        fn base(&self, base: &str) {
            std::env::set_var("GROK_CLI_CHAT_PROXY_BASE_URL", base);
        }
    }

    impl Drop for Env {
        fn drop(&mut self) {
            std::env::remove_var("GROK_HOME");
            std::env::remove_var("GROK_CLI_CHAT_PROXY_BASE_URL");
        }
    }

    /// A server answering `n` requests, each by the first route whose path the request line contains.
    fn serve_routes(n: usize, routes: Vec<(&'static str, u16, &'static str)>) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        thread::spawn(move || {
            for _ in 0..n {
                let Ok((mut s, _)) = l.accept() else { break };
                let mut buf = [0u8; 4096];
                let got = s.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..got]).into_owned();
                let line = head.lines().next().unwrap_or("").to_string();
                let (code, body) = routes.iter().find(|(p, _, _)| line.contains(p)).map(|(_, c, b)| (*c, *b)).unwrap_or((404, "{}"));
                let _ = write!(s, "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        url
    }

    #[test]
    fn auth_cases() {
        let d = scratch_dir("auth");
        let now = CAPTURED;
        // 1. no auth file (read twice, 200 ms apart)
        let a = read_auth(&d.join("auth.json"), now);
        assert_eq!((a.status.as_str(), a.token.as_str()), ("NO LOGIN", ""));
        assert!(a.hint.starts_with("NO GROK LOGIN ON THIS"));
        // 2. not JSON
        let a = read_auth(&auth_file(&d, "nope"), now);
        assert_eq!((a.status.as_str(), a.hint), ("BAD AUTH FILE", format!("{} IS NOT JSON. RUN: grok login", d.join("auth.json").display())));
        // 3. no usable entry: empty, an empty key, a scope of no known kind
        for text in ["{}", r#"{"https://auth.x.ai::c": {"key": ""}}"#, r#"{"other": {"key": "abc"}}"#, "[]"] {
            let a = read_auth(&auth_file(&d, text), now);
            assert!(a.token.is_empty(), "{text}");
            assert!(a.status == "NO LOGIN" || a.status == "BAD AUTH FILE", "{text}: {}", a.status);
        }
        assert_eq!(read_auth(&auth_file(&d, "{}"), now).hint, "AUTH FILE HAS NO TOKENS. RUN: grok login");
        // 4. an API key or a cookie jar in the key slot
        for key in ["xai-notARealKey000", "sso=abc; sso-rw=def", "a=b; c=d"] {
            let a = read_auth(&auth_file(&d, &format!(r#"{{"https://auth.x.ai::c": {{"key": "{key}", "expires_at": "2099-01-01T00:00:00Z"}}}}"#)), now);
            assert_eq!(
                (a.status.as_str(), a.hint.as_str(), a.token.as_str()),
                ("NO PLAN ACCESS", "GROK IS LOGGED IN WITH AN API KEY, NOT A SUBSCRIPTION. RUN: grok login", ""),
                "{key}"
            );
        }
        // 5. the fixture: an OIDC login with 6 h left
        let a = read_auth(&auth_file(&d, AUTH), now);
        assert_eq!((a.status.as_str(), a.kind.as_str(), a.principal.as_str(), a.expires, a.has_refresh), ("", "oidc", "User", EXPIRES, true));
        assert_eq!((a.token, a.created.as_str()), (fixture_key(), "2026-09-08T14:20:55.031303Z"));
        // 6. expired 5 min early, as the CLI does; one second before that it is still good
        let a = read_auth(&auth_file(&d, AUTH), EXPIRES - EARLY_SECS);
        assert_eq!((a.status.as_str(), a.hint.as_str(), a.token.as_str(), a.expires), ("TOKEN EXPIRED", "OPEN GROK ONCE, IT REFRESHES THE TOKEN", "", EXPIRES));
        assert!(!read_auth(&auth_file(&d, AUTH), EXPIRES - EARLY_SECS - 1).token.is_empty());
        assert_eq!(read_auth(&auth_file(&d, AUTH), EXPIRES + 86400).status, "TOKEN EXPIRED");
        // 7. the legacy session scope, with the +00:00 stamp form
        let a = read_auth(
            &auth_file(
                &d,
                r#"{"https://accounts.x.ai/sign-in": {"key": "legacy-session-key", "expires_at": "2026-09-08T20:20:55.031303+00:00", "principal_type": "Team"}}"#,
            ),
            now,
        );
        assert_eq!(
            (a.status.as_str(), a.kind.as_str(), a.principal.as_str(), a.expires, a.has_refresh, a.token.as_str()),
            ("", "legacy", "Team", EXPIRES, false, "legacy-session-key")
        );
        // 8. no expires_at: the JWT's exp claim (one second before the file's stamp); neither: call and let the server say
        let a = read_auth(&auth_file(&d, &format!(r#"{{"https://auth.x.ai::c": {{"key": "{}"}}}}"#, fixture_key())), now);
        assert_eq!((a.status.as_str(), a.expires), ("", EXPIRES - 1));
        assert_eq!(read_auth(&auth_file(&d, &format!(r#"{{"https://auth.x.ai::c": {{"key": "{}"}}}}"#, fixture_key())), EXPIRES).status, "TOKEN EXPIRED");
        let a = read_auth(&auth_file(&d, r#"{"https://auth.x.ai::c": {"key": "opaque-token"}}"#), now);
        assert_eq!((a.status.as_str(), a.expires, a.token.as_str()), ("", 0, "opaque-token"));
        // 9. preference: OIDC over legacy over an SSO issuer; an SSO issuer alone is accepted
        let both =
            r#"{"https://accounts.x.ai/sign-in": {"key": "old"}, "https://sso.example.com::cid": {"key": "sso"}, "https://auth.x.ai::cid": {"key": "new"}}"#;
        assert_eq!(read_auth(&auth_file(&d, both), now).token, "new");
        let a = read_auth(&auth_file(&d, r#"{"https://sso.example.com::cid": {"key": "sso"}, "https://accounts.x.ai/sign-in": {"key": "old"}}"#), now);
        assert_eq!((a.token.as_str(), a.kind.as_str()), ("old", "legacy"));
        let a = read_auth(&auth_file(&d, r#"{"https://sso.example.com::cid": {"key": "sso"}}"#), now);
        assert_eq!((a.token.as_str(), a.kind.as_str()), ("sso", "sso"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn real_reply() {
        let c = v(REPLY);
        assert!(reply_ok(&c));
        let w = windows(&c, CAPTURED);
        assert_eq!(rows(&w), vec![("WEEK".into(), "1.0".into(), Some(RESET.into()))]);
        assert_eq!(w[0].pct, json!(1.0)); // as the reply wrote it
                                          // one product mirroring the pool and an on-demand cap of 0 add nothing
        assert_eq!(w.len(), 1);
        // the percent is explicit, so the window stays after the period (the cache is stale, not empty)
        assert_eq!(windows(&c, PERIOD_END + 3600).len(), 1);
        assert_eq!(plan_of_reply(&c), "");
        assert_eq!(plan_of_settings(&v(SETTINGS)), "X PREMIUM+");
        assert_eq!(
            v(DENIED)["body"]["error"].as_str().unwrap(),
            "Invalid or expired credentials (auth_kind=bearer, x_xai_token_auth=xai-grok-cli, upstream=PermissionDenied, reason=no auth context)"
        );
    }

    #[test]
    fn zero_usage() {
        let c = v(ZERO);
        assert!(c["config"].get("creditUsagePercent").is_none());
        let w = windows(&c, CAPTURED);
        assert_eq!(rows(&w), vec![("WEEK".into(), "0".into(), Some(RESET.into()))]);
        assert_eq!(rows(&windows(&c, PERIOD_START)), rows(&w)); // the first second of the period counts
                                                                // outside the period a missing percent is unknown, not 0
        assert!(windows(&c, PERIOD_END).is_empty());
        assert!(windows(&c, PERIOD_START - 1).is_empty());
    }

    #[test]
    fn monthly_ondemand_products_and_stamps() {
        let c = v(r#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_MONTHLY","start":"2026-09-01T00:00:00Z","end":"2026-10-01T00:00:00Z"},
            "creditUsagePercent":42.5,"onDemandCap":{"val":2000},"onDemandUsed":{"val":500},
            "productUsage":[{"product":"GrokBuild","usagePercent":40},{"product":"GrokVoice","usagePercent":2.5}],
            "subscriptionTier":"supergrok_heavy"}}"#);
        assert_eq!(
            rows(&windows(&c, CAPTURED)),
            vec![
                ("MONTH".into(), "42.5".into(), Some("2026-10-01T00:00:00Z".into())),
                ("ONDEMAND".into(), "25.0".into(), Some("2026-10-01T00:00:00Z".into())),
                ("BUILD".into(), "40".into(), Some("2026-10-01T00:00:00Z".into())),
                ("VOICE".into(), "2.5".into(), Some("2026-10-01T00:00:00Z".into())),
            ]
        );
        assert_eq!(plan_of_reply(&c), "SUPERGROK HEAVY");
        assert_eq!(plan_of_reply(&v(r#"{"subscriptionTier":"SUPERGROK","config":{}}"#)), "SUPERGROK");
        // one product that differs from the pool is shown; clamping at both ends, on-demand over its cap too
        let c = v(r#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","end":"2026-09-14T17:30:18Z"},
            "creditUsagePercent":120,"onDemandCap":{"val":100},"onDemandUsed":{"val":150},
            "productUsage":[{"product":"GrokBuild","usagePercent":-3}, "junk", {"usagePercent":7}]}}"#);
        assert_eq!(
            rows(&windows(&c, CAPTURED)),
            vec![
                ("WEEK".into(), "100".into(), Some("2026-09-14T17:30:18Z".into())),
                ("ONDEMAND".into(), "100".into(), Some("2026-09-14T17:30:18Z".into())),
                ("BUILD".into(), "0".into(), Some("2026-09-14T17:30:18Z".into())),
                ("PRODUCT".into(), "7".into(), Some("2026-09-14T17:30:18Z".into()))
            ]
        );
        // no type: the label from the span; both stamp forms parse to the same instant
        let c = v(r#"{"config":{"currentPeriod":{"start":"2026-09-07T17:30:18.071364+00:00","end":"2026-09-14T17:30:18Z"},"creditUsagePercent":5}}"#);
        assert_eq!(rows(&windows(&c, CAPTURED)), vec![("WEEK".into(), "5".into(), Some("2026-09-14T17:30:18Z".into()))]);
        assert_eq!(period_label("", Some(0), Some(30 * 86400)), "MONTH");
        assert_eq!(period_label("", Some(0), Some(2 * 86400)), "2D");
        assert_eq!(period_label("", Some(0), Some(7200)), "2H");
        assert_eq!(period_label("", None, None), "WEEK");
        assert_eq!(period_label("USAGE_PERIOD_TYPE_MONTHLY", Some(0), Some(604800)), "MONTH");
        // no period at all: an explicit percent still makes the pool, resetting at billingPeriodEnd; a missing one is unknown
        let c = v(r#"{"config":{"creditUsagePercent":9,"billingPeriodEnd":"2026-09-14T17:30:18+00:00"}}"#);
        assert_eq!(rows(&windows(&c, CAPTURED)), vec![("WEEK".into(), "9".into(), Some("2026-09-14T17:30:18+00:00".into()))]);
        assert!(windows(&v(r#"{"config":{"billingPeriodEnd":"2026-09-14T17:30:18+00:00"}}"#), CAPTURED).is_empty());
        assert!(windows(&v(r#"{"config":{}}"#), CAPTURED).is_empty());
        // the billing period bounds stand in for a missing currentPeriod
        let c = v(r#"{"config":{"billingPeriodStart":"2026-09-07T17:30:18Z","billingPeriodEnd":"2026-09-14T17:30:18Z"}}"#);
        assert_eq!(rows(&windows(&c, CAPTURED)), vec![("WEEK".into(), "0".into(), Some("2026-09-14T17:30:18Z".into()))]);
        // no config: nothing, and not a reply to accept
        for text in ["{}", r#"{"config":null}"#, r#"{"config":"x"}"#, r#"{"error":"upstream error"}"#] {
            assert!(windows(&v(text), CAPTURED).is_empty(), "{text}");
            assert!(!reply_ok(&v(text)), "{text}");
        }
    }

    #[test]
    fn plan_labels_and_urls() {
        assert_eq!(plan_label("X Premium+"), "X PREMIUM+");
        assert_eq!(plan_label("SuperGrok Heavy"), "SUPERGROK HEAVY");
        assert_eq!(plan_label("supergrok_heavy"), "SUPERGROK HEAVY");
        assert_eq!(plan_label("heavy"), "SUPERGROK HEAVY");
        assert_eq!(plan_label("SUPERGROK"), "SUPERGROK");
        assert_eq!(plan_label("supergrok"), "SUPERGROK");
        assert_eq!(plan_label(" x_basic "), "X BASIC");
        assert_eq!(plan_label(""), "");
        assert_eq!(plan_of_settings(&v(r#"{"allow_access":false}"#)), "");
        assert_eq!(plan_of_settings(&v(r#"{"subscription_tier_display":null}"#)), "");
        assert_eq!(billing_url(BASE_URL), "https://cli-chat-proxy.grok.com/v1/billing?format=credits");
        assert_eq!(settings_url(BASE_URL), "https://cli-chat-proxy.grok.com/v1/settings");
        assert!(!not_a_login(&fixture_key()));
        assert!(not_a_login("xai-abc"));
    }

    #[test]
    fn http_paths() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("grok-http");
        let mut st = Store::new("grok");
        fetch(&mut st, &serve(200, REPLY), "tok");
        assert_eq!((st.status.as_str(), st.source.as_str(), st.cache_age), ("", "LIVE", 0));
        assert!(st.cache.is_file());
        let d = st.emit("X PREMIUM+", windows(&st.cached().unwrap(), CAPTURED), Value::Null);
        assert_eq!((d.plan.as_str(), d.windows.len(), d.history), ("X PREMIUM+", 1, vec![(st.now, 1)]));
        assert_eq!(std::fs::read_to_string(&st.history).unwrap(), format!("{}\t1\t1\n", st.now)); // WEEK is both the bar's number and the week column
                                                                                                  // 401 with the body the proxy really sends; the last good reply stays
        let mut st = Store::new("grok");
        st.cache_age = 1000;
        fetch(&mut st, &serve(401, denied_body()), "tok");
        assert_eq!((st.status.as_str(), st.hint.as_str()), ("TOKEN EXPIRED", "OPEN GROK ONCE, IT REFRESHES THE TOKEN"));
        assert!(st.cached().is_some());
        let last = v(&std::fs::read_to_string(&st.last_reply).unwrap());
        assert_eq!(last["http"], 401);
        assert!(last["body"]["error"].as_str().unwrap().starts_with("Invalid or expired credentials"));
        // 429: back off, silent under 5 min of cache, RATE LIMITED beyond
        let mut st = Store::new("grok");
        st.cache_age = 100;
        fetch(&mut st, &serve(429, "{}"), "tok");
        assert_eq!(st.status, "");
        assert!(st.backing_off());
        st.cache_age = 1000;
        fetch(&mut st, &serve(429, "{}"), "tok");
        assert_eq!(st.status, "RATE LIMITED");
        // shapes that are not the billing document
        let mut st = Store::new("grok");
        fetch(&mut st, &serve(200, "{\"unexpected\":true}"), "tok");
        assert_eq!((st.status.as_str(), st.hint.as_str()), ("BAD RESPONSE", "THE USAGE ENDPOINT CHANGED SHAPE"));
        let mut st = Store::new("grok");
        fetch(&mut st, &serve(200, "not json"), "tok");
        assert_eq!(st.status, "BAD RESPONSE");
        let mut st = Store::new("grok");
        fetch(&mut st, &refused(), "tok");
        assert_eq!((st.status.as_str(), st.hint.as_str()), ("NETWORK", "COULD NOT REACH CLI-CHAT-PROXY.GROK.COM"));
        let mut st = Store::new("grok");
        fetch(&mut st, &serve(403, "{}"), "tok");
        assert_eq!((st.status.as_str(), st.hint.as_str()), ("NO PLAN ACCESS", "THIS LOGIN CANNOT READ PLAN LIMITS. LOG IN TO GROK WITH A SUBSCRIPTION"));
        let mut st = Store::new("grok");
        fetch(&mut st, &serve(404, "{}"), "tok");
        assert_eq!(st.status, "NO USAGE DATA");
        let mut st = Store::new("grok");
        fetch(&mut st, &serve(502, "upstream error"), "tok");
        assert_eq!((st.status.as_str(), st.hint.as_str()), ("HTTP 502", "UNEXPECTED ANSWER FROM GROK.COM"));
        // a reply with config but nothing in it is accepted and reads as NO LIMITS IN REPLY
        let mut st = Store::new("grok");
        fetch(&mut st, &serve(200, "{\"config\":{}}"), "tok");
        assert_eq!(st.status, "");
        let d = st.emit("", windows(&st.cached().unwrap(), CAPTURED), Value::Null);
        assert_eq!((d.status.as_str(), d.hint.as_str()), ("NO LIMITS IN REPLY", "THE USAGE REPLY HAD NO WINDOWS. RUN: pulse-limits doctor"));
        drop(s);
    }

    #[test]
    fn settings_lookup() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("grok-plan");
        let now = crate::util::now();
        assert!(plan_stale(now));
        assert_eq!(plan_cached(), None);
        lookup_plan(&serve(200, SETTINGS), "tok");
        assert_eq!(plan_cached().as_deref(), Some("X PREMIUM+"));
        assert!(!plan_stale(now));
        assert!(plan_stale(crate::util::now() + PLAN_TTL + 1)); // from the clock now: the file may be a second newer than `now`
                                                                // a settings reply naming no tier: looked up, nothing to show
        lookup_plan(&serve(200, "{\"allow_access\":false}"), "tok");
        assert_eq!(plan_cached().as_deref(), Some(""));
        // an error keeps the last label; no answer leaves the file alone
        lookup_plan(&serve(200, SETTINGS), "tok");
        lookup_plan(&serve(503, "down"), "tok");
        assert_eq!(plan_cached().as_deref(), Some("X PREMIUM+"));
        std::fs::remove_file(s.cache().join("plan-grok")).unwrap();
        lookup_plan(&refused(), "tok");
        assert_eq!(plan_cached(), None);
        drop(s);
    }

    #[test]
    fn run_live_then_cached() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("grok-run");
        let home = s.0.join("grok-home");
        std::fs::create_dir_all(&home).unwrap();
        auth_file(&home, &auth_expiring("2099-01-01T00:00:00Z"));
        // first run: the billing GET, then the settings GET for the plan
        let env = Env::new(&home, &serve_routes(2, vec![("/billing?format=credits", 200, REPLY), ("/settings", 200, SETTINGS)]));
        let d = run(270);
        assert_eq!((d.provider.as_str(), d.status.as_str(), d.hint.as_str(), d.source.as_str(), d.plan.as_str()), ("grok", "", "", "LIVE", "X PREMIUM+"));
        assert_eq!(rows(&d.windows), vec![("WEEK".into(), "1.0".into(), Some(RESET.into()))]);
        assert_eq!(d.credits, Value::Null);
        assert_eq!(d.session().unwrap().label, "WEEK"); // no SESSION window: the first one is the bar's number
        assert_eq!(std::fs::read_to_string(s.cache().join("plan-grok")).unwrap(), "X PREMIUM+\n");
        assert!(s.cache().join("usage-grok.json").is_file());
        assert!(s.cache().join("last-reply-grok.json").is_file());
        // second run: the cache is fresh, nothing is called (a call would hit a closed port and say NETWORK)
        env.base(&refused());
        let d = run(270);
        assert_eq!((d.status.as_str(), d.source.as_str(), d.plan.as_str(), d.windows.len()), ("", "CACHE", "X PREMIUM+", 1));
        // a due call within the hour asks billing only: the plan file keeps its label
        std::fs::remove_file(s.cache().join("usage-grok.json")).unwrap();
        env.base(&serve_routes(2, vec![("/billing?format=credits", 200, ZERO), ("/settings", 200, "{\"subscription_tier_display\":\"Changed\"}")]));
        let d = run(270);
        assert_eq!((d.status.as_str(), d.source.as_str(), d.plan.as_str()), ("", "LIVE", "X PREMIUM+"));
        assert_eq!(std::fs::read_to_string(s.cache().join("plan-grok")).unwrap(), "X PREMIUM+\n");
        // the plan from a billing reply fills in when settings named none
        std::fs::write(s.cache().join("plan-grok"), "\n").unwrap();
        std::fs::write(
            s.cache().join("usage-grok.json"),
            r#"{"config":{"subscriptionTier":"supergrok","creditUsagePercent":3,"billingPeriodEnd":"2099-01-08T00:00:00Z"}}"#,
        )
        .unwrap();
        let d = run(270);
        assert_eq!((d.source.as_str(), d.plan.as_str(), d.windows[0].pct.to_string().as_str()), ("CACHE", "SUPERGROK", "3"));
        drop(env);
        drop(s);
    }

    #[test]
    fn run_without_a_usable_token_makes_no_call() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("grok-nocall");
        let home = s.0.join("grok-home");
        std::fs::create_dir_all(&home).unwrap();
        let env = Env::new(&home, &refused());
        // no auth file
        let d = run(270);
        assert_eq!((d.status.as_str(), d.source.as_str(), d.fetched, d.windows.len()), ("NO LOGIN", "", 0, 0));
        assert!(d.hint.starts_with("NO GROK LOGIN ON THIS"));
        assert!(!s.cache().join("last-reply-grok.json").exists());
        // an expires_at in the past (the run uses the real clock)
        auth_file(&home, &auth_expiring("2020-01-01T00:00:00Z"));
        let d = run(270);
        assert_eq!((d.status.as_str(), d.hint.as_str()), ("TOKEN EXPIRED", "OPEN GROK ONCE, IT REFRESHES THE TOKEN"));
        assert!(!s.cache().join("last-reply-grok.json").exists());
        // an API key
        auth_file(&home, r#"{"https://auth.x.ai::c": {"key": "xai-notARealKey000"}}"#);
        let d = run(270);
        assert_eq!(d.status, "NO PLAN ACCESS");
        assert!(!s.cache().join("last-reply-grok.json").exists());
        drop(env);
        drop(s);
    }

    #[test]
    fn run_rate_limited_backs_off() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("grok-429");
        let home = s.0.join("grok-home");
        std::fs::create_dir_all(&home).unwrap();
        auth_file(&home, &auth_expiring("2099-01-01T00:00:00Z"));
        let env = Env::new(&home, &serve(429, "{\"error\":\"slow down\"}"));
        let d = run(270);
        assert_eq!((d.status.as_str(), d.hint.as_str(), d.source.as_str()), ("RATE LIMITED", "TOO MANY USAGE CALLS FOR THIS ACCOUNT, RETRYING IN 3 MIN", ""));
        let until: i64 = std::fs::read_to_string(s.cache().join("backoff-grok")).unwrap().trim().parse().unwrap();
        assert!(until > crate::util::now() + BACKOFF_SECS - 5);
        // the second run makes no call while the backoff runs (a call would say NETWORK)
        env.base(&refused());
        let d = run(270);
        assert_eq!((d.status.as_str(), d.hint.as_str()), ("RATE LIMITED", "BACKING OFF AFTER A 429, RETRYING IN A FEW MINUTES"));
        let last = v(&std::fs::read_to_string(s.cache().join("last-reply-grok.json")).unwrap());
        assert_eq!(last["http"], 429);
        // the 401 the proxy really sends, through run()
        std::fs::remove_file(s.cache().join("backoff-grok")).unwrap();
        env.base(&serve(401, denied_body()));
        let d = run(270);
        assert_eq!((d.status.as_str(), d.source.as_str()), ("TOKEN EXPIRED", ""));
        drop(env);
        drop(s);
    }

    #[test]
    fn doctor_reports_the_login_the_plan_and_the_endpoint() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Sandbox::new("grok-doc");
        let home = s.home().join(".grok");
        std::fs::create_dir_all(&home).unwrap();
        let base = std::env::var("GROK_CLI_CHAT_PROXY_BASE_URL").unwrap();
        let out = capture(|| doctor(""));
        assert!(out.contains(&format!("  ok       GROK_HOME={} in this shell", home.display())), "{out}");
        assert!(out.contains(&format!("  ok       GROK_CLI_CHAT_PROXY_BASE_URL={base} in this shell")), "{out}");
        assert!(out.contains(&format!("  PROBLEM  no {}: run 'grok login' (or set GROK_HOME", home.join("auth.json").display())), "{out}");
        assert!(out.contains("  ok       plan: not looked up yet (asked after the first good billing reply, then hourly)\n"), "{out}");
        assert!(out.contains("  ok       grok CLI not found (only needed to log in)\n"), "{out}");
        assert!(out.contains(&format!("  ok       usage url: {base}/billing?format=credits\n")), "{out}");
        assert!(out.ends_with("  usage api\n  PROBLEM  skipped (no usable token)\n  last reply (shape digest)\n  PROBLEM  no reply cached yet\n"), "{out}");
        // an API key in the file, and the CLI under GROK_HOME/bin (found, never run)
        std::fs::create_dir_all(home.join("bin")).unwrap();
        std::os::unix::fs::symlink(which("true").unwrap(), home.join("bin").join("grok")).unwrap();
        auth_file(&home, r#"{"https://auth.x.ai::c": {"key": "xai-notARealKey000"}}"#);
        let out = capture(|| doctor(""));
        assert!(out.contains(": NO PLAN ACCESS (GROK IS LOGGED IN WITH AN API KEY, NOT A SUBSCRIPTION. RUN: grok login)\n"), "{out}");
        assert!(out.contains(&format!("  ok       grok CLI: {} (never run from here", home.join("bin").join("grok").display())), "{out}");
        // the fixture login; a settings lookup that named no tier, then one that did
        auth_file(&home, &auth_expiring("2099-01-01T00:00:00Z"));
        std::fs::write(s.scratch.cache().join("plan-grok"), "\n").unwrap();
        let out = capture(|| doctor(""));
        assert!(
            out.contains(": oidc login, principal User, token expires 2099-01-01T00:00:00Z, created 2026-09-08T14:20:55.031303Z, refresh token present\n"),
            "{out}"
        );
        assert!(out.contains("  ok       plan: the settings reply named no tier\n"), "{out}");
        assert!(out.contains("  usage api\n  ok       reached through the plugin (not probed again: keep the calls rare)\n"), "{out}");
        std::fs::write(s.scratch.cache().join("plan-grok"), "X PREMIUM+\n").unwrap();
        let out = capture(|| doctor(""));
        assert!(out.contains(&format!("  ok       plan: X PREMIUM+ (from {base}/settings, 0 min old)\n")), "{out}");
        // backing off: not probed
        Store::new("grok").set_backoff(180);
        assert!(capture(|| doctor("RATE LIMITED")).contains("  ok       not probed: backing off after a 429 until "));
        std::fs::remove_file(s.scratch.cache().join("backoff-grok")).unwrap();
        // the probe, one answer per call
        for (url, line) in [
            (serve(200, REPLY), "  ok       HTTP 200: {\"keys\":\"config\",\"period\":\"USAGE_PERIOD_TYPE_WEEKLY\",\"creditUsagePercent\":1.0,\"onDemandCap\":0,\"error\":null}\n"),
            (serve(200, "nope"), "  ok       HTTP 200: unexpected JSON shape\n"),
            (serve(401, denied_body()), "  PROBLEM  HTTP 401: the token is expired or revoked. Open the Grok CLI once, or run 'grok login'.\n"),
            (serve(403, "{}"), "  PROBLEM  HTTP 403: this login cannot read plan limits"),
            (serve(429, "{}"), "  PROBLEM  HTTP 429 rate limited: too many usage calls recently."),
            (refused(), "  PROBLEM  no answer from cli-chat-proxy.grok.com\n"),
            (serve(502, "upstream\nerror"), "  PROBLEM  HTTP 502: upstream error\n"),
        ] {
            std::env::set_var("GROK_CLI_CHAT_PROXY_BASE_URL", url);
            let out = capture(|| doctor("HTTP 502"));
            assert!(out.contains(line), "{out}");
        }
        // the digest of a cached reply, and no override line when the base is the default
        let mut st = Store::new("grok");
        fetch(&mut st, &serve(200, REPLY), "tok");
        std::env::remove_var("GROK_CLI_CHAT_PROXY_BASE_URL");
        let out = capture(|| doctor(""));
        assert!(!out.contains("GROK_CLI_CHAT_PROXY_BASE_URL="), "{out}");
        assert!(out.contains(&format!("  ok       usage url: {BASE_URL}/billing?format=credits\n")), "{out}");
        assert!(out.contains("  ok       cached reply (0 min old): {\"keys\":\"config\",\"period\":\"USAGE_PERIOD_TYPE_WEEKLY\",\"creditUsagePercent\":1.0,\"onDemandCap\":0,\"error\":null}\n"), "{out}");
        drop(s);
    }
}
