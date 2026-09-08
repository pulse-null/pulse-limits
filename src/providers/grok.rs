//! Grok: the xAI login the Grok CLI keeps in ~/.grok/auth.json (GROK_HOME moves that folder)
//! and the billing endpoint the CLI itself reads its weekly credit pool from, behind the
//! cli-chat-proxy. Neither is documented; the file and reply shapes follow what the CLI and
//! CodexBar (MIT) read. The only things that leave this machine are one GET to
//! cli-chat-proxy.grok.com for the pool and, at most hourly, one more for the plan name.
//!
//! The token lasts 6 h and the CLI renews it only while it runs. When it is about to expire and
//! the CLI is away (no `grok` process, no live pid in auth.json.lock or active_sessions.json),
//! this reader renews it the way the CLI does: the issuer's OIDC token endpoint, the refresh
//! token, no secret, and auth.json rewritten with the new key and nothing else changed. Never
//! twice from the same token, never while the CLI could be doing the same, and a refusal leaves
//! the file exactly as it was: only the CLI deletes a login. The `grok` binary is never spawned:
//! any run may self-update, sync its config, or rewrite the login.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::providers::codex::jwt_claims;
use crate::providers::{bad, ok, say, Doc, Store, Window, BACKOFF_SECS, PROBE_DELAY_SECS};
use crate::util::{
    cache_dir, env_path, epoch_of, hhmmss, home, is_executable, is_macos, iso_utc, local_offset, mtime, process_running, read_trimmed, run_quiet, upper, which,
    write_atomic,
};

pub const BASE_URL: &str = "https://cli-chat-proxy.grok.com/v1";
pub const TOKEN_AUTH: &str = "xai-grok-cli"; // X-XAI-Token-Auth: "validate as a CLI session token"
pub const EARLY_SECS: i64 = 300; // the CLI's own GROK_AUTH_EARLY_INVALIDATION_SECS
pub const PLAN_TTL: i64 = 3600; // the plan name is asked at most hourly
pub const EXPIRED_HINT: &str = "OPEN GROK ONCE, IT REFRESHES THE TOKEN";
pub const REFRESH_TIMEOUT_SECS: u64 = 8; // discovery and the token POST each get this long
pub const DISCOVERY_TTL: i64 = 86400; // the token endpoint is asked of the issuer at most daily
pub const TOKEN_LIFETIME: i64 = 21600; // 6 h: what a reply without expires_in and a token without exp is given

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
    pub scope: String,     // the entry's key in the file
    pub refresh: String,   // refresh_token, empty when absent; only ever sent to the issuer
    pub issuer: String,    // oidc_issuer, else the scope's issuer half; empty for a legacy login
    pub client_id: String, // oidc_client_id, else the scope's client half
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
    a.refresh = str_of(&entry, "refresh_token").unwrap_or_default();
    a.has_refresh = !a.refresh.is_empty();
    let halves = scope.split_once("::");
    a.issuer = str_of(&entry, "oidc_issuer").or_else(|| halves.map(|(i, _)| i.to_string())).unwrap_or_default().trim_end_matches('/').to_string();
    a.client_id = str_of(&entry, "oidc_client_id").or_else(|| halves.map(|(_, c)| c.to_string())).unwrap_or_default();
    a.scope = scope;
    let key = str_of(&entry, "key").unwrap_or_default();
    if not_a_login(&key) {
        a.status = "NO PLAN ACCESS".into();
        a.hint = "GROK IS LOGGED IN WITH AN API KEY, NOT A SUBSCRIPTION. RUN: grok login".into();
        return a;
    }
    a.expires = expiry_of(&entry);
    if a.expires > 0 && now >= a.expires - EARLY_SECS {
        a.status = "TOKEN EXPIRED".into();
        a.hint = EXPIRED_HINT.into();
    } else {
        a.token = key;
    }
    a
}

/// When an entry's token dies: expires_at, else the JWT's exp claim, else 0 (unknown).
fn expiry_of(entry: &Value) -> i64 {
    str_of(entry, "expires_at")
        .and_then(|s| epoch_of(&s))
        .or_else(|| jwt_claims(&str_of(entry, "key").unwrap_or_default()).and_then(|c| c.get("exp").and_then(Value::as_f64)).map(|e| e as i64))
        .unwrap_or(0)
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
    // productUsage (Build, Chat, Imagine, Voice) is not shown: the pool is the meter, the rest is noise.
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
        401 => st.set("TOKEN EXPIRED", EXPIRED_HINT),
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

/// The prepaid balance as the credits caption. `used` and `currency` are what the panel, the
/// tooltip and the TUI print ("CREDITS 1097.00 PREPAID"); `balance` is the honest name: credits
/// bought, not spend, so never a percentage. Null without one; the on-demand cap is a ring instead.
pub fn credits(c: &Value) -> Value {
    match c.get("config").and_then(|c| c.get("prepaidBalance")).and_then(|m| m.get("val")).filter(|v| v.as_f64().is_some_and(|b| b > 0.0)) {
        Some(balance) => json!({ "used": balance, "currency": "PREPAID", "balance": balance }),
        None => Value::Null,
    }
}

/// The label to show: the settings lookup first, then the tier a billing reply may carry.
fn plan_of(cached: Option<&Value>) -> String {
    plan_cached().filter(|p| !p.is_empty()).unwrap_or_else(|| cached.map(plan_of_reply).unwrap_or_default())
}

fn refresh_file() -> PathBuf {
    cache_dir().join("refresh-grok.json")
}

/// What refresh-grok.json remembers: the issuer's token endpoint (asked at most daily) and the
/// last refresh attempt, keyed by the expiry of the token it started from, so one token is never
/// refreshed twice and a refusal is not tried again until the CLI writes a new login. Survives
/// `pulse-limits reset` on purpose.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct Memory {
    pub issuer: String,
    pub endpoint: String,
    pub discovered: i64, // epoch of the discovery; 0 = never
    pub from: i64,       // expiry (epoch) of the token the last attempt started from; 0 = no attempt yet
    pub at: i64,
    pub result: String, // "refreshed", "refresh refused (invalid_grant)", "refresh got no answer", …
    pub retry: bool,    // no answer or a server error: the next due call may try again
    pub expires: i64,   // the new token's expiry after "refreshed"
}

pub fn memory() -> Memory {
    let v = fs::read(refresh_file()).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).unwrap_or(Value::Null);
    let s = |k: &str| str_of(&v, k).unwrap_or_default();
    let n = |k: &str| v.get(k).and_then(Value::as_i64).unwrap_or(0);
    Memory {
        issuer: s("issuer"),
        endpoint: s("token_endpoint"),
        discovered: n("discovered"),
        from: n("from"),
        at: n("at"),
        result: s("result"),
        retry: v.get("retry").and_then(Value::as_bool).unwrap_or(false),
        expires: n("expires"),
    }
}

fn remember(m: &Memory) {
    let v = json!({ "issuer": m.issuer, "token_endpoint": m.endpoint, "discovered": m.discovered, "from": m.from, "at": m.at,
                    "result": m.result, "retry": m.retry, "expires": m.expires });
    let _ = write_atomic(&refresh_file(), format!("{v}\n").as_bytes());
}

/// `kill -0`: is that process id alive? Another user's process says no; the CLI runs as us.
pub fn pid_alive(pid: i64) -> bool {
    pid > 0 && run_quiet("kill", &["-0", &pid.to_string()])
}

/// Why the CLI may be about to refresh the token itself: a `grok` process, a live holder of
/// auth.json.lock (`<pid>:<unix seconds>`), or a live pid in active_sessions.json. A dead pid
/// in either file is a crash's leftover and blocks nothing. None: the CLI is away.
pub fn cli_around(dir: &Path) -> Option<String> {
    if process_running("grok") {
        return Some("grok is running".into());
    }
    if let Some(text) = read_trimmed(&dir.join("auth.json.lock")) {
        match text.split(':').next().and_then(|p| p.trim().parse::<i64>().ok()) {
            Some(pid) if pid_alive(pid) => return Some(format!("auth.json.lock is held by pid {pid}")),
            Some(_) => {}
            None => return Some("auth.json.lock names no pid".into()),
        }
    }
    let sessions = fs::read(dir.join("active_sessions.json")).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let mut pids = sessions.as_ref().and_then(Value::as_array).into_iter().flatten().filter_map(|s| s.get("pid").and_then(Value::as_i64));
    pids.find(|p| pid_alive(*p)).map(|p| format!("a grok session is open (pid {p})"))
}

/// What the login lacks for a refresh, if anything.
fn cannot_refresh(auth: &Auth) -> Option<&'static str> {
    if auth.refresh.is_empty() {
        Some("no refresh token")
    } else if auth.issuer.is_empty() {
        Some("no issuer in the login")
    } else if auth.client_id.is_empty() {
        Some("no client id in the login")
    } else {
        None
    }
}

/// What the last attempt on this very token left, and whether it may be tried again.
fn tried(auth: &Auth, mem: &Memory, now: i64) -> Option<(String, bool)> {
    if mem.from == 0 || mem.from != auth.expires {
        return None;
    }
    let phrase = match (mem.retry, mem.result.as_str()) {
        (true, r) => format!("{r} {} ago", age(now - mem.at)),
        (false, "refreshed") => "already refreshed from this token, yet auth.json still holds it".into(),
        (false, r) => r.into(),
    };
    Some((phrase, mem.retry))
}

/// "4h12m" / "2h" / "3m" / "40s": the doctor's ages.
pub fn age(secs: i64) -> String {
    let s = secs.max(0);
    let (h, m) = (s / 3600, s % 3600 / 60);
    if h > 0 && m > 0 {
        format!("{h}h{m:02}m")
    } else if h > 0 {
        format!("{h}h")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{s}s")
    }
}

/// "auth.x.ai" out of "https://auth.x.ai/…": the host a status line names.
fn host_of(url: &str) -> String {
    url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or("").to_string()
}

/// Why a refresh gave no token; `retry` says whether the next due call may try again.
#[derive(Debug, PartialEq)]
enum Fail {
    Refused(String), // a 4xx: the refresh token is dead or the request wrong; asking again cannot help
    NoAnswer(&'static str),
    Http(&'static str, u16),
    Other(String), // a reply of the wrong shape or a file this user cannot write: worth another try
}

impl Fail {
    fn phrase(&self) -> String {
        match self {
            Fail::Refused(e) => format!("refresh refused ({e})"),
            Fail::NoAnswer(step) => format!("{step} got no answer"),
            Fail::Http(step, c) => format!("{step} got HTTP {c}"),
            Fail::Other(s) => s.clone(),
        }
    }

    fn retry(&self) -> bool {
        match self {
            Fail::Refused(_) => false,
            Fail::NoAnswer(_) | Fail::Http(..) | Fail::Other(_) => true,
        }
    }
}

/// One request to the issuer, a GET or a form POST, with an 8 s deadline. 0 = no answer.
fn issuer_call(url: &str, form: Option<&[(&str, &str)]>) -> (u16, Vec<u8>) {
    let cfg = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0) // a POST carrying the refresh token goes to the endpoint the issuer named, nowhere else
        .timeout_global(Some(Duration::from_secs(REFRESH_TIMEOUT_SECS)))
        .user_agent("pulse-limits")
        .build();
    let agent = ureq::Agent::new_with_config(cfg);
    let sent = match form {
        Some(f) => agent.post(url).header("Accept", "application/json").send_form(f.iter().copied()),
        None => agent.get(url).header("Accept", "application/json").call(),
    };
    match sent {
        Ok(mut r) => {
            let code = r.status().as_u16();
            (code, r.body_mut().read_to_vec().unwrap_or_default())
        }
        Err(_) => (0, vec![]),
    }
}

/// The issuer's token endpoint: remembered for a day, else from its discovery document.
fn token_endpoint(auth: &Auth, mem: &mut Memory, now: i64) -> Result<String, Fail> {
    if mem.issuer == auth.issuer && !mem.endpoint.is_empty() && now - mem.discovered < DISCOVERY_TTL {
        return Ok(mem.endpoint.clone());
    }
    let (code, body) = issuer_call(&format!("{}/.well-known/openid-configuration", auth.issuer), None);
    let endpoint = match code {
        200 => serde_json::from_slice::<Value>(&body).ok().and_then(|v| str_of(&v, "token_endpoint")),
        0 => return Err(Fail::NoAnswer("discovery")),
        c => return Err(Fail::Http("discovery", c)),
    };
    let Some(endpoint) = endpoint else { return Err(Fail::Other("discovery named no token endpoint".into())) };
    (mem.issuer, mem.endpoint, mem.discovered) = (auth.issuer.clone(), endpoint.clone(), now);
    Ok(endpoint)
}

/// A Home Manager link is refused (the CLI's file would become a plain one and the next switch
/// would stop on it), so is a file this user cannot open for writing; checked before any token
/// is spent, because a new token that cannot be saved would only log the CLI out.
fn writable(file: &Path) -> io::Result<()> {
    if fs::read_link(file).map(|t| t.starts_with("/nix/store")).unwrap_or(false) {
        return Err(io::Error::other("managed by Home Manager"));
    }
    fs::OpenOptions::new().write(true).open(file).map(drop)
}

/// The file rewritten through a sibling temp file created with mode 0600, as the CLI keeps it
/// (`write_atomic` would leave the umask's mode on a bearer token), and a rename.
fn write_auth(file: &Path, text: &str) -> io::Result<()> {
    writable(file)?;
    let tmp = file.with_extension(format!("json.tmp{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, file)
}

/// The refresh: the token endpoint, then the POST a public client makes (the refresh token and
/// the client id, no secret). Ok: the new key, the rotated refresh token if the issuer sent one,
/// and the seconds it lasts (expires_in, else the key's exp claim, else the known 6 h).
fn refresh(auth: &Auth, mem: &mut Memory, now: i64) -> Result<(String, Option<String>, i64), Fail> {
    writable(&auth.file).map_err(|e| Fail::Other(format!("auth.json is not writable: {e}")))?;
    let endpoint = token_endpoint(auth, mem, now)?;
    let form = [("grant_type", "refresh_token"), ("refresh_token", auth.refresh.as_str()), ("client_id", auth.client_id.as_str())];
    let (code, body) = issuer_call(&endpoint, Some(&form));
    let reply = match code {
        200 => serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null),
        0 => return Err(Fail::NoAnswer("refresh")),
        400..=499 => {
            let error = serde_json::from_slice::<Value>(&body).ok().and_then(|v| str_of(&v, "error")).unwrap_or_else(|| format!("HTTP {code}"));
            return Err(Fail::Refused(error.chars().take(40).collect())); // the issuer's word, kept short: it is printed
        }
        c => return Err(Fail::Http("refresh", c)),
    };
    let Some(key) = str_of(&reply, "access_token") else { return Err(Fail::Other("refresh reply had no token".into())) };
    let life = reply
        .get("expires_in")
        .and_then(Value::as_f64)
        .map(|s| s as i64)
        .or_else(|| jwt_claims(&key).and_then(|c| c.get("exp").and_then(Value::as_f64)).map(|e| e as i64 - now))
        .filter(|s| *s > 0)
        .unwrap_or(TOKEN_LIFETIME);
    Ok((key, str_of(&reply, "refresh_token"), life))
}

/// auth.json with the refreshed entry: key, expires_at, create_time and, when rotated, the
/// refresh token; every other key and entry as it was. Only if the file still holds the token
/// the refresh started from: the CLI may have written a newer login meanwhile, and then its wins.
fn write_back(auth: &Auth, key: &str, now: i64, expires: i64, rotated: Option<&str>) -> Result<(), String> {
    let text = fs::read_to_string(&auth.file).map_err(|e| format!("auth.json write failed: {e}"))?;
    let mut doc: Value = serde_json::from_str(&text).map_err(|e| format!("auth.json write failed: {e}"))?;
    let changed = || "auth.json changed during the refresh, not written".to_string();
    let entry = doc.get(&auth.scope).filter(|e| e.is_object()).ok_or_else(changed)?;
    if expiry_of(entry) != auth.expires || str_of(entry, "refresh_token").unwrap_or_default() != auth.refresh {
        return Err(changed());
    }
    let Some(obj) = doc.get_mut(&auth.scope).and_then(Value::as_object_mut) else { return Err(changed()) };
    obj.insert("key".into(), json!(key));
    obj.insert("expires_at".into(), json!(iso_utc(expires)));
    obj.insert("create_time".into(), json!(iso_utc(now)));
    if let Some(r) = rotated {
        obj.insert("refresh_token".into(), json!(r));
    }
    let out = serde_json::to_string_pretty(&doc).map_err(|e| format!("auth.json write failed: {e}"))?;
    write_auth(&auth.file, &format!("{out}\n")).map_err(|e| format!("auth.json write failed: {e}"))
}

/// One attempt, recorded in `mem` whatever happens. Ok is the bearer for this run, even when
/// saving it failed: the issuer did its part, and that token is not asked for again.
fn attempt(auth: &Auth, mem: &mut Memory, now: i64) -> Result<String, Fail> {
    (mem.from, mem.at, mem.expires, mem.retry) = (auth.expires, now, 0, false);
    match refresh(auth, mem, now) {
        Ok((key, rotated, life)) => {
            match write_back(auth, &key, now, now + life, rotated.as_deref()) {
                Ok(()) => (mem.result, mem.expires) = ("refreshed".into(), now + life),
                Err(e) => mem.result = e,
            }
            Ok(key)
        }
        Err(f) => {
            (mem.result, mem.retry) = (f.phrase(), f.retry());
            Err(f)
        }
    }
}

/// The bearer for this run, or None with the status and hint set: auth.json's key while it has
/// more than 5 min left; past that, one refreshed here when the login can be refreshed, the CLI
/// is away and this token was not tried before (or got no answer and a call is due again).
pub fn token(st: &mut Store, auth: &Auth, min_interval: i64) -> Option<String> {
    if !auth.token.is_empty() {
        return Some(auth.token.clone());
    }
    st.set(&auth.status, &auth.hint);
    if auth.status != "TOKEN EXPIRED" {
        return None;
    }
    let mut mem = memory();
    let due = st.due(min_interval);
    let dir = auth.file.parent().map(Path::to_path_buf).unwrap_or_default();
    if cannot_refresh(auth).is_some() || tried(auth, &mem, st.now).is_some_and(|(_, retry)| !(retry && due)) || cli_around(&dir).is_some() {
        return None;
    }
    let got = attempt(auth, &mut mem, st.now);
    remember(&mem);
    let host = upper(&host_of(&auth.issuer));
    match got {
        Ok(key) => {
            st.set("", "");
            Some(key)
        }
        Err(Fail::Refused(_)) => {
            st.set_backoff(BACKOFF_SECS);
            None
        }
        Err(Fail::NoAnswer(_)) => {
            st.set("NETWORK", &format!("COULD NOT REACH {host}"));
            None
        }
        Err(Fail::Http(_, c)) => {
            st.set(&format!("HTTP {c}"), &format!("UNEXPECTED ANSWER FROM {host}"));
            None
        }
        Err(Fail::Other(_)) => None,
    }
}

/// The doctor's line on the token: how it was obtained and when it expires, or why it is expired
/// and what became of the refresh. Ages and reasons only. None when there is no token to speak of.
pub fn token_line(auth: &Auth, mem: &Memory, now: i64) -> Option<(bool, String)> {
    if !auth.token.is_empty() {
        let left = if auth.expires > 0 { format!("expires in {}", age(auth.expires - now)) } else { "no expiry in the file".into() };
        let how = if mem.expires > 0 && mem.expires == auth.expires && mem.result == "refreshed" {
            format!("refreshed by pulse-limits {} ago", age(now - mem.at))
        } else {
            "from auth.json".into()
        };
        return Some((true, format!("token: {how}, {left}")));
    }
    if auth.status != "TOKEN EXPIRED" {
        return None;
    }
    let when = if now < auth.expires {
        format!("expires in {}, inside the CLI's 5 min margin", age(auth.expires - now))
    } else {
        format!("expired {} ago", age(now - auth.expires))
    };
    let dir = auth.file.parent().map(Path::to_path_buf).unwrap_or_default();
    let why = cannot_refresh(auth)
        .map(|r| format!("refresh skipped: {r}"))
        .or_else(|| tried(auth, mem, now).map(|(phrase, _)| phrase))
        .or_else(|| cli_around(&dir).map(|r| format!("refresh skipped: {r}")))
        .unwrap_or_else(|| "refresh not attempted yet".into());
    Some((false, format!("token: {when}, {why}")))
}

/// The document from the cache and, when due, one live call (plus the hourly plan lookup).
pub fn run(min_interval: i64) -> Doc {
    let mut st = Store::new("grok");
    let auth = read_auth(&grok_dir().join("auth.json"), st.now);
    let bearer = token(&mut st, &auth, min_interval);
    let base = base_url();
    if let Some(tok) = bearer.filter(|_| st.due(min_interval)) {
        fetch(&mut st, &billing_url(&base), &tok);
        // the plan name is not in the billing reply: ask /settings after a good one, at most hourly
        if st.source == "LIVE" && plan_stale(st.now) {
            lookup_plan(&base, &tok);
        }
    }
    let cached = st.cached();
    let windows = cached.as_ref().map(|c| windows(c, st.now)).unwrap_or_default();
    let credits = cached.as_ref().map(credits).unwrap_or(Value::Null);
    st.emit(&plan_of(cached.as_ref()), windows, credits)
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
    match token_line(&auth, &memory(), st.now) {
        Some((true, line)) => ok(&line),
        Some((false, line)) => bad(&line),
        None => {}
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
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Mutex};
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
    // issue #11: an expired login with the +00:00 stamps and fields to preserve, the issuer's
    // discovery document, the token replies (rotated, kept, refused), and the extra-usage shapes
    const EXPIRED: &str = include_str!("../../tests/fixtures/grok/auth-expired.synthetic.json");
    const DISCOVERY: &str = include_str!("../../tests/fixtures/grok/oidc-discovery.synthetic.json");
    const TOKEN_OK: &str = include_str!("../../tests/fixtures/grok/token-200.synthetic.json");
    const TOKEN_KEPT: &str = include_str!("../../tests/fixtures/grok/token-200-no-rotation.synthetic.json");
    const TOKEN_DEAD: &str = include_str!("../../tests/fixtures/grok/token-400-invalid-grant.synthetic.json");
    const CAPPED: &str = include_str!("../../tests/fixtures/grok/fixture-200-ondemand-cap.synthetic.json");
    const NO_EXTRAS: &str = include_str!("../../tests/fixtures/grok/fixture-200-no-extras.synthetic.json");

    const CAPTURED: i64 = 1788877617; // 2026-09-08T14:26:57Z, inside the fixture's period and token life
    const PERIOD_START: i64 = 1788802218; // 2026-09-07T17:30:18Z
    const PERIOD_END: i64 = 1789407018; // 2026-09-14T17:30:18Z
    const EXPIRES: i64 = 1788898855; // the fixture's expires_at, 2026-09-08T20:20:55Z
    const OLD_EXPIRES: i64 = 1577858400; // the expired login's expires_at, 2020-01-01T06:00:00Z
    const RESET: &str = "2026-09-14T17:30:18.071364+00:00";
    const SCOPE: &str = "https://auth.x.ai::00000000-0000-4000-8000-000000000000";
    const CLIENT: &str = "00000000-0000-4000-8000-000000000000";
    const WELL_KNOWN: &str = "/.well-known/openid-configuration";
    const TOKEN_PATH: &str = "/oauth2/token";
    const BILLING: &str = "/billing?format=credits";

    fn v(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    fn fixture_key() -> String {
        v(AUTH).as_object().unwrap().values().next().unwrap()["key"].as_str().unwrap().to_string()
    }

    fn new_key() -> String {
        v(TOKEN_OK)["access_token"].as_str().unwrap().to_string()
    }

    /// The OIDC entry of an auth.json text.
    fn entry(text: &str) -> Value {
        pick_entry(&v(text)).unwrap().1
    }

    /// `text` with its login's issuer pointed at `issuer` (a test server): a refresh never leaves this machine.
    fn issued_by(text: &str, issuer: &str) -> String {
        let mut a = v(text);
        let scope = pick_entry(&a).unwrap().0;
        a[&scope]["oidc_issuer"] = json!(issuer);
        a.to_string()
    }

    fn keys(o: &Value) -> Vec<String> {
        o.as_object().unwrap().keys().cloned().collect()
    }

    /// The login at GROK_HOME, the proxy on `base`, a fresh plan label (so no settings call).
    fn login(s: &Sandbox, text: &str, base: &str) -> PathBuf {
        let home = s.home().join(".grok");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("GROK_CLI_CHAT_PROXY_BASE_URL", base);
        std::fs::write(s.scratch.cache().join("plan-grok"), "X PREMIUM+\n").unwrap();
        auth_file(&home, &issued_by(text, base))
    }

    /// The bearer a request carried, if it did.
    fn bearer_of(req: &str) -> Option<String> {
        let prefix = "authorization: bearer ";
        req.lines().find(|l| l.to_ascii_lowercase().starts_with(prefix)).map(|l| l[prefix.len()..].to_string())
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
        spy(n, routes).0
    }

    /// The same, keeping every request (head and body) so a test can say what was sent, or that
    /// nothing was. `https://auth.x.ai` in a body becomes this server's URL, so the discovery
    /// fixture points the token endpoint here.
    fn spy(n: usize, routes: Vec<(&'static str, u16, &'static str)>) -> (String, Arc<Mutex<Vec<String>>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(vec![]));
        let (log, me) = (seen.clone(), url.clone());
        thread::spawn(move || {
            for _ in 0..n {
                let Ok((mut s, _)) = l.accept() else { break };
                let mut raw = vec![];
                let mut buf = [0u8; 4096];
                loop {
                    let got = s.read(&mut buf).unwrap_or(0);
                    if got == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buf[..got]);
                    if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&raw[..end]).to_ascii_lowercase();
                        let len: usize = head.lines().find_map(|h| h.strip_prefix("content-length:")).and_then(|n| n.trim().parse().ok()).unwrap_or(0);
                        if raw.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                let req = String::from_utf8_lossy(&raw).into_owned();
                let line = req.lines().next().unwrap_or("").to_string();
                let (code, body) =
                    routes.iter().find(|(p, _, _)| line.contains(p)).map(|(_, c, b)| (*c, b.replace("https://auth.x.ai", &me))).unwrap_or((404, "{}".into()));
                log.lock().unwrap().push(req);
                let _ = write!(s, "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        (url, seen)
    }

    fn requests(seen: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        seen.lock().unwrap().clone()
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
    fn monthly_ondemand_and_stamps() {
        let c = v(r#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_MONTHLY","start":"2026-09-01T00:00:00Z","end":"2026-10-01T00:00:00Z"},
            "creditUsagePercent":42.5,"onDemandCap":{"val":2000},"onDemandUsed":{"val":500},
            "productUsage":[{"product":"GrokBuild","usagePercent":40},{"product":"GrokVoice","usagePercent":2.5}],
            "subscriptionTier":"supergrok_heavy"}}"#);
        assert_eq!(
            rows(&windows(&c, CAPTURED)),
            vec![
                ("MONTH".into(), "42.5".into(), Some("2026-10-01T00:00:00Z".into())),
                ("ONDEMAND".into(), "25.0".into(), Some("2026-10-01T00:00:00Z".into())),
            ]
        );
        assert_eq!(plan_of_reply(&c), "SUPERGROK HEAVY");
        assert_eq!(plan_of_reply(&v(r#"{"subscriptionTier":"SUPERGROK","config":{}}"#)), "SUPERGROK");
        // products are not windows; clamping at both ends, on-demand over its cap too
        let c = v(r#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","end":"2026-09-14T17:30:18Z"},
            "creditUsagePercent":120,"onDemandCap":{"val":100},"onDemandUsed":{"val":150},
            "productUsage":[{"product":"GrokBuild","usagePercent":-3}, "junk", {"usagePercent":7}]}}"#);
        assert_eq!(
            rows(&windows(&c, CAPTURED)),
            vec![("WEEK".into(), "100".into(), Some("2026-09-14T17:30:18Z".into())), ("ONDEMAND".into(), "100".into(), Some("2026-09-14T17:30:18Z".into()))]
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
        assert_eq!(d.credits, json!({ "used": 1097, "currency": "PREPAID", "balance": 1097 })); // the prepaid balance, never a ring
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

    #[test]
    fn extras_ring_only_above_cap_zero_and_the_prepaid_caption() {
        // the real reply: cap 0 and a balance of 1097 -> no ONDEMAND ring, the balance as the caption
        let c = v(REPLY);
        assert_eq!(rows(&windows(&c, CAPTURED)).iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), vec!["WEEK"]);
        assert_eq!(credits(&c), json!({ "used": 1097, "currency": "PREPAID", "balance": 1097 }));
        // a cap: the ring at used/cap, and a balance of 0 is no caption
        let c = v(CAPPED);
        assert_eq!(
            rows(&windows(&c, CAPTURED)),
            vec![("WEEK".into(), "42.5".into(), Some(RESET.into())), ("ONDEMAND".into(), "25.0".into(), Some(RESET.into()))]
        );
        assert_eq!(credits(&c), Value::Null);
        // neither
        let c = v(NO_EXTRAS);
        assert_eq!(rows(&windows(&c, CAPTURED)), vec![("WEEK".into(), "12".into(), Some(RESET.into()))]);
        assert_eq!(credits(&c), Value::Null);
        // both, and the balance as the reply wrote it
        let c = v(
            r#"{"config":{"creditUsagePercent":3,"billingPeriodEnd":"2099-01-08T00:00:00Z","onDemandCap":{"val":100},"onDemandUsed":{"val":10},"prepaidBalance":{"val":12.5}}}"#,
        );
        assert_eq!(rows(&windows(&c, CAPTURED)).len(), 2);
        assert_eq!(credits(&c), json!({ "used": 12.5, "currency": "PREPAID", "balance": 12.5 }));
        // shapes that carry no balance
        for text in
            [r#"{"config":{"prepaidBalance":{"val":-5}}}"#, r#"{"config":{"prepaidBalance":{"val":"1097"}}}"#, r#"{"config":{"prepaidBalance":7}}"#, "{}"]
        {
            assert_eq!(credits(&v(text)), Value::Null, "{text}");
        }
    }

    #[test]
    fn refresh_when_the_cli_is_away() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Sandbox::new("grok-refresh");
        let (base, seen) = spy(3, vec![(WELL_KNOWN, 200, DISCOVERY), (TOKEN_PATH, 200, TOKEN_OK), (BILLING, 200, REPLY)]);
        // the Z stamp form, expired years ago, a refresh token present
        let file = login(&s, &auth_expiring("2020-01-01T00:00:00Z"), &base);
        let before = v(&std::fs::read_to_string(&file).unwrap());
        let t0 = crate::util::now();
        let d = run(270);
        assert_eq!((d.status.as_str(), d.hint.as_str(), d.source.as_str()), ("", "", "LIVE"));
        assert_eq!(rows(&d.windows), vec![("WEEK".into(), "1.0".into(), Some(RESET.into()))]);
        // the requests: discovery, the POST a public client makes, then billing with the new bearer
        let reqs = requests(&seen);
        assert_eq!(reqs.len(), 3, "{reqs:?}");
        assert!(reqs[0].starts_with(&format!("GET {WELL_KNOWN} ")), "{}", reqs[0]);
        assert!(reqs[1].starts_with(&format!("POST {TOKEN_PATH} ")), "{}", reqs[1]);
        assert!(reqs[1].to_ascii_lowercase().contains("content-type: application/x-www-form-urlencoded"), "{}", reqs[1]);
        let old_refresh = entry(AUTH)["refresh_token"].as_str().unwrap().to_string();
        assert!(reqs[1].ends_with(&format!("\r\n\r\ngrant_type=refresh_token&refresh_token={old_refresh}&client_id={CLIENT}")), "{}", reqs[1]);
        assert!(!reqs[1].contains("secret"));
        assert_eq!(bearer_of(&reqs[1]), None);
        assert_eq!(bearer_of(&reqs[2]), Some(new_key()));
        // the file: key, expiry and create time renewed, the rotated refresh token written, everything else as it was
        let after = v(&std::fs::read_to_string(&file).unwrap());
        let (b, a) = (&before[SCOPE], &after[SCOPE]);
        assert_eq!(a["key"].as_str().unwrap(), new_key());
        assert_eq!(a["refresh_token"], v(TOKEN_OK)["refresh_token"]);
        let expires = epoch_of(a["expires_at"].as_str().unwrap()).unwrap();
        let created = epoch_of(a["create_time"].as_str().unwrap()).unwrap();
        assert!((expires - (t0 + 21600)).abs() <= 5 && (created - t0).abs() <= 5, "{a}");
        assert!(a["expires_at"].as_str().unwrap().ends_with('Z') && a["create_time"].as_str().unwrap().ends_with('Z'), "{a}");
        for (k, val) in b.as_object().unwrap() {
            if !["key", "refresh_token", "expires_at", "create_time"].contains(&k.as_str()) {
                assert_eq!(&a[k], val, "{k}");
            }
        }
        assert_eq!(keys(a), keys(b), "the key order is kept");
        assert_eq!(keys(&after), keys(&before));
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read_dir(file.parent().unwrap()).unwrap().count(), 1, "no temp file left behind");
        // the memory: refreshed from the old expiry, the endpoint discovered
        let m = memory();
        assert_eq!((m.from, m.result.as_str(), m.retry, m.expires), (epoch_of("2020-01-01T00:00:00Z").unwrap(), "refreshed", false, expires));
        assert_eq!((m.issuer.as_str(), m.endpoint), (base.as_str(), format!("{base}{TOKEN_PATH}")));
        assert!(m.discovered >= t0);
        let out = capture(|| doctor(""));
        assert!(out.contains("  ok       token: refreshed by pulse-limits ") && out.contains("s ago, expires in "), "{out}");
        assert!(out.contains(", refresh token present\n"), "{out}");
        // a second reading: the cache is fresh, nothing is called, the file is left alone
        let (quiet, seen2) = spy(1, vec![]);
        std::env::set_var("GROK_CLI_CHAT_PROXY_BASE_URL", &quiet);
        let d = run(270);
        assert_eq!((d.status.as_str(), d.source.as_str()), ("", "CACHE"));
        assert!(requests(&seen2).is_empty());
        assert_eq!(v(&std::fs::read_to_string(&file).unwrap()), after);
        drop(s);
    }

    #[test]
    fn refresh_keeps_the_refresh_token_when_the_issuer_sends_none() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Sandbox::new("grok-keep");
        let (base, seen) = spy(3, vec![(WELL_KNOWN, 200, DISCOVERY), (TOKEN_PATH, 200, TOKEN_KEPT), (BILLING, 200, REPLY)]);
        // the +00:00 stamp form, an emptied legacy entry and unknown fields alongside
        let file = login(&s, EXPIRED, &base);
        let before = v(&std::fs::read_to_string(&file).unwrap());
        let d = run(270);
        assert_eq!((d.status.as_str(), d.source.as_str()), ("", "LIVE"));
        assert_eq!(requests(&seen).len(), 3);
        assert!(requests(&seen)[1].contains("refresh_token=DUMMYREFRESHTOKEN_before_rotation"));
        let after = v(&std::fs::read_to_string(&file).unwrap());
        assert_eq!(after["https://accounts.x.ai/sign-in"], before["https://accounts.x.ai/sign-in"]);
        assert_eq!(after["top_level_unknown"], before["top_level_unknown"]);
        assert_eq!(after[SCOPE]["future_field"], before[SCOPE]["future_field"]);
        assert_eq!(after[SCOPE]["refresh_token"], before[SCOPE]["refresh_token"], "the old refresh token stays");
        assert_eq!(after[SCOPE]["key"].as_str().unwrap(), new_key());
        assert_ne!(after[SCOPE]["expires_at"], before[SCOPE]["expires_at"]);
        assert_eq!(keys(&after), keys(&before));
        assert_eq!(keys(&after[SCOPE]), keys(&before[SCOPE]));
        assert_eq!(memory().from, OLD_EXPIRES);
        drop(s);
    }

    #[test]
    fn refresh_lifetime_from_the_reply_the_key_or_the_default() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Sandbox::new("grok-life");
        // no expires_in: the new key's exp claim (2100-01-01); an opaque key: the known 6 h
        let no_expires_in = Box::leak(
            v(TOKEN_KEPT)
                .as_object()
                .map(|o| o.iter().filter(|(k, _)| *k != "expires_in").map(|(k, v)| (k.clone(), v.clone())).collect::<serde_json::Map<_, _>>())
                .map(|m| Value::Object(m).to_string())
                .unwrap()
                .into_boxed_str(),
        );
        for (body, expected) in [
            (&*no_expires_in, "2100-01-01T00:00:00Z".to_string()),
            (r#"{"access_token":"opaque-refreshed-token","token_type":"Bearer"}"#, iso_utc(crate::util::now() + TOKEN_LIFETIME)),
            (r#"{"access_token":"opaque-refreshed-token","expires_in":0}"#, iso_utc(crate::util::now() + TOKEN_LIFETIME)),
        ] {
            let _ = std::fs::remove_file(s.scratch.cache().join("refresh-grok.json"));
            let _ = std::fs::remove_file(s.scratch.cache().join("usage-grok.json"));
            let (base, _) = spy(3, vec![(WELL_KNOWN, 200, DISCOVERY), (TOKEN_PATH, 200, body), (BILLING, 200, REPLY)]);
            let file = login(&s, EXPIRED, &base);
            let d = run(270);
            assert_eq!((d.status.as_str(), d.source.as_str()), ("", "LIVE"), "{body}");
            let got = v(&std::fs::read_to_string(&file).unwrap())[SCOPE]["expires_at"].as_str().unwrap().to_string();
            assert!((epoch_of(&got).unwrap() - epoch_of(&expected).unwrap()).abs() <= 5, "{body}: {got} vs {expected}");
        }
        drop(s);
    }

    #[test]
    fn a_refused_refresh_leaves_the_file_alone_and_backs_off() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Sandbox::new("grok-refused");
        let (base, seen) = spy(2, vec![(WELL_KNOWN, 200, DISCOVERY), (TOKEN_PATH, 400, TOKEN_DEAD)]);
        let file = login(&s, EXPIRED, &base);
        let before = std::fs::read(&file).unwrap();
        let d = run(270);
        assert_eq!((d.status.as_str(), d.hint.as_str(), d.source.as_str()), ("TOKEN EXPIRED", EXPIRED_HINT, ""));
        assert_eq!(std::fs::read(&file).unwrap(), before, "byte for byte");
        assert_eq!(requests(&seen).len(), 2);
        let until: i64 = std::fs::read_to_string(s.scratch.cache().join("backoff-grok")).unwrap().trim().parse().unwrap();
        assert!(until > crate::util::now() + BACKOFF_SECS - 5);
        let m = memory();
        assert_eq!((m.from, m.result.as_str(), m.retry, m.expires), (OLD_EXPIRES, "refresh refused (invalid_grant)", false, 0));
        let out = capture(|| doctor("TOKEN EXPIRED"));
        assert!(out.contains("  PROBLEM  token: expired ") && out.contains(" ago, refresh refused (invalid_grant)\n"), "{out}");
        // never again for this token, backoff or not: no request, the memory as it was
        std::fs::remove_file(s.scratch.cache().join("backoff-grok")).unwrap();
        let (quiet, seen2) = spy(1, vec![]);
        login(&s, EXPIRED, &quiet);
        let d = run(270);
        assert_eq!((d.status.as_str(), d.hint.as_str()), ("TOKEN EXPIRED", EXPIRED_HINT));
        assert!(requests(&seen2).is_empty());
        assert_eq!(memory(), m);
        assert!(!s.scratch.cache().join("backoff-grok").exists());
        // a 4xx with no JSON error is refused by its code
        std::fs::remove_file(s.scratch.cache().join("refresh-grok.json")).unwrap();
        let (base, _) = spy(2, vec![(WELL_KNOWN, 200, DISCOVERY), (TOKEN_PATH, 403, "forbidden")]);
        let file = login(&s, EXPIRED, &base);
        let before = std::fs::read(&file).unwrap();
        assert_eq!(run(270).status, "TOKEN EXPIRED");
        assert_eq!(std::fs::read(&file).unwrap(), before);
        assert_eq!(memory().result, "refresh refused (HTTP 403)");
        drop(s);
    }

    #[test]
    fn no_answer_and_server_errors_keep_the_file_and_the_cached_reading() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Sandbox::new("grok-5xx");
        let (base, seen) = spy(2, vec![(WELL_KNOWN, 200, DISCOVERY), (TOKEN_PATH, 503, "upstream error")]);
        let file = login(&s, EXPIRED, &base);
        let before = std::fs::read(&file).unwrap();
        let mut st = Store::new("grok");
        st.accept(REPLY.as_bytes()); // a reading from before the token died
        let host = upper(&host_of(&base));
        let d = run(270);
        assert_eq!((d.status.as_str(), d.hint, d.source.as_str()), ("HTTP 503", format!("UNEXPECTED ANSWER FROM {host}"), "CACHE"));
        assert_eq!(rows(&d.windows).len(), 1, "the cached reading stays");
        assert_eq!(std::fs::read(&file).unwrap(), before);
        assert!(!s.scratch.cache().join("backoff-grok").exists(), "no backoff beyond the throttle");
        let m = memory();
        assert_eq!((m.from, m.result.as_str(), m.retry), (OLD_EXPIRES, "refresh got HTTP 503", true));
        assert_eq!(requests(&seen).len(), 2);
        // not due again yet: no request, and the token is still what it is
        let d = run(270);
        assert_eq!((d.status.as_str(), d.hint.as_str(), d.source.as_str()), ("TOKEN EXPIRED", EXPIRED_HINT, "CACHE"));
        assert_eq!(requests(&seen).len(), 2);
        assert_eq!(memory(), m);
        let out = capture(|| doctor("TOKEN EXPIRED"));
        assert!(out.contains(" ago, refresh got HTTP 503 0s ago\n"), "{out}");
        // due again: the remembered endpoint (the server is gone by now) gets no answer
        std::fs::remove_file(s.scratch.cache().join("usage-grok.json")).unwrap();
        let d = run(270);
        assert_eq!((d.status.as_str(), d.hint), ("NETWORK", format!("COULD NOT REACH {host}")));
        assert_eq!((memory().result.as_str(), memory().retry), ("refresh got no answer", true));
        assert_eq!(std::fs::read(&file).unwrap(), before);
        // the other answers that are not a refusal: each leaves the file and asks for a retry
        for (routes, status, result) in [
            (vec![], "NETWORK", "discovery got no answer"),
            (vec![(WELL_KNOWN, 404, "{}")], "HTTP 404", "discovery got HTTP 404"),
            (vec![(WELL_KNOWN, 200, "{\"issuer\":\"x\"}")], "TOKEN EXPIRED", "discovery named no token endpoint"),
            (vec![(WELL_KNOWN, 200, DISCOVERY), (TOKEN_PATH, 200, "{\"token_type\":\"Bearer\"}")], "TOKEN EXPIRED", "refresh reply had no token"),
            (vec![(WELL_KNOWN, 200, DISCOVERY), (TOKEN_PATH, 200, "not json")], "TOKEN EXPIRED", "refresh reply had no token"),
            (vec![(WELL_KNOWN, 200, DISCOVERY), (TOKEN_PATH, 302, "")], "HTTP 302", "refresh got HTTP 302"),
        ] {
            std::fs::remove_file(s.scratch.cache().join("refresh-grok.json")).unwrap();
            let base = if routes.is_empty() { refused() } else { spy(2, routes).0 };
            let file = login(&s, EXPIRED, &base);
            let before = std::fs::read(&file).unwrap();
            let d = run(270);
            assert_eq!(d.status, status, "{result}");
            assert_eq!(std::fs::read(&file).unwrap(), before, "{result}");
            assert_eq!((memory().result.as_str(), memory().retry), (result, true));
        }
        drop(s);
    }

    #[test]
    fn the_cli_around_blocks_a_refresh_a_dead_pid_does_not() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Sandbox::new("grok-cli");
        let dir = s.home().join(".grok");
        std::fs::create_dir_all(&dir).unwrap();
        let me = std::process::id() as i64;
        assert!(pid_alive(me) && !pid_alive(0) && !pid_alive(-1));
        assert_eq!(cli_around(&dir), None, "nothing at all");
        // a grok process (the sandbox's pgrep says no by default)
        s.shim("pgrep", "true");
        assert_eq!(cli_around(&dir).as_deref(), Some("grok is running"));
        s.shim("pgrep", "false");
        // the lock: a live holder, a dead one, no pid
        std::fs::write(dir.join("auth.json.lock"), format!("{me}:{}", crate::util::now())).unwrap();
        assert_eq!(cli_around(&dir), Some(format!("auth.json.lock is held by pid {me}")));
        std::fs::write(dir.join("auth.json.lock"), "garbage").unwrap();
        assert_eq!(cli_around(&dir).as_deref(), Some("auth.json.lock names no pid"));
        std::fs::write(dir.join("auth.json.lock"), "").unwrap();
        assert_eq!(cli_around(&dir).as_deref(), Some("auth.json.lock names no pid"));
        s.shim("kill", "false"); // every pid is dead now
        std::fs::write(dir.join("auth.json.lock"), format!("{me}:{}", crate::util::now())).unwrap();
        assert_eq!(cli_around(&dir), None);
        std::fs::remove_file(dir.join("auth.json.lock")).unwrap();
        // the sessions: a live pid, dead ones, junk
        s.shim("kill", "true");
        std::fs::write(dir.join("active_sessions.json"), format!(r#"[{{"session_id":"s1","pid":{me},"cwd":"/","opened_at":"2026-09-08T14:20:42Z"}}]"#))
            .unwrap();
        assert_eq!(cli_around(&dir), Some(format!("a grok session is open (pid {me})")));
        std::fs::write(dir.join("active_sessions.json"), r#"[{"session_id":"s1"}, "junk", {"pid":"x"}]"#).unwrap();
        assert_eq!(cli_around(&dir), None);
        std::fs::write(dir.join("active_sessions.json"), "[]").unwrap();
        assert_eq!(cli_around(&dir), None);
        std::fs::write(dir.join("active_sessions.json"), "not json").unwrap();
        assert_eq!(cli_around(&dir), None);
        s.shim("kill", "false");
        std::fs::write(dir.join("active_sessions.json"), format!(r#"[{{"pid":{me}}}, {{"pid":{me}}}]"#)).unwrap();
        assert_eq!(cli_around(&dir), None);
        std::fs::remove_file(dir.join("active_sessions.json")).unwrap();
        // through run(): grok running -> no request at all, no memory, TOKEN EXPIRED; the doctor says why
        s.shim("pgrep", "true");
        let (base, seen) = spy(3, vec![(WELL_KNOWN, 200, DISCOVERY), (TOKEN_PATH, 200, TOKEN_OK), (BILLING, 200, REPLY)]);
        let file = login(&s, EXPIRED, &base);
        let before = std::fs::read(&file).unwrap();
        let d = run(270);
        assert_eq!((d.status.as_str(), d.hint.as_str()), ("TOKEN EXPIRED", EXPIRED_HINT));
        assert!(requests(&seen).is_empty());
        assert!(!s.scratch.cache().join("refresh-grok.json").exists());
        assert_eq!(std::fs::read(&file).unwrap(), before);
        let out = capture(|| doctor("TOKEN EXPIRED"));
        assert!(out.contains("  PROBLEM  token: expired ") && out.contains(" ago, refresh skipped: grok is running\n"), "{out}");
        // a live session pid: the same
        s.shim("pgrep", "false");
        s.shim("kill", "true");
        std::fs::write(dir.join("active_sessions.json"), r#"[{"pid": 4242}]"#).unwrap();
        assert_eq!(run(270).status, "TOKEN EXPIRED");
        assert!(requests(&seen).is_empty());
        assert!(capture(|| doctor("TOKEN EXPIRED")).contains(", refresh skipped: a grok session is open (pid 4242)\n"));
        // the same pid dead, and a dead lock holder: the refresh goes ahead
        s.shim("kill", "false");
        std::fs::write(dir.join("auth.json.lock"), "4243:1788877617").unwrap();
        let d = run(270);
        assert_eq!((d.status.as_str(), d.source.as_str()), ("", "LIVE"));
        assert_eq!(requests(&seen).len(), 3);
        assert_eq!(v(&std::fs::read_to_string(&file).unwrap())[SCOPE]["key"].as_str().unwrap(), new_key());
        drop(s);
    }

    #[test]
    fn one_refresh_per_token_even_when_saving_it_fails() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Sandbox::new("grok-once");
        let (base, seen) = spy(3, vec![(WELL_KNOWN, 200, DISCOVERY), (TOKEN_PATH, 200, TOKEN_OK), (BILLING, 200, REPLY)]);
        let file = login(&s, EXPIRED, &base);
        let before = std::fs::read(&file).unwrap();
        let dir = file.parent().unwrap().to_path_buf();
        // the file can be opened for writing, the folder refuses the temp file: the token is used, not saved
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let d = run(270);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!((d.status.as_str(), d.source.as_str()), ("", "LIVE"));
        assert_eq!(bearer_of(&requests(&seen)[2]), Some(new_key()));
        assert_eq!(std::fs::read(&file).unwrap(), before);
        let m = memory();
        assert!(m.result.starts_with("auth.json write failed: "), "{}", m.result);
        assert_eq!((m.from, m.retry, m.expires), (OLD_EXPIRES, false, 0));
        // the next due reading: the file still holds the old token, and it is not asked for again
        std::fs::remove_file(s.scratch.cache().join("usage-grok.json")).unwrap();
        let (quiet, seen2) = spy(1, vec![]);
        std::env::set_var("GROK_CLI_CHAT_PROXY_BASE_URL", &quiet);
        let d = run(270);
        assert_eq!((d.status.as_str(), d.hint.as_str()), ("TOKEN EXPIRED", EXPIRED_HINT));
        assert!(requests(&seen2).is_empty());
        assert_eq!(memory(), m);
        let out = capture(|| doctor("TOKEN EXPIRED"));
        assert!(out.contains(" ago, auth.json write failed: "), "{out}");
        // a file that cannot be opened for writing is found out before any token is spent
        std::fs::remove_file(s.scratch.cache().join("refresh-grok.json")).unwrap();
        let (base, seen3) = spy(1, vec![]);
        let file = login(&s, EXPIRED, &base);
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o400)).unwrap();
        assert_eq!(run(270).status, "TOKEN EXPIRED");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(requests(&seen3).is_empty());
        assert!(memory().result.starts_with("auth.json is not writable: "), "{}", memory().result);
        assert!(memory().retry);
        drop(s);
    }

    #[test]
    fn write_back_guards_and_the_home_manager_link() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("grok-write");
        let d = s.0.join("grok-home");
        std::fs::create_dir_all(&d).unwrap();
        let now = crate::util::now();
        let file = auth_file(&d, EXPIRED);
        let auth = read_auth(&file, now);
        assert_eq!(
            (auth.status.as_str(), auth.expires, auth.scope.as_str(), auth.client_id.as_str(), auth.issuer.as_str()),
            ("TOKEN EXPIRED", OLD_EXPIRES, SCOPE, CLIENT, "https://auth.x.ai")
        );
        // the happy path, and expires_at added when the file had none
        write_back(&auth, "new-key", now, now + 60, Some("rotated")).unwrap();
        let after = v(&std::fs::read_to_string(&file).unwrap());
        assert_eq!(
            (after[SCOPE]["key"].as_str(), after[SCOPE]["refresh_token"].as_str(), after[SCOPE]["expires_at"].as_str()),
            (Some("new-key"), Some("rotated"), Some(iso_utc(now + 60).as_str()))
        );
        // the file moved on: another token, another refresh token, the entry gone, not JSON, gone
        let changed = Err("auth.json changed during the refresh, not written".to_string());
        assert_eq!(write_back(&auth, "k", now, now, None), changed);
        auth_file(&d, &EXPIRED.replace("DUMMYREFRESHTOKEN_before_rotation", "DUMMYREFRESHTOKEN_someone_elses"));
        assert_eq!(write_back(&auth, "k", now, now, None), changed);
        auth_file(&d, "{}");
        assert_eq!(write_back(&auth, "k", now, now, None), changed);
        auth_file(&d, "nope");
        assert!(write_back(&auth, "k", now, now, None).unwrap_err().starts_with("auth.json write failed: "));
        std::fs::remove_file(&file).unwrap();
        assert!(write_back(&auth, "k", now, now, None).unwrap_err().starts_with("auth.json write failed: "));
        // no expires_at in the file: the key's exp claim identifies the token
        let bare = format!(r#"{{"{SCOPE}": {{"key": "{}", "refresh_token": "r", "oidc_issuer": "https://auth.x.ai"}}}}"#, fixture_key());
        auth_file(&d, &bare);
        let auth = read_auth(&file, EXPIRES + 10);
        assert_eq!((auth.status.as_str(), auth.expires), ("TOKEN EXPIRED", EXPIRES - 1));
        write_back(&auth, "new-key", now, now + 60, None).unwrap();
        let after = v(&std::fs::read_to_string(&file).unwrap());
        assert_eq!(keys(&after[SCOPE]), vec!["key", "refresh_token", "oidc_issuer", "expires_at", "create_time"]);
        // a Home Manager link is refused before and at the write, and left as it is
        let hm = d.join("linked.json");
        std::os::unix::fs::symlink("/nix/store/abc-home-manager-files/.grok/auth.json", &hm).unwrap();
        assert_eq!(writable(&hm).unwrap_err().to_string(), "managed by Home Manager");
        assert_eq!(write_auth(&hm, "{}").unwrap_err().to_string(), "managed by Home Manager");
        assert_eq!(std::fs::read_link(&hm).unwrap(), Path::new("/nix/store/abc-home-manager-files/.grok/auth.json"));
        assert!(writable(&d.join("missing.json")).is_err());
        // a plain write: mode 0600, the temp file gone
        write_auth(&file, "{\"a\":1}\n").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{\"a\":1}\n");
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 2);
    }

    #[test]
    fn discovery_is_remembered_for_a_day() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Sandbox::new("grok-discovery");
        let (base, seen) = spy(20, vec![(WELL_KNOWN, 200, DISCOVERY), (TOKEN_PATH, 200, TOKEN_OK), (BILLING, 200, REPLY)]);
        let discoveries = |seen: &Arc<Mutex<Vec<String>>>| requests(seen).iter().filter(|r| r.contains(WELL_KNOWN)).count();
        login(&s, EXPIRED, &base);
        assert_eq!(run(270).source, "LIVE");
        assert_eq!((requests(&seen).len(), discoveries(&seen)), (3, 1));
        // another expired token the same day: the endpoint is known
        std::fs::remove_file(s.scratch.cache().join("usage-grok.json")).unwrap();
        login(&s, &auth_expiring("2020-06-01T00:00:00Z"), &base);
        assert_eq!(run(270).source, "LIVE");
        assert_eq!((requests(&seen).len(), discoveries(&seen)), (5, 1));
        // a day later, or another issuer: asked again
        for (patch, expires_at) in [("discovered", "2020-07-01T00:00:00Z"), ("issuer", "2020-08-01T00:00:00Z")] {
            let mut m = memory();
            if patch == "discovered" {
                m.discovered -= DISCOVERY_TTL;
            } else {
                m.issuer = "https://other.example".into();
            }
            remember(&m);
            std::fs::remove_file(s.scratch.cache().join("usage-grok.json")).unwrap();
            login(&s, &auth_expiring(expires_at), &base);
            assert_eq!(run(270).source, "LIVE", "{patch}");
        }
        assert_eq!((requests(&seen).len(), discoveries(&seen)), (11, 3));
        assert_eq!(memory().issuer, base);
        drop(s);
    }

    #[test]
    fn no_refresh_when_the_token_is_good_or_the_login_cannot() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Sandbox::new("grok-norefresh");
        // 6 h left: billing only
        let (base, seen) = spy(3, vec![(WELL_KNOWN, 200, DISCOVERY), (TOKEN_PATH, 200, TOKEN_OK), (BILLING, 200, REPLY)]);
        let file = login(&s, &auth_expiring("2099-01-01T00:00:00Z"), &base);
        let before = std::fs::read(&file).unwrap();
        let d = run(270);
        assert_eq!((d.status.as_str(), d.source.as_str()), ("", "LIVE"));
        assert_eq!(requests(&seen).len(), 1);
        assert_eq!(bearer_of(&requests(&seen)[0]), Some(fixture_key()));
        assert_eq!(std::fs::read(&file).unwrap(), before);
        assert!(!s.scratch.cache().join("refresh-grok.json").exists());
        assert!(capture(|| doctor("")).contains("  ok       token: from auth.json, expires in "));
        // expired without a refresh token, a legacy login with one but no issuer, an OIDC scope with no client id
        let mut no_refresh = v(EXPIRED);
        no_refresh[SCOPE].as_object_mut().unwrap().remove("refresh_token");
        // (an issuer these carry is a closed port: a request would fail, never leave the machine)
        let legacy =
            r#"{"https://accounts.x.ai/sign-in": {"key": "legacy-session-key", "expires_at": "2020-01-01T06:00:00Z", "refresh_token": "r"}}"#.to_string();
        let no_client = format!(r#"{{"{}::": {{"key": "opaque", "expires_at": "2020-01-01T06:00:00Z", "refresh_token": "r"}}}}"#, refused());
        for (text, why) in [
            (issued_by(&no_refresh.to_string(), &refused()), "no refresh token"),
            (legacy, "no issuer in the login"),
            (no_client, "no client id in the login"),
        ] {
            let _ = std::fs::remove_file(s.scratch.cache().join("usage-grok.json"));
            let file = auth_file(&s.home().join(".grok"), &text);
            let before = std::fs::read(&file).unwrap();
            let d = run(270);
            assert_eq!((d.status.as_str(), d.hint.as_str()), ("TOKEN EXPIRED", EXPIRED_HINT), "{why}");
            assert_eq!(requests(&seen).len(), 1, "{why}");
            assert_eq!(std::fs::read(&file).unwrap(), before);
            assert!(capture(|| doctor("TOKEN EXPIRED")).contains(&format!(" ago, refresh skipped: {why}\n")), "{why}");
        }
        assert!(!s.scratch.cache().join("refresh-grok.json").exists());
        drop(s);
    }

    #[test]
    fn the_doctor_token_line() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Sandbox::new("grok-tokenline");
        let dir = s.home().join(".grok");
        std::fs::create_dir_all(&dir).unwrap();
        let now = crate::util::now();
        let fresh = auth_file(&dir, &auth_expiring(&iso_utc(now + 4 * 3600 + 12 * 60 + 30)));
        let auth = read_auth(&fresh, now);
        assert_eq!(token_line(&auth, &Memory::default(), now), Some((true, "token: from auth.json, expires in 4h12m".into())));
        // refreshed here 3 min ago: the memory names this very token
        let mine = Memory { result: "refreshed".into(), expires: auth.expires, at: now - 180, from: 1, ..Memory::default() };
        assert_eq!(token_line(&auth, &mine, now), Some((true, "token: refreshed by pulse-limits 3m ago, expires in 4h12m".into())));
        let other = Memory { expires: auth.expires - 1, ..mine.clone() };
        assert_eq!(token_line(&auth, &other, now).unwrap().1, "token: from auth.json, expires in 4h12m");
        // no expiry in the file
        let auth = read_auth(&auth_file(&dir, &format!(r#"{{"{SCOPE}": {{"key": "opaque-token"}}}}"#)), now);
        assert_eq!(token_line(&auth, &Memory::default(), now).unwrap().1, "token: from auth.json, no expiry in the file");
        // expired 2 h ago, tried and got no answer 2 min ago; the odd case of a refresh the file does not show
        let auth = read_auth(&auth_file(&dir, &auth_expiring(&iso_utc(now - 7200))), now);
        let tried_it = Memory { from: auth.expires, at: now - 120, result: "refresh got no answer".into(), retry: true, ..Memory::default() };
        assert_eq!(token_line(&auth, &tried_it, now), Some((false, "token: expired 2h ago, refresh got no answer 2m ago".into())));
        let odd = Memory { result: "refreshed".into(), retry: false, ..tried_it };
        assert_eq!(token_line(&auth, &odd, now).unwrap().1, "token: expired 2h ago, already refreshed from this token, yet auth.json still holds it");
        // inside the CLI's margin, nothing tried, nothing in the way
        let auth = read_auth(&auth_file(&dir, &auth_expiring(&iso_utc(now + 100))), now);
        assert_eq!(token_line(&auth, &Memory::default(), now).unwrap().1, "token: expires in 1m, inside the CLI's 5 min margin, refresh not attempted yet");
        // nothing to say about a login that is not one
        let auth = read_auth(&auth_file(&dir, r#"{"https://auth.x.ai::c": {"key": "xai-notARealKey000"}}"#), now);
        assert_eq!(token_line(&auth, &Memory::default(), now), None);
        assert_eq!(token_line(&read_auth(&dir.join("nope.json"), now), &Memory::default(), now), None);
        assert!(!capture(|| doctor("")).contains("token:"));
        // the pieces
        assert_eq!(age(4 * 3600 + 12 * 60 + 59), "4h12m");
        assert_eq!(age(7200), "2h");
        assert_eq!(age(3 * 60 + 5), "3m");
        assert_eq!(age(40), "40s");
        assert_eq!(age(-5), "0s");
        assert_eq!(host_of("https://auth.x.ai/oauth2/token"), "auth.x.ai");
        assert_eq!(host_of("http://127.0.0.1:8080"), "127.0.0.1:8080");
        assert_eq!(host_of("auth.x.ai"), "auth.x.ai");
        assert!(!Fail::Refused("x".into()).retry());
        assert!(Fail::NoAnswer("refresh").retry() && Fail::Http("discovery", 500).retry() && Fail::Other("y".into()).retry());
        assert_eq!(Fail::Other("y".into()).phrase(), "y");
        let m = Memory { issuer: "i".into(), endpoint: "e".into(), discovered: 1, from: 2, at: 3, result: "r".into(), retry: true, expires: 4 };
        remember(&m);
        assert_eq!(memory(), m);
        std::fs::write(s.scratch.cache().join("refresh-grok.json"), "junk").unwrap();
        assert_eq!(memory(), Memory::default());
        drop(s);
    }
}
