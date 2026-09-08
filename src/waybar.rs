//! One JSON line for a Waybar custom module (Linux): the session % as text, the tone as a
//! CSS class (ok | warn | crit | stale | dead), the percentage Waybar picks its ring glyph
//! from, and the right-click menu of macOS as the tooltip.

use serde_json::{json, Value};

use crate::payload::Built;
use crate::swiftbar::row;
use crate::util::{fmt_k, now, round_half_up, short, upper};

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
    for w in &a.windows {
        tip.push_str(&format!("\n{}", row(&w.label, round_half_up(w.pct_f()), w.resets.as_deref(), now)));
    }
    if let Some(c) = b.payload.get("credits").filter(|c| c.is_object()) {
        tip.push_str(&format!("\nEXTRA    {} {}", c["used"], c["currency"].as_str().unwrap_or("")));
    }
    // the other enabled providers, as the macOS menu lists them
    for d in b.docs.iter().filter(|d| b.enabled.contains(&d.provider) && d.provider != b.active) {
        tip.push_str(&format!("\n{}{}", upper(&d.provider), if d.plan.is_empty() { String::new() } else { format!("  ·  {}", d.plan) }));
        if !d.status.is_empty() {
            tip.push_str(&format!("\n? {}", d.status));
        }
        for w in &d.windows {
            tip.push_str(&format!("\n{}", row(&w.label, round_half_up(w.pct_f()), w.resets.as_deref(), now)));
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
            fetched: now() - 45,
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
        assert_eq!(v["tooltip"], "PULSE LIMITS  ·  MAX 20X\nSESSION  ███░░░░░░░░░░░░░░░░░  17%   RESETS IN ?\nWEEK     ████████░░░░░░░░░░░░  42%   RESETS IN ?\nEXTRA    1.5 EUR\n8.1K TOK/MIN · 4 SESSIONS\nUPDATED 45S AGO · CACHE");
        assert!(render(&b).starts_with("{\"text\":\"17%\",\"tooltip\":\"PULSE LIMITS"));
        let mut stale = d.clone();
        stale.status = "TOKEN EXPIRED".into();
        stale.hint = "OPEN CLAUDE CODE ONCE, IT REFRESHES THE TOKEN".into();
        let b = assemble(vec![stale], vec!["claude".into()], "claude", "crt", json!({ "tok_per_min": 0, "idle_s": 125, "sessions": 0 }));
        let v: Value = serde_json::from_str(&render(&b)).unwrap();
        assert_eq!((v["text"].as_str(), v["class"].as_str()), (Some("17%!"), Some("stale")));
        assert!(v["tooltip"].as_str().unwrap().ends_with("\nIDLE 2M\nUPDATED 45S AGO · CACHE\n? TOKEN EXPIRED\nOPEN CLAUDE CODE ONCE, IT REFRESHES THE TOKEN"));
        let b = assemble(vec![], vec![], "", "crt", Value::Null);
        let v: Value = serde_json::from_str(&render(&b)).unwrap();
        assert_eq!((v["text"].as_str(), v["class"].as_str(), v["percentage"].as_i64()), (Some("--"), Some("dead"), Some(0)));
        assert_eq!(v["tooltip"], "PULSE LIMITS\n? NO PROVIDER\nENABLE ONE: pulse-limits provider grok, claude or codex");
        std::env::remove_var("CLAUDE_PROJECTS_DIR");
    }
}
