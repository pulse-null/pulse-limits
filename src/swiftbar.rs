//! The lines SwiftBar reads once a minute: the menu bar item (the session % and a ring, as one
//! image from bin/pulse-menubar) and the right-click menu: FORCE REFRESH (Option), THEME and
//! PROVIDERS submenus, then one block per provider with its plan, its error and its windows.
//! Every click action runs this binary directly (runInBash=false in the shim).

use std::path::Path;
use std::process::{Command, Stdio};

use crate::payload::Built;
use crate::providers::KNOWN;
use crate::util::{bar, countdown, epoch_of, is_executable, now, qualified, round_half_up, tone, upper, Tone, THEMES};

// "light,dark" pairs for the text menu
pub const C_HEAD: &str = "#1c5f8a,#8fd3ff";
pub const C_GREEN: &str = "#1E7F2A,#5FD75F";
pub const C_AMBER: &str = "#A85E00,#FFB000";
pub const C_RED: &str = "#B71C1C,#FF5C5C";
pub const C_DIM: &str = "#707070,#8C8C8C";
pub const MONO: &str = "font=Menlo size=12 trim=false";
pub const POPOVER_W: i64 = 520;
pub const POPOVER_H: i64 = 316; // our own popover: 300 of screen + bezel. SwiftBar's fallback adds its 32 px header

fn color(t: Tone) -> &'static str {
    match t {
        Tone::Red => C_RED,
        Tone::Amber => C_AMBER,
        Tone::Green => C_GREEN,
    }
}

fn line(out: &mut String, title: &str, attrs: &str) {
    out.push_str(&format!("{title} | {attrs}\n"));
}

/// "SESSION  ███░░░░░░░░░░░░░░░░░  17%   RESETS IN 2H 14M"
/// `width` is the label column: 8 for one provider, 14 when the labels carry the provider name.
pub fn row(label: &str, pct: i64, resets: Option<&str>, now: i64, width: usize) -> String {
    let when = resets.and_then(epoch_of).map(|e| countdown(e, now)).unwrap_or_else(|| "?".into());
    let name: String = label.chars().take(width).collect();
    format!("{name:<width$} {} {pct:>3}%   RESETS IN {when}", bar(pct, 20))
}

/// The menu bar image from bin/pulse-menubar: "<width> <height> <base64 png>".
fn menubar_image(bin: &Path, pct: i64, label: &str, text: &str, track: &str) -> Option<(String, String, String)> {
    let out = Command::new(bin).args([&pct.to_string(), label, text, text, track]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let mut f = s.split_whitespace();
    Some((f.next()?.into(), f.next()?.into(), f.next()?.into()))
}

pub fn render(b: &Built, lib: &Path) -> String {
    let now = now();
    let bin = lib.join("bin").join("pulse-limits").display().to_string();
    let mut out = String::new();
    let (status, hint, plan) = (b.str("status"), b.str("hint"), b.str("plan"));

    // menu bar line: "10%" then a ring that fills with it, like the battery item
    let (mut label, mut tcolor, ring) = if b.have_data { (format!("{}%", b.s_pct), color(tone(b.s_pct)), b.s_pct) } else { ("--".to_string(), C_RED, 0) };
    if b.have_data && !status.is_empty() {
        label.push('!');
        tcolor = C_RED;
    }
    let (mut title, mut img) = (format!("● {label}"), String::new());
    let menubar = lib.join("bin").join("pulse-menubar");
    if is_executable(&menubar) {
        // one PNG per menu bar appearance: text and arc in the tone colour, a neutral track
        let (light, dark) = tcolor.split_once(',').unwrap_or((tcolor, tcolor));
        if let (Some((w, h, l)), Some((_, _, d))) =
            (menubar_image(&menubar, ring, &label, light, "#c9ced2"), menubar_image(&menubar, ring, &label, dark, "#3a4044"))
        {
            title = String::new();
            img = format!("image={l},{d} width={w} height={h}");
        }
    }
    let action = if is_executable(&lib.join("bin").join("pulse-popover")) {
        format!("bash={bin} param1=open terminal=false")
    } else {
        format!("webview=true webvieww={POPOVER_W} webviewh={} href={}", POPOVER_H + 32, b.panel_url(lib))
    };
    line(&mut out, &title, &format!("{MONO} color={tcolor} {img} {action}"));
    out.push_str("---\n");

    // text menu (right-click)
    let n_enabled = b.enabled.len();
    let mut hdr = "PULSE LIMITS".to_string();
    if !b.active.is_empty() && n_enabled > 1 {
        hdr.push_str(&format!("  ·  {}", upper(&b.active)));
    }
    if !plan.is_empty() {
        hdr.push_str(&format!("  ·  {plan}"));
    }
    line(&mut out, &hdr, &format!("{MONO} color={C_HEAD}"));
    line(&mut out, "FORCE REFRESH", &format!("{MONO} color={C_GREEN} alternate=true bash={bin} param1=reset terminal=false refresh=true"));
    line(&mut out, &format!("THEME  ·  {}", upper(&b.theme)), &format!("{MONO} color={C_DIM}"));
    for th in THEMES {
        line(
            &mut out,
            &format!("--{}", upper(th)),
            &format!("{MONO} color={C_GREEN} checked={} bash={bin} param1=theme param2={th} terminal=false refresh=true", th == b.theme),
        );
    }
    line(&mut out, "PROVIDERS", &format!("{MONO} color={C_DIM}"));
    for p in KNOWN {
        line(
            &mut out,
            &format!("--{}", upper(p)),
            &format!("{MONO} color={C_GREEN} checked={} bash={bin} param1=provider param2={p} terminal=false refresh=true", b.enabled.iter().any(|e| e == p)),
        );
    }
    if n_enabled == 0 {
        line(&mut out, &status, &format!("{MONO} color={C_RED}"));
        if !hint.is_empty() {
            line(&mut out, &hint, &format!("{MONO} color={C_DIM}"));
        }
    }
    // one block per provider: its plan, its error if any, its windows (named after it when several are on)
    let multi = b.enabled.len() > 1;
    for d in b.docs.iter().filter(|d| b.enabled.contains(&d.provider)) {
        out.push_str("---\n");
        let mut h = upper(&d.provider);
        if !d.plan.is_empty() {
            h.push_str(&format!("  ·  {}", d.plan));
        }
        line(&mut out, &h, &format!("{MONO} color={C_HEAD}"));
        if !d.status.is_empty() {
            line(&mut out, &d.status, &format!("{MONO} color={C_RED}"));
            if !d.hint.is_empty() {
                line(&mut out, &d.hint, &format!("{MONO} color={C_DIM}"));
            }
        }
        for w in &d.windows {
            let pct = round_half_up(w.pct_f());
            let label = if multi { qualified(&d.provider, &w.label) } else { w.label.clone() };
            line(&mut out, &row(&label, pct, w.resets.as_deref(), now, if multi { 14 } else { 8 }), &format!("{MONO} color={}", color(tone(pct))));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::assemble;
    use crate::providers::testing::{Scratch, ENV};
    use crate::providers::{Doc, Window};
    use crate::util::testing::{calls, fake_bin, Vars};
    use serde_json::{json, Value};

    fn doc(name: &str, status: &str, hint: &str, wins: Vec<(&str, Value)>) -> Doc {
        Doc {
            provider: name.into(),
            plan: if name == "claude" { "MAX 20X".into() } else { String::new() },
            source: "CACHE".into(),
            fetched: 1788876097,
            status: status.into(),
            hint: hint.into(),
            windows: wins.into_iter().map(|(l, p)| Window { label: l.into(), pct: p, resets: None }).collect(),
            credits: Value::Null,
            history: vec![],
        }
    }

    #[test]
    fn rows() {
        let now = 1788874800; // 2026-09-08T13:40:00Z
        assert_eq!(row("SESSION", 13, Some("2026-09-08T13:47:00.075883+00:00"), now, 8), "SESSION  ███░░░░░░░░░░░░░░░░░  13%   RESETS IN 7M");
        assert_eq!(row("WEEK", 9, Some("2026-09-09T06:07:00Z"), now, 8), "WEEK     ██░░░░░░░░░░░░░░░░░░   9%   RESETS IN 16H 27M");
        assert_eq!(row("FABLE", 100, None, now, 8), "FABLE    ████████████████████ 100%   RESETS IN ?");
        assert_eq!(row("LONGLABELHERE", 0, Some("-"), now, 8), "LONGLABE ░░░░░░░░░░░░░░░░░░░░   0%   RESETS IN ?");
    }

    #[test]
    fn menu_lines() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("swiftbar");
        std::env::set_var("CLAUDE_PROJECTS_DIR", s.0.join("none"));
        let lib = s.0.join("lib");
        std::fs::create_dir_all(lib.join("bin")).unwrap();
        std::fs::write(lib.join("panel.html"), "x").unwrap();
        let docs = vec![
            doc("claude", "", "", vec![("SESSION", json!(13)), ("WEEK", json!(9.4))]),
            doc("codex", "NO LOGIN", "NO CODEX LOGIN ON THIS MAC. RUN: codex login", vec![]),
        ];
        let b = assemble(docs, vec!["claude".into(), "codex".into()], "claude", "crt", Value::Null);
        let text = render(&b, &lib);
        let bin = lib.join("bin/pulse-limits").display().to_string();
        let expected = format!(
            "● 13% | font=Menlo size=12 trim=false color=#1E7F2A,#5FD75F  webview=true webvieww=520 webviewh=348 href=file://{panel}#{b64}
---
PULSE LIMITS  ·  CLAUDE  ·  MAX 20X | font=Menlo size=12 trim=false color=#1c5f8a,#8fd3ff
FORCE REFRESH | font=Menlo size=12 trim=false color=#1E7F2A,#5FD75F alternate=true bash={bin} param1=reset terminal=false refresh=true
THEME  ·  CRT | font=Menlo size=12 trim=false color=#707070,#8C8C8C
--CRT | font=Menlo size=12 trim=false color=#1E7F2A,#5FD75F checked=true bash={bin} param1=theme param2=crt terminal=false refresh=true
--MODERN | font=Menlo size=12 trim=false color=#1E7F2A,#5FD75F checked=false bash={bin} param1=theme param2=modern terminal=false refresh=true
--CYBER | font=Menlo size=12 trim=false color=#1E7F2A,#5FD75F checked=false bash={bin} param1=theme param2=cyber terminal=false refresh=true
--SYNTH | font=Menlo size=12 trim=false color=#1E7F2A,#5FD75F checked=false bash={bin} param1=theme param2=synth terminal=false refresh=true
--ANALOG | font=Menlo size=12 trim=false color=#1E7F2A,#5FD75F checked=false bash={bin} param1=theme param2=analog terminal=false refresh=true
PROVIDERS | font=Menlo size=12 trim=false color=#707070,#8C8C8C
--GROK | font=Menlo size=12 trim=false color=#1E7F2A,#5FD75F checked=false bash={bin} param1=provider param2=grok terminal=false refresh=true
--CLAUDE | font=Menlo size=12 trim=false color=#1E7F2A,#5FD75F checked=true bash={bin} param1=provider param2=claude terminal=false refresh=true
--CODEX | font=Menlo size=12 trim=false color=#1E7F2A,#5FD75F checked=true bash={bin} param1=provider param2=codex terminal=false refresh=true
---
CLAUDE  ·  MAX 20X | font=Menlo size=12 trim=false color=#1c5f8a,#8fd3ff
CLAUDE SESSION ███░░░░░░░░░░░░░░░░░  13%   RESETS IN ? | font=Menlo size=12 trim=false color=#1E7F2A,#5FD75F
CLAUDE 7D      ██░░░░░░░░░░░░░░░░░░   9%   RESETS IN ? | font=Menlo size=12 trim=false color=#1E7F2A,#5FD75F
---
CODEX | font=Menlo size=12 trim=false color=#1c5f8a,#8fd3ff
NO LOGIN | font=Menlo size=12 trim=false color=#B71C1C,#FF5C5C
NO CODEX LOGIN ON THIS MAC. RUN: codex login | font=Menlo size=12 trim=false color=#707070,#8C8C8C
",
            panel = lib.join("panel.html").display(),
            b64 = b.b64
        );
        assert_eq!(text, expected);
        // an error on the active provider: the label gets a "!" and turns red; no provider: "--"
        let b = assemble(vec![doc("claude", "TOKEN EXPIRED", "x", vec![("SESSION", json!(90))])], vec!["claude".into()], "claude", "synth", Value::Null);
        let text = render(&b, &lib);
        assert!(text.starts_with("● 90%! | font=Menlo size=12 trim=false color=#B71C1C,#FF5C5C  webview"));
        assert!(text.contains("\nPULSE LIMITS  ·  MAX 20X | "));
        assert!(text.contains("\nTOKEN EXPIRED | "));
        let b = assemble(vec![], vec![], "", "crt", Value::Null);
        let text = render(&b, &lib);
        assert!(text.starts_with("● -- | font=Menlo size=12 trim=false color=#B71C1C,#FF5C5C  webview"));
        assert!(text.contains("\nNO PROVIDER SELECTED | font=Menlo size=12 trim=false color=#B71C1C,#FF5C5C\n"));
        std::env::remove_var("CLAUDE_PROJECTS_DIR");
    }

    #[test]
    fn menubar_image_and_popover() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("swiftbar-bin");
        let mut vars = Vars::default();
        vars.set("CLAUDE_PROJECTS_DIR", s.0.join("none"));
        let lib = s.0.join("lib");
        std::fs::create_dir_all(lib.join("bin")).unwrap();
        std::fs::write(lib.join("panel.html"), "x").unwrap();
        let bin = lib.join("bin").join("pulse-limits").display().to_string();
        let b = assemble(vec![doc("claude", "", "", vec![("SESSION", json!(70))])], vec!["claude".into()], "claude", "crt", Value::Null);
        // the helpers built: one PNG per appearance in the amber tone, and the click runs `open`
        let menubar = fake_bin(&lib.join("bin"), "pulse-menubar", "printf '22 18 QUJD\\n'");
        fake_bin(&lib.join("bin"), "pulse-popover", "");
        let text = render(&b, &lib);
        assert!(
            text.starts_with(&format!(
                " | font=Menlo size=12 trim=false color=#A85E00,#FFB000 image=QUJD,QUJD width=22 height=18 bash={bin} param1=open terminal=false\n---\n"
            )),
            "{text}"
        );
        assert_eq!(calls(&menubar), vec!["70 70% #A85E00 #A85E00 #c9ced2", "70 70% #FFB000 #FFB000 #3a4044"]);
        assert!(text.contains("\nSESSION  ██████████████░░░░░░  70%   RESETS IN ? | font=Menlo size=12 trim=false color=#A85E00,#FFB000\n"));
        // a helper that fails, or prints too little: the text title stays
        fake_bin(&lib.join("bin"), "pulse-menubar", "exit 1");
        assert!(render(&b, &lib).starts_with("● 70% | font=Menlo size=12 trim=false color=#A85E00,#FFB000  bash="));
        fake_bin(&lib.join("bin"), "pulse-menubar", "printf '22 18\\n'");
        assert!(render(&b, &lib).starts_with("● 70% | "));
        // nothing enabled but a document with an error: its status and hint under the menu; no name in the header
        let b = assemble(vec![doc("codex", "NO LOGIN", "RUN: codex login", vec![])], vec![], "codex", "crt", Value::Null);
        let text = render(&b, &lib);
        assert!(text.contains("\nPULSE LIMITS | "));
        assert!(text.contains(
            "\nNO LOGIN | font=Menlo size=12 trim=false color=#B71C1C,#FF5C5C\nRUN: codex login | font=Menlo size=12 trim=false color=#707070,#8C8C8C\n"
        ));
        // an error without a hint in a provider block, in the red tone
        let b = assemble(vec![doc("grok", "HTTP 500", "", vec![("WEEK", json!(90))])], vec!["grok".into()], "grok", "crt", Value::Null);
        let text = render(&b, &lib);
        assert!(text.ends_with(
            "---\nGROK | font=Menlo size=12 trim=false color=#1c5f8a,#8fd3ff\nHTTP 500 | font=Menlo size=12 trim=false color=#B71C1C,#FF5C5C\nWEEK     ██████████████████░░  90%   RESETS IN ? | font=Menlo size=12 trim=false color=#B71C1C,#FF5C5C\n"
        ), "{text}");
    }
}
