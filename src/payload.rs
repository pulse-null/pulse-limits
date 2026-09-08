//! The document panel.html and the TUI read: the active provider on top, every provider in
//! providers[], the activity reading, and Claude's dead-reckoned session estimate. Packed as
//! base64 into the URL fragment of panel.html and written to panel.url for the popover.

use std::path::Path;

use serde_json::{json, Value};

use crate::activity;
use crate::estimate;
use crate::providers::{self, Doc};
use crate::util::{base64_encode, cache_dir, config_dir, lib_dir, process_running, read_trimmed, round_half_up, write_atomic, THEMES};

pub const BAR_INTERVAL: i64 = 270; // the bar runs every minute; each API sees one call per 5
pub const PANEL_INTERVAL: i64 = 45; // the popover and the TUI ask every 2 min while open

pub struct Built {
    pub payload: Value,
    pub docs: Vec<Doc>,
    pub enabled: Vec<String>,
    pub active: String,
    pub theme: String,
    pub have_data: bool,
    pub s_api: f64,
    pub s_pct: i64, // what the bar shows: the estimate when calibrated, else the reading
    pub b64: String,
}

impl Built {
    pub fn str(&self, key: &str) -> String {
        self.payload.get(key).and_then(Value::as_str).unwrap_or("").to_string()
    }

    pub fn active_doc(&self) -> Doc {
        self.docs.iter().find(|d| d.provider == self.active).cloned().unwrap_or_else(Doc::none)
    }

    pub fn panel_url(&self, lib: &Path) -> String {
        format!("file://{}#{}", lib.join("panel.html").display(), self.b64)
    }
}

/// The chosen theme; default crt.
pub fn theme() -> String {
    read_trimmed(&config_dir().join("theme")).filter(|t| THEMES.contains(&t.as_str())).unwrap_or_else(|| "crt".into())
}

pub fn set_theme(name: &str) -> Result<(), String> {
    if !THEMES.contains(&name) {
        return Err(format!("unknown theme: {name} (one of: {})", THEMES.join(" ")));
    }
    write_atomic(&config_dir().join("theme"), format!("{name}\n").as_bytes()).map_err(|e| e.to_string())
}

/// The bar shows one provider: the first enabled one whose CLI runs right now, else the first.
pub fn active_of(enabled: &[String]) -> String {
    enabled.iter().find(|p| process_running(p)).or(enabled.first()).cloned().unwrap_or_default()
}

/// Asks every enabled provider (plus `extra`, for a TUI pointed at a disabled one), measures
/// the activity, and assembles the document. `write_url` refreshes panel.url for the popover.
pub fn build(min_interval: i64, write_url: bool, extra: Option<&str>) -> Built {
    let enabled = providers::enabled();
    let mut names = enabled.clone();
    if let Some(e) = extra.filter(|e| !enabled.iter().any(|p| p == e)) {
        names.push(e.into());
    }
    let docs: Vec<Doc> = names.iter().map(|p| providers::run(p, min_interval)).collect();
    let active = match extra {
        Some(e) => e.to_string(),
        None => active_of(&enabled),
    };
    let b = assemble(docs, enabled, &active, &theme(), activity::measure().to_json());
    if write_url {
        let _ = write_atomic(&cache_dir().join("panel.url"), b.panel_url(&lib_dir()).as_bytes());
    }
    b
}

pub fn assemble(docs: Vec<Doc>, enabled: Vec<String>, active: &str, theme: &str, activity: Value) -> Built {
    let a = docs.iter().find(|d| d.provider == active).cloned().unwrap_or_else(Doc::none);
    let mut payload = json!({
        "provider": a.provider, "plan": a.plan, "source": a.source, "theme": theme, "fetched": a.fetched,
        "history": a.history.iter().map(|(t, p)| json!([t, p])).collect::<Vec<_>>(),
        "activity": activity,
        "status": a.status, "hint": a.hint,
        "windows": a.windows.iter().map(|w| w.to_json()).collect::<Vec<_>>(),
        "credits": a.credits,
        "providers": docs.iter().filter(|d| enabled.contains(&d.provider) || d.provider == active).map(Doc::summary).collect::<Vec<_>>(),
    });
    let have_data = !a.windows.is_empty();
    let session = a.session().map(|w| w.pct.clone());
    let s_api = session.as_ref().and_then(Value::as_f64).unwrap_or(0.0);
    let mut s_pct = round_half_up(s_api);
    // dead reckoning between readings, Claude only: the tokens are counted from Claude Code's transcripts
    if have_data && active == "claude" {
        let e = estimate::estimate(s_api, a.fetched);
        if e.calibrated {
            s_pct = round_half_up(e.pct_est);
        }
        payload["estimate"] = e.to_json(session);
    }
    let b64 = base64_encode(payload.to_string().as_bytes());
    Built { payload, docs, enabled, active: active.into(), theme: theme.into(), have_data, s_api, s_pct, b64 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::testing::{Scratch, ENV};
    use crate::providers::Window;

    fn doc(name: &str, windows: Vec<(&str, Value)>) -> Doc {
        Doc {
            provider: name.into(),
            plan: "PLAN".into(),
            source: "CACHE".into(),
            fetched: 1788876097,
            status: String::new(),
            hint: String::new(),
            windows: windows.into_iter().map(|(l, p)| Window { label: l.into(), pct: p, resets: Some("2026-09-08T18:30:00Z".into()) }).collect(),
            credits: Value::Null,
            history: vec![(1788870000, 3)],
        }
    }

    #[test]
    fn shape_and_order() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("payload");
        std::env::set_var("CLAUDE_PROJECTS_DIR", s.0.join("none"));
        let docs = vec![doc("claude", vec![("SESSION", json!(13)), ("WEEK", json!(17))]), doc("codex", vec![("SESSION", json!(17.0))])];
        let b = assemble(docs, vec!["claude".into(), "codex".into()], "claude", "crt", json!({"tok_per_min": 0, "idle_s": 5, "sessions": 0}));
        let keys: Vec<&str> = b.payload.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["provider", "plan", "source", "theme", "fetched", "history", "activity", "status", "hint", "windows", "credits", "providers", "estimate"]
        );
        assert_eq!(b.payload["providers"].as_array().unwrap().len(), 2);
        assert_eq!(b.payload["providers"][1]["name"], "codex");
        assert_eq!(b.payload["providers"][1]["windows"][0]["pct"].to_string(), "17.0");
        assert_eq!(b.payload["estimate"]["pct_api"].to_string(), "13");
        assert_eq!(b.payload["estimate"]["calibrated"], false);
        assert_eq!((b.have_data, b.s_pct), (true, 13));
        assert!(!b.b64.contains('\n'));
        assert_eq!(serde_json::from_slice::<Value>(&crate::util::base64_decode(&b.b64).unwrap()).unwrap(), b.payload);
        // codex active: no estimate key at all
        let b = assemble(vec![doc("codex", vec![("SESSION", json!(64.5))])], vec!["codex".into()], "codex", "synth", Value::Null);
        assert!(b.payload.get("estimate").is_none());
        assert_eq!((b.s_pct, b.str("provider").as_str()), (65, "codex"));
        // a provider with no SESSION window (a weekly pool only): its first window is the bar's number
        let b = assemble(vec![doc("grok", vec![("WEEK", json!(37.5))])], vec!["grok".into()], "grok", "crt", Value::Null);
        assert_eq!((b.have_data, b.s_pct, b.s_api), (true, 38, 37.5));
        assert!(b.payload.get("estimate").is_none());
        // nothing enabled
        let b = assemble(vec![], vec![], "", "crt", Value::Null);
        assert_eq!((b.str("status").as_str(), b.have_data, b.s_pct), ("NO PROVIDER SELECTED", false, 0));
        assert_eq!(b.payload["providers"].as_array().unwrap().len(), 0);
        std::env::remove_var("CLAUDE_PROJECTS_DIR");
    }

    #[test]
    fn theme_file() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let _s = Scratch::new("theme");
        assert_eq!(theme(), "crt");
        set_theme("synth").unwrap();
        assert_eq!(theme(), "synth");
        assert!(set_theme("nope").is_err());
        assert_eq!(std::fs::read_to_string(config_dir().join("theme")).unwrap(), "synth\n");
    }

    #[test]
    fn build_asks_the_enabled_providers_and_writes_the_panel_url() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = crate::providers::testing::Sandbox::new("payload-build");
        let lib = s.scratch.0.join("lib");
        // nothing enabled, nothing running: NO PROVIDER SELECTED, and panel.url is written
        let b = build(PANEL_INTERVAL, true, None);
        assert_eq!((b.active.as_str(), b.str("status").as_str(), b.have_data, b.enabled.len(), b.docs.len()), ("", "NO PROVIDER SELECTED", false, 0, 0));
        let url = std::fs::read_to_string(s.scratch.cache().join("panel.url")).unwrap();
        assert_eq!(url, format!("file://{}/panel.html#{}", lib.display(), b.b64));
        assert_eq!(b.panel_url(&lib), url);
        assert_eq!(b.active_doc().status, "NO PROVIDER SELECTED");
        // two enabled, none running (pgrep is shimmed): the first is the bar's; both say NO LOGIN without a call
        s.enable("codex grok");
        let b = build(PANEL_INTERVAL, false, None);
        assert_eq!((b.active.as_str(), b.str("provider").as_str(), b.str("status").as_str(), b.theme.as_str()), ("codex", "codex", "NO LOGIN", "crt"));
        assert_eq!(b.payload["providers"].as_array().unwrap().len(), 2);
        assert_eq!(b.payload["providers"][1]["name"], "grok");
        assert_eq!(b.payload["activity"]["sessions"], 0);
        assert_eq!(active_of(&b.enabled), "codex");
        assert_eq!(active_of(&[]), "");
        // a provider named that is not enabled: asked too, shown, and listed in providers[]
        let b = build(PANEL_INTERVAL, false, Some("claude"));
        assert_eq!((b.active.as_str(), b.docs.len(), b.active_doc().provider.as_str()), ("claude", 3, "claude"));
        assert_eq!(b.payload["providers"].as_array().unwrap().len(), 3);
        // a named one that is enabled is not asked twice
        let b = build(PANEL_INTERVAL, false, Some("grok"));
        assert_eq!((b.active.as_str(), b.docs.len()), ("grok", 2));
        drop(s);
    }

    #[test]
    fn a_calibrated_estimate_is_what_the_bar_shows() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = crate::providers::testing::Sandbox::new("payload-estimate");
        let projects = s.home().join("projects").join("p");
        std::fs::create_dir_all(&projects).unwrap();
        let fetched = crate::util::now() - 600;
        // 2750 output tokens since the reading, at k = 0.001 % per token: 13 + 2.75 = 15.75 -> 15.8 -> the bar shows 16
        crate::estimate::save(&crate::estimate::calib_file(), &crate::estimate::Calib { anchor_pct: 13.0, anchor_at: fetched, k: 0.001, samples: 2 });
        std::fs::write(
            projects.join("s.jsonl"),
            format!(
                "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"id\":\"m\",\"usage\":{{\"output_tokens\":2750}}}}}}\n",
                crate::util::iso_utc(fetched + 60)
            ),
        )
        .unwrap();
        let mut d = doc("claude", vec![("SESSION", json!(13)), ("WEEK", json!(17))]);
        d.fetched = fetched;
        let b = assemble(vec![d], vec!["claude".into()], "claude", "crt", Value::Null);
        assert_eq!((b.payload["estimate"]["calibrated"].as_bool(), b.payload["estimate"]["pct_est"].as_f64()), (Some(true), Some(15.8)));
        assert_eq!((b.s_api, b.s_pct), (13.0, 16));
        drop(s);
    }
}
