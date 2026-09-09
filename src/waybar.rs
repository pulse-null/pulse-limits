//! One JSON line for a Waybar custom module (Linux): the session % as text, the tone as a
//! CSS class (ok | warn | crit | stale | dead), the percentage Waybar picks its ring glyph
//! from, and the right-click menu of macOS as the tooltip.

use serde_json::{json, Value};

use crate::payload::Built;
use crate::providers::{Doc, Window};
use crate::swiftbar::row;
use crate::util::{fmt_k, now, qualified, round_half_up, short, upper};

pub fn class(have_data: bool, s_pct: i64, status: &str) -> &'static str {
    if !have_data {
        "dead"
    } else if !status.is_empty() {
        "stale"
    } else if s_pct >= 85 {
        "crit"
    } else if s_pct >= 60 {
        "warn"
    } else {
        "ok"
    }
}

/// Waybar reads the tooltip as Pango markup.
pub fn pango(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn render(b: &Built) -> String {
    let now = now();
    let (status, hint, plan) = (b.str("status"), b.str("hint"), b.str("plan"));
    let label = if b.have_data { format!("{}%{}", b.s_pct, if status.is_empty() { "" } else { "!" }) } else { "--".into() };
    let mut tip = "PULSE LIMITS".to_string();
    if !plan.is_empty() {
        tip.push_str(&format!("  ·  {plan}"));
    }
    let a = b.active_doc();
    let (multi, width) = (b.enabled.len() > 1, if b.enabled.len() > 1 { 14 } else { 8 });
    let name = |d: &Doc, w: &Window| if multi { qualified(&d.provider, &w.label) } else { w.label.clone() };
    for w in &a.windows {
        tip.push_str(&format!("\n{}", row(&name(&a, w), round_half_up(w.pct_f()), w.resets.as_deref(), now, width)));
    }
    if let Some(c) = b.payload.get("credits").filter(|c| c.is_object()) {
        let cur = c["currency"].as_str().unwrap_or("");
        tip.push_str(&format!("\n{} {:.2}{}{cur}", c["label"].as_str().unwrap_or("CREDITS"), c["used"].as_f64().unwrap_or(0.0), if cur.is_empty() { "" } else { " " }));
    }
    // the other enabled providers, as the macOS menu lists them
    for d in b.docs.iter().filter(|d| b.enabled.contains(&d.provider) && d.provider != b.active) {
        tip.push_str(&format!("\n{}{}", upper(&d.provider), if d.plan.is_empty() { String::new() } else { format!("  ·  {}", d.plan) }));
        if !d.status.is_empty() {
            tip.push_str(&format!("\n? {}", d.status));
        }
        for w in &d.windows {
            tip.push_str(&format!("\n{}", row(&name(d, w), round_half_up(w.pct_f()), w.resets.as_deref(), now, width)));
        }
    }
    let act = &b.payload["activity"];
    if let Some(tok) = act.get("tok_per_min").and_then(Value::as_i64) {
        let sessions = act.get("sessions").and_then(Value::as_i64).unwrap_or(0);
        let idle = act.get("idle_s").and_then(Value::as_i64).unwrap_or(0);
        if tok > 0 {
            tip.push_str(&format!("\n{} TOK/MIN", fmt_k(tok)));
            if sessions > 1 {
                tip.push_str(&format!(" · {sessions} SESSIONS"));
            }
        } else {
            tip.push_str(&format!("\nIDLE {}", short(idle)));
        }
    }
    if b.have_data {
        if b.s_pct != round_half_up(b.s_api) {
            tip.push_str(&format!("\nSESSION NOW {}% · EST", b.s_pct));
        }
        tip.push_str(&format!("\nUPDATED {} AGO · {}", short(now - a.fetched), b.str("source")));
    }
    if !status.is_empty() {
        tip.push_str(&format!("\n? {status}"));
    }
    if !hint.is_empty() {
        tip.push_str(&format!("\n{hint}"));
    }
    json!({ "text": label, "tooltip": pango(&tip), "class": class(b.have_data, b.s_pct, &status), "percentage": b.s_pct }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::assemble;
    use crate::providers::testing::{Scratch, ENV};
    use crate::providers::{Doc, Window};
    use crate::util::testing::Vars;

    #[test]
    fn classes_and_tone() {
        assert_eq!(class(false, 0, ""), "dead");
        assert_eq!(class(false, 0, "NO LOGIN"), "dead");
        assert_eq!(class(true, 12, ""), "ok");
        assert_eq!(class(true, 60, ""), "warn");
        assert_eq!(class(true, 85, ""), "crit");
        assert_eq!(class(true, 99, "TOKEN EXPIRED"), "stale");
        assert_eq!(pango("a & b <c>"), "a &amp; b &lt;c&gt;");
    }

    #[test]
    fn json_line() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("waybar");
        std::env::set_var("CLAUDE_PROJECTS_DIR", s.0.join("none"));
        let d = Doc {
            provider: "claude".into(),
            plan: "MAX 20X".into(),
            source: "CACHE".into(),
            fetched: now() - 90,
            status: String::new(),
            hint: String::new(),
            windows: vec![Window { label: "SESSION".into(), pct: json!(17), resets: None }, Window { label: "WEEK".into(), pct: json!(42), resets: None }],
            credits: json!({ "used": 1.5, "currency": "EUR" }),
            history: vec![],
        };
        let b = assemble(vec![d.clone()], vec!["claude".into()], "claude", "crt", json!({ "tok_per_min": 8098, "idle_s": 0, "sessions": 4 }));
        let v: Value = serde_json::from_str(&render(&b)).unwrap();
        assert_eq!(v["text"], "17%");
        assert_eq!(v["class"], "ok");
        assert_eq!(v["percentage"], 17);
        assert_eq!(v["tooltip"], "PULSE LIMITS  ·  MAX 20X\nSESSION  ███░░░░░░░░░░░░░░░░░  17%   RESETS IN ?\nWEEK     ████████░░░░░░░░░░░░  42%   RESETS IN ?\nCREDITS 1.50 EUR\n8.1K TOK/MIN · 4 SESSIONS\nUPDATED 1M AGO · CACHE");
        assert!(render(&b).starts_with("{\"text\":\"17%\",\"tooltip\":\"PULSE LIMITS"));
        let mut stale = d.clone();
        stale.status = "TOKEN EXPIRED".into();
        stale.hint = "OPEN CLAUDE CODE ONCE, IT REFRESHES THE TOKEN".into();
        let b = assemble(vec![stale], vec!["claude".into()], "claude", "crt", json!({ "tok_per_min": 0, "idle_s": 125, "sessions": 0 }));
        let v: Value = serde_json::from_str(&render(&b)).unwrap();
        assert_eq!((v["text"].as_str(), v["class"].as_str()), (Some("17%!"), Some("stale")));
        assert!(v["tooltip"]
            .as_str()
            .unwrap()
            .ends_with("\nIDLE 2M\nUPDATED 1M AGO · CACHE\n? TOKEN EXPIRED\nOPEN CLAUDE CODE ONCE, IT REFRESHES THE TOKEN"));
        let b = assemble(vec![], vec![], "", "crt", Value::Null);
        let v: Value = serde_json::from_str(&render(&b)).unwrap();
        assert_eq!((v["text"].as_str(), v["class"].as_str(), v["percentage"].as_i64()), (Some("--"), Some("dead"), Some(0)));
        assert_eq!(v["tooltip"], "PULSE LIMITS\n? NO PROVIDER SELECTED");
        std::env::remove_var("CLAUDE_PROJECTS_DIR");
    }

    #[test]
    fn other_providers_and_estimate() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("waybar-multi");
        let mut vars = Vars::default();
        let projects = s.0.join("projects");
        vars.set("CLAUDE_PROJECTS_DIR", &projects);
        let fetched = now() - 90;
        let mk = |name: &str, plan: &str, status: &str, wins: Vec<(&str, i64)>| Doc {
            provider: name.into(),
            plan: plan.into(),
            source: "CACHE".into(),
            fetched,
            status: status.into(),
            hint: String::new(),
            windows: wins.into_iter().map(|(l, p)| Window { label: l.into(), pct: json!(p), resets: None }).collect(),
            credits: Value::Null,
            history: vec![],
        };
        // the other enabled providers follow, as the macOS menu lists them; a disabled one does not
        let docs = vec![
            mk("claude", "MAX 20X", "", vec![("SESSION", 17)]),
            mk("codex", "PLUS", "TOKEN EXPIRED", vec![("WEEK", 42)]),
            mk("grok", "", "", vec![("WEEK", 90)]),
        ];
        let b = assemble(docs, vec!["claude".into(), "codex".into()], "claude", "crt", json!({ "tok_per_min": 0, "idle_s": 0, "sessions": 0 }));
        let v: Value = serde_json::from_str(&render(&b)).unwrap();
        let tip = v["tooltip"].as_str().unwrap();
        assert!(tip.starts_with("PULSE LIMITS  ·  MAX 20X\nCLAUDE SESSION ███░░░░░░░░░░░░░░░░░  17%   RESETS IN ?\nCODEX  ·  PLUS\n? TOKEN EXPIRED\nCODEX 7D       ████████░░░░░░░░░░░░  42%   RESETS IN ?\nIDLE 0S\nUPDATED 1M"), "{tip}");
        assert!(!tip.contains("GROK"));
        let b = assemble(
            vec![mk("claude", "", "", vec![("SESSION", 17)]), mk("grok", "", "", vec![("WEEK", 90)])],
            vec!["claude".into(), "grok".into()],
            "claude",
            "crt",
            Value::Null,
        );
        let v: Value = serde_json::from_str(&render(&b)).unwrap();
        assert!(v["tooltip"].as_str().unwrap().starts_with(
            "PULSE LIMITS\nCLAUDE SESSION ███░░░░░░░░░░░░░░░░░  17%   RESETS IN ?\nGROK\nGROK 7D        ██████████████████░░  90%   RESETS IN ?\nUPDATED 1M"
        ));
        // a calibrated estimate moves the number the bar shows: the tooltip says so
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::write(
            projects.join("s.jsonl"),
            format!(
                "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"id\":\"m\",\"usage\":{{\"output_tokens\":2750}}}}}}\n",
                crate::util::iso_utc(now() - 10)
            ),
        )
        .unwrap();
        crate::estimate::save(&crate::estimate::calib_file(), &crate::estimate::Calib { anchor_pct: 17.0, anchor_at: fetched, k: 0.001, samples: 3 });
        let b = assemble(vec![mk("claude", "MAX 20X", "", vec![("SESSION", 17)])], vec!["claude".into()], "claude", "crt", Value::Null);
        assert_eq!((b.s_api, b.s_pct), (17.0, 20)); // 17 + 0.001 * 2750 = 19.75
        let v: Value = serde_json::from_str(&render(&b)).unwrap();
        assert_eq!(v["text"], "20%");
        assert_eq!(v["percentage"], 20);
        assert!(v["tooltip"].as_str().unwrap().contains("\nSESSION NOW 20% · EST\nUPDATED 1M"));
    }
}
