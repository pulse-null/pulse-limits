//! PulseLimits in the terminal: the monitor of panel.html as a ratatui screen.
//!
//!   pulse-limits tui [claude|codex] [--theme NAME]      (`pulse-limits claude` and `codex` are the same)
//!
//! Every 5 s it reads the payload the bar last wrote for the panel
//! (`~/.cache/pulse-limits/panel.url`, a `file://…#base64-JSON` line the menu bar rewrites
//! every minute, estimate included): a file read, no API call. Every 120 s, as the popover does,
//! it builds the payload itself, in process, which is what feeds a box with no bar; the
//! providers keep their own API throttle. Activity is measured every 2 s, in process. It draws
//! what the panel draws: the ECG whose rate follows the tokens per minute, the session
//! percentage in big digits, the other windows as bars, twelve hours of history as a
//! sparkline. Meant for a tmux pane or a tiling-WM tile; degrades down to about 40x12.
//!
//! With a provider named, it shows that provider whether or not the bar does, and leaves
//! panel.url alone.
//!
//! Keys: q quit, t next theme, r re-read the payload now, ? help.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::backend::Backend;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Painter, Shape};
use ratatui::widgets::{Block, Clear, LineGauge, Paragraph, Sparkline, SparklineBar};
use ratatui::{Frame, Terminal};
use serde_json::{Map, Value};

use crate::activity;
use crate::payload::{self, PANEL_INTERVAL};
use crate::providers;
use crate::util::{base64_decode, cache_dir, config_dir, epoch_of_f, fmt_k, hhmm, local_offset, mtime_f, now_f, short, span, THEMES};

const PANEL_EVERY: Duration = Duration::from_secs(5); // panel.url: a file read, decoded only when its mtime moved
const PAYLOAD_EVERY: Duration = Duration::from_secs(120); // the payload is built in process, as the popover runs it; the providers throttle the API
const ACTIVITY_EVERY: Duration = Duration::from_secs(2); // as the popover does
const FRAME: Duration = Duration::from_millis(50); // ~20 fps; ratatui only sends what changed
const SWEEP: f64 = 3.7; // s for the trace to cross the screen, like the panel (336 px at 90 px/s)
const HISTORY_HOURS: f64 = 12.0;
const MIN_ROWS: u16 = 10;
const MIN_COLS: u16 = 30;

// 3x5 block digits for the session percentage
const FONT: [(char, [&str; 5]); 12] = [
    ('0', ["###", "# #", "# #", "# #", "###"]),
    ('1', [" # ", "## ", " # ", " # ", "###"]),
    ('2', ["###", "  #", "###", "#  ", "###"]),
    ('3', ["###", "  #", "###", "  #", "###"]),
    ('4', ["# #", "# #", "###", "  #", "  #"]),
    ('5', ["###", "#  ", "###", "  #", "###"]),
    ('6', ["###", "#  ", "###", "# #", "###"]),
    ('7', ["###", "  #", "  #", "  #", "  #"]),
    ('8', ["###", "# #", "###", "# #", "###"]),
    ('9', ["###", "# #", "###", "  #", "###"]),
    ('%', ["# #", "  #", " # ", "#  ", "# #"]),
    ('-', ["   ", "   ", "###", "   ", "   "]),
];

fn glyph(c: char) -> &'static [&'static str; 5] {
    FONT.iter().find(|(k, _)| *k == c).map(|(_, g)| g).unwrap_or(&FONT[11].1)
}

fn rnd(x: f64) -> i64 {
    (x + 0.5).floor() as i64
}

fn now() -> f64 {
    now_f()
}

fn age_of(p: &Path) -> Option<Duration> {
    mtime_f(p).map(|m| Duration::from_secs_f64((now() - m).max(0.0)))
}

fn saved_theme() -> usize {
    let t = std::fs::read_to_string(config_dir().join("theme")).unwrap_or_default();
    THEMES.iter().position(|n| *n == t.trim()).unwrap_or(0)
}

// ---- data: the payload and the activity, off the drawing thread ----------------------------
#[derive(Default)]
struct Shared {
    payload: Option<Value>,
    activity: Option<Value>,
    version: u64, // bumped on every change
}

struct Feed {
    shared: Arc<Mutex<Shared>>,
    poke: Arc<AtomicBool>, // the r key: re-read panel.url now, and rebuild the payload only if that is stale
    panel_url: PathBuf,
    provider: Option<String>, // named on the command line: no panel.url, our own builds only
}

impl Feed {
    fn start(provider: Option<String>) -> Feed {
        let panel_url = cache_dir().join("panel.url");
        let shared = Arc::new(Mutex::new(Shared::default()));
        let poke = Arc::new(AtomicBool::new(false));
        let mut poller = Poller::new(shared.clone(), poke.clone(), panel_url.clone(), provider.clone());
        thread::spawn(move || loop {
            poller.tick();
            thread::sleep(Duration::from_millis(250));
        });
        Feed { shared, poke, panel_url, provider }
    }
}

/// The feed thread's state: what it does every 250 ms is `tick`.
struct Poller {
    shared: Arc<Mutex<Shared>>,
    poke: Arc<AtomicBool>,
    url: PathBuf,
    prov: Option<String>,
    seen_mtime: Option<f64>,
    next_panel: Instant,
    next_payload: Instant,
    next_activity: Instant,
}

impl Poller {
    fn new(shared: Arc<Mutex<Shared>>, poke: Arc<AtomicBool>, url: PathBuf, prov: Option<String>) -> Poller {
        let mut p = Poller {
            shared,
            poke,
            url,
            prov,
            seen_mtime: None,
            next_panel: Instant::now(),
            next_payload: Instant::now(),
            next_activity: Instant::now(),
        };
        // a fresh panel.url means a bar is feeding it: no need to build the payload ourselves yet
        if !p.stale() {
            p.next_payload = Instant::now() + PAYLOAD_EVERY;
        }
        p
    }

    fn stale(&self) -> bool {
        self.prov.is_some() || age_of(&self.url).is_none_or(|a| a > PAYLOAD_EVERY)
    }

    fn publish(&self, v: Value) {
        let mut s = self.shared.lock().unwrap();
        s.payload = Some(v);
        s.version += 1;
    }

    fn tick(&mut self) {
        let poked = self.poke.swap(false, Ordering::Relaxed);
        if self.prov.is_none() && (poked || Instant::now() >= self.next_panel) {
            self.next_panel = Instant::now() + PANEL_EVERY;
            let m = mtime_f(&self.url);
            if m.is_some() && (poked || m != self.seen_mtime) {
                self.seen_mtime = m;
                if let Some(v) = read_panel_url(&self.url) {
                    self.publish(v);
                }
            }
        }
        if poked && self.stale() {
            self.next_payload = Instant::now();
        }
        if Instant::now() >= self.next_payload {
            self.next_payload = Instant::now() + PAYLOAD_EVERY;
            let b = payload::build(PANEL_INTERVAL, self.prov.is_none(), self.prov.as_deref());
            self.publish(b.payload);
        }
        if Instant::now() >= self.next_activity {
            self.next_activity = Instant::now() + ACTIVITY_EVERY;
            let a = activity::measure().to_json();
            let mut s = self.shared.lock().unwrap();
            s.activity = Some(a);
            s.version += 1;
        }
    }
}

/// The payload the bar last packed for the panel: `file://…/panel.html#<base64 JSON>`.
fn read_panel_url(p: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(p).ok()?;
    let (_, b64) = text.trim().rsplit_once('#')?;
    let v: Value = serde_json::from_slice(&base64_decode(b64)?).ok()?;
    v.get("windows")?.as_array()?;
    Some(v)
}

// ---- the ECG of panel.html, one sample per braille dot column ---------------------------
#[derive(Default)]
struct Ecg {
    cols: Vec<f64>,
    head: usize,
    ph: f64,
    acc: f64,
    beat: f64,
}

impl Ecg {
    fn init(&mut self, w: usize) {
        if w != self.cols.len() {
            self.cols = vec![0.0; w];
            self.head = 0;
        }
    }

    fn shape(p: f64) -> f64 {
        // P, Q, R, S, T as five gaussians over one beat
        let g = |a: f64, m: f64, s: f64| a * (-((p - m) * (p - m)) / (2.0 * s * s)).exp();
        g(0.12, 0.16, 0.03) + g(-0.12, 0.31, 0.012) + g(1.0, 0.335, 0.013) + g(-0.28, 0.362, 0.012) + g(0.26, 0.56, 0.05)
    }

    fn step(&mut self, dt: f64, bpm: f64) {
        let w = self.cols.len();
        if w == 0 {
            return;
        }
        let speed = w as f64 / SWEEP;
        self.acc += speed * dt;
        while self.acc >= 1.0 {
            self.head = (self.head + 1) % w;
            if bpm > 0.0 {
                let (b, dph) = (self.ph, (bpm / 60.0) / speed);
                self.ph = (self.ph + dph) % 1.0;
                if b < 0.335 && self.ph >= 0.335 {
                    self.beat = 1.0;
                }
                // a dot column covers dph of the beat: keep its extreme, so the narrow R spike never falls between samples
                let mut v = 0.0f64;
                for k in 0..4 {
                    let s = Self::shape((b + dph * k as f64 / 4.0) % 1.0);
                    if s.abs() > v.abs() {
                        v = s;
                    }
                }
                self.cols[self.head] = v;
            } else {
                self.cols[self.head] = 0.0;
            }
            self.acc -= 1.0;
        }
        self.beat = (self.beat - dt * 4.0).max(0.0);
    }
}

/// Paints the trace straight into the braille grid: a vertical run joins consecutive samples,
/// the newest third is bright, the oldest dim, and a blank gap runs ahead of the head.
struct Trace<'a> {
    ecg: &'a Ecg,
    rows: usize,
    color: Color,
    truecolor: bool,
}

impl Shape for Trace<'_> {
    fn draw(&self, p: &mut Painter) {
        let e = self.ecg;
        let w = e.cols.len();
        if w == 0 || self.rows == 0 {
            return;
        }
        let dh = (self.rows * 4) as i64;
        let base = (dh as f64 * 0.62) as i64;
        let amp = (base - 1).max(1) as f64;
        let gap = (w / 12).max(4);
        let mut prev: Option<i64> = None;
        for i in 0..w {
            let age = (e.head + w - i) % w;
            if age > w - gap {
                prev = None;
                continue;
            }
            let y = (base - rnd(e.cols[i] * amp)).clamp(0, dh - 1);
            let (lo, hi) = match prev {
                None => (y, y),
                Some(q) => (q.min(y), q.max(y)),
            };
            let keep = if age < w / 3 {
                1.0
            } else if age < 2 * w / 3 {
                0.7
            } else {
                0.4
            };
            let c = faded(self.color, keep, self.truecolor);
            for yy in lo..=hi {
                p.paint(i, yy as usize, c);
            }
            prev = Some(y);
        }
    }
}

// ---- themes: the panel's palettes, truecolor or the 16 ANSI colours ------------------------
#[derive(Clone, Copy)]
struct Role(u32, Color);

struct Palette {
    title: Role,
    text: Role,
    dim: Role,
    ok: Role,
    warn: Role,
    bad: Role,
    accent: Role,
}

fn palette(name: &str) -> Palette {
    use Color::*;
    match name {
        "modern" => Palette {
            title: Role(0xf5f5f7, White),
            text: Role(0xf5f5f7, White),
            dim: Role(0x98989d, DarkGray),
            ok: Role(0x30d158, LightGreen),
            warn: Role(0xffd60a, LightYellow),
            bad: Role(0xff453a, LightRed),
            accent: Role(0x0a84ff, LightBlue),
        },
        "cyber" => Palette {
            title: Role(0x00f0ff, LightCyan),
            text: Role(0xdbe4ff, White),
            dim: Role(0x4a5080, Blue),
            ok: Role(0x00f0ff, LightCyan),
            warn: Role(0xf9f002, LightYellow),
            bad: Role(0xff3b5c, LightRed),
            accent: Role(0xff2bd6, LightMagenta),
        },
        "synth" => Palette {
            title: Role(0xff2d95, LightMagenta),
            text: Role(0xffffff, White),
            dim: Role(0xb39ddb, Magenta),
            ok: Role(0x22e6ff, LightCyan),
            warn: Role(0xffb347, LightYellow),
            bad: Role(0xff4d4d, LightRed),
            accent: Role(0xb39ddb, LightMagenta),
        },
        "analog" => Palette {
            title: Role(0xefe6cf, LightYellow),
            text: Role(0xefe6cf, LightYellow),
            dim: Role(0x8a8069, DarkGray),
            ok: Role(0xc9a227, Yellow),
            warn: Role(0xff9f1c, LightYellow),
            bad: Role(0xff5a5a, LightRed),
            accent: Role(0xc9a227, Yellow),
        },
        _ => Palette {
            title: Role(0xb9ffcb, LightGreen),
            text: Role(0xb9ffcb, LightGreen),
            dim: Role(0x3d8a52, Green),
            ok: Role(0x5cff8a, LightGreen),
            warn: Role(0xffb63b, LightYellow),
            bad: Role(0xff5a5a, LightRed),
            accent: Role(0x8fd3ff, LightCyan),
        },
    }
}

fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8 & 0xff) as u8, (hex & 0xff) as u8)
}

fn faded(c: Color, keep: f64, truecolor: bool) -> Color {
    use Color::*;
    match c {
        Rgb(r, g, b) if truecolor => Rgb((r as f64 * keep) as u8, (g as f64 * keep) as u8, (b as f64 * keep) as u8),
        _ if keep >= 0.6 => c,
        LightGreen => Green,
        LightRed => Red,
        LightYellow => Yellow,
        LightCyan => Cyan,
        LightMagenta => Magenta,
        LightBlue => Blue,
        White => Gray,
        Gray => DarkGray,
        _ => c,
    }
}

// ---- state ---------------------------------------------------------------------------------
#[derive(Clone)]
struct Win {
    label: String,
    pct: f64,
    resets: Option<f64>,
}

fn wins(v: Option<&Value>) -> Vec<Win> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|w| {
                    Some(Win {
                        label: w.get("label")?.as_str()?.to_string(),
                        pct: w.get("pct").and_then(Value::as_f64).unwrap_or(0.0),
                        resets: w.get("resets").and_then(Value::as_str).and_then(epoch_of_f),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// CLAUDE SESSION, GROK 7D: a window named after a model or a product (FABLE, GPT-5) keeps its own name.
fn qualify(provider: &str, w: &Win) -> Win {
    Win { label: crate::util::qualified(provider, &w.label), ..w.clone() }
}

/// An activity reading: tokens/min, idle seconds, sessions; None when there is none.
type Act = Option<(f64, f64, i64)>;

/// Null and absent are no reading.
fn act_of(a: Option<&Value>) -> Act {
    let a = a?;
    Some((
        a.get("tok_per_min")?.as_f64()?,
        a.get("idle_s").and_then(Value::as_f64).unwrap_or(0.0),
        a.get("sessions").and_then(Value::as_i64).unwrap_or(0),
    ))
}

/// The rate of a trace: nothing streaming is a flat line.
fn bpm_of(act: Act) -> f64 {
    match act {
        Some((tok, _, _)) if tok > 0.0 => (60.0 + 60.0 * (1.0 + tok / 100.0).log10()).min(180.0),
        _ => 0.0,
    }
}

fn busy_of(act: Act) -> bool {
    matches!(act, Some((tok, _, _)) if tok > 0.0)
}

fn label_of(act: Act) -> Option<String> {
    let (tok, idle, sessions) = act?;
    Some(if tok > 0.0 {
        format!("{} TOK/MIN{}", fmt_k(tok as i64), if sessions > 1 { format!(" · {sessions} SESSIONS") } else { String::new() })
    } else {
        format!("IDLE {}", short(idle as i64))
    })
}

/// A provider's headline window: SESSION, or its first one when it has none (a weekly pool only).
fn head_i(ws: &[Win]) -> Option<usize> {
    ws.iter().position(|w| w.label == "SESSION").or(if ws.is_empty() { None } else { Some(0) })
}

/// One trace of several: a provider with an activity reading of its own, when more than one has one.
struct Lane {
    key: String,  // the provider as the payload spells it
    name: String, // upper-cased: the label
    active: bool, // the active provider's lane follows the feed's live reading
    act: Act,
    pct: Option<f64>,  // its headline window, for the colour
    head: Option<Win>, // that window, qualified, for the headline beside the trace; None for the active lane (the big number's) and with no windows
    status: String,    // why there is no window, when there is none
    ecg: Ecg,
}

impl Lane {
    fn bpm(&self, alive: bool) -> f64 {
        if self.active && !alive {
            0.0
        } else {
            bpm_of(self.act)
        }
    }
}

struct App {
    feed: Feed,
    ecg: Ecg,              // the single trace of the active provider
    lanes: Vec<Lane>,      // one per provider with an activity reading, when more than one has one; else empty
    multi: bool,           // more than one provider on the monitor: every SESSION and WEEK window says whose it is
    d: Map<String, Value>, // the payload, merged like Object.assign in the panel
    session: Option<Win>,
    others: Vec<Win>,
    session_pct: i64,
    alive: bool,
    act: Act,                // the active provider's reading, the feed's live one first
    est: Option<(f64, f64)>, // dead-reckoned %, API %
    seen: u64,
    theme: usize,
    help: bool,
    truecolor: bool,
    tz: i64,
}

impl App {
    fn apply(&mut self) -> bool {
        let (payload, activity) = {
            let s = self.feed.shared.lock().unwrap();
            if s.version == self.seen {
                return false;
            }
            self.seen = s.version;
            (s.payload.clone(), s.activity.clone())
        };
        if let Some(Value::Object(m)) = payload {
            for (k, v) in m {
                self.d.insert(k, v);
            }
        }
        let windows = wins(self.d.get("windows"));
        let providers = self.d.get("providers").and_then(Value::as_array).cloned().unwrap_or_default();
        let provider = self.str("provider");
        self.multi = providers.len() > 1 && !provider.is_empty();
        // SESSION is the big number; a provider with no session window (a weekly pool only) shows its first one there
        let session_i = head_i(&windows);
        self.session = session_i.map(|i| windows[i].clone());
        // the lane sources: providers with an activity reading of their own (providers[i].activity; absent counts as null)
        let srcs: Vec<(&str, Act, Vec<Win>, String)> = providers
            .iter()
            .filter_map(|p| {
                let (name, act) = (p.get("name")?.as_str()?, act_of(p.get("activity"))?);
                let status = p.get("status").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("NO DATA");
                Some((name, Some(act), wins(p.get("windows")), status.to_string()))
            })
            .collect();
        let laned = |name: &str| srcs.len() > 1 && srcs.iter().any(|(n, ..)| *n == name);
        // with more than one provider on, every window says whose it is, and the other providers' windows join the bars,
        // except a lane's headline window, drawn beside its trace
        let multi = self.multi;
        self.others = windows
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != session_i)
            .map(|(_, w)| if multi { qualify(&provider, w) } else { w.clone() })
            .collect();
        for p in &providers {
            let Some(name) = p.get("name").and_then(Value::as_str).filter(|n| !n.is_empty() && *n != provider.as_str()) else { continue };
            let ws = wins(p.get("windows"));
            let head = if laned(name) { head_i(&ws) } else { None };
            self.others.extend(ws.iter().enumerate().filter(|(i, _)| Some(*i) != head).map(|(_, w)| qualify(name, w)));
        }
        self.alive = !windows.is_empty();
        self.session_pct = self.session.as_ref().map(|s| rnd(s.pct)).unwrap_or(0);
        self.est = None;
        if let Some(e) = self.d.get("estimate") {
            if let Some(pe) = e.get("pct_est").and_then(Value::as_f64) {
                self.est = Some((pe, e.get("pct_api").and_then(Value::as_f64).unwrap_or(pe)));
                if self.session.is_some() && e.get("calibrated").and_then(Value::as_bool) == Some(true) {
                    self.session_pct = rnd(pe);
                }
            }
        }
        let act = activity.or_else(|| self.d.get("activity").cloned());
        self.act = act_of(act.as_ref());
        // one lane per source when more than one has a reading; a lane keeps its trace across payload refreshes
        let mut old = std::mem::take(&mut self.lanes);
        if srcs.len() > 1 {
            self.lanes = srcs
                .into_iter()
                .map(|(name, act, ws, status)| {
                    let (active, head) = (name == provider, head_i(&ws).map(|i| ws[i].clone()));
                    Lane {
                        key: name.to_string(),
                        name: name.to_ascii_uppercase(),
                        active,
                        act: if active { self.act } else { act },
                        pct: head.as_ref().map(|w| w.pct),
                        head: if active { None } else { head.map(|w| qualify(name, &w)) },
                        status,
                        ecg: old.iter().position(|l| l.key == name).map(|i| old.remove(i).ecg).unwrap_or_default(),
                    }
                })
                .collect();
        }
        true
    }

    /// A lane's headline: its window's label, the percentage, the reset (or why there is none). The active lane's
    /// is the big number's, estimate and EST marker included; the others carry their own.
    fn headline(&self, lane: &Lane, wide: bool) -> (String, String, String) {
        if lane.active {
            let big = if self.alive { format!("{}%", self.session_pct) } else { "--".to_string() };
            let sub = match (&self.session, self.alive) {
                (Some(s), true) => self.reset_text(s, wide),
                _ => "NO SIGNAL".to_string(),
            };
            (self.session_label(), big, sub)
        } else if let Some(w) = &lane.head {
            (w.label.clone(), format!("{}%", rnd(w.pct)), self.reset_text(w, wide))
        } else {
            (format!("{} SESSION", lane.name), "--".to_string(), lane.status.clone())
        }
    }

    /// The caption over the big number: SESSION, or whose window it is when more than one provider is on.
    fn session_label(&self) -> String {
        match (&self.session, self.multi) {
            (Some(s), true) => qualify(&self.str("provider"), s).label,
            (None, true) => format!("{} SESSION", self.str("provider").to_ascii_uppercase()),
            _ => "SESSION".into(),
        }
    }

    fn str(&self, key: &str) -> String {
        self.d.get(key).and_then(Value::as_str).unwrap_or("").to_string()
    }

    fn status(&self) -> String {
        self.str("status")
    }

    fn estimating(&self) -> bool {
        matches!(self.est, Some((pe, pa)) if rnd(pe) != rnd(pa))
    }

    fn bpm(&self) -> f64 {
        if !self.alive {
            return 0.0;
        }
        match self.act {
            None => 60.0, // no reading yet: a resting pulse
            act => bpm_of(act),
        }
    }

    fn busy(&self) -> bool {
        busy_of(self.act)
    }

    fn activity_label(&self) -> Option<String> {
        label_of(self.act)
    }

    fn age(&self) -> f64 {
        (now() - self.d.get("fetched").and_then(Value::as_f64).unwrap_or(0.0)).max(0.0)
    }

    fn fresh(&self) -> bool {
        self.alive && self.status().is_empty() && self.age() < 120.0
    }

    fn status_info(&self, t: f64) -> (String, &'static str) {
        let st = self.status();
        if !self.alive {
            (format!("? {st}"), "bad")
        } else if !st.is_empty() {
            (format!("○ {st}"), "warn")
        } else if self.fresh() {
            (format!("{}LIVE", if (t as i64) % 2 == 1 { "● " } else { "  " }), "ok")
        } else {
            (String::new(), "dim")
        }
    }

    fn reset_text(&self, w: &Win, wide: bool) -> String {
        let left = w.resets.map(|r| span((r - now()) as i64)).unwrap_or_else(|| "?".into());
        let mut t = if wide { format!("RESET {left}") } else { left };
        if w.label == "SESSION" && self.estimating() {
            t.push_str(if wide { " · EST" } else { "·EST" });
        }
        t
    }

    fn next_reset(&self) -> String {
        let n = now();
        let next = self.session.iter().chain(self.others.iter()).filter_map(|w| w.resets).filter(|r| *r > n).fold(f64::INFINITY, f64::min);
        if next.is_finite() {
            hhmm(next as i64, self.tz)
        } else {
            "?".into()
        }
    }

    fn buckets(&self, n: usize) -> Vec<f64> {
        // 12 h of session history in n buckets: max per bucket, -1 when empty
        let (t, size) = (now(), HISTORY_HOURS * 3600.0 / n as f64);
        let mut mx = vec![-1.0f64; n];
        for row in self.d.get("history").and_then(Value::as_array).into_iter().flatten() {
            let (Some(ts), Some(pct)) = (row.get(0).and_then(Value::as_f64), row.get(1).and_then(Value::as_f64)) else { continue };
            let b = n as i64 - 1 - ((t - ts) / size).floor() as i64;
            if b >= 0 && (b as usize) < n {
                mx[b as usize] = mx[b as usize].max(pct);
            }
        }
        mx
    }

    fn color(&self, r: Role) -> Color {
        if self.truecolor {
            rgb(r.0)
        } else {
            r.1
        }
    }

    fn tone(&self, p: &Palette, pct: f64) -> Color {
        self.color(if pct >= 85.0 {
            p.bad
        } else if pct >= 60.0 {
            p.warn
        } else {
            p.ok
        })
    }
}

// ---- layout ---------------------------------------------------------------------------------
struct Layout {
    too_small: bool,
    m: u16,
    wide: bool,
    big: String,
    reset: String,
    caption: String,
    lanes: Vec<(u16, u16, u16)>,          // per lane: its label row, the first trace row, the trace rows; empty for the single trace
    heads: Vec<(String, String, String)>, // per lane: its headline's label, percentage and reset
    right_w: u16,
    trace_x: u16,
    trace_w: u16,
    n_win: usize,
    rule_y: Option<u16>,
    label_y: u16,
    body_y: u16,
    body_h: u16,
    bar_y: u16,
    win_y: u16,
    spark_label_y: Option<u16>,
    spark_y: u16,
    spark_h: u16,
    axis_y: Option<u16>,
    footer_y: u16,
    label_w: u16,
    reset_in_body: bool,
}

fn layout(app: &mut App, w: u16, h: u16) -> Layout {
    let (wi, hi) = (w as i32, h as i32);
    let m: i32 = if w >= 70 { 2 } else { 1 };
    let wide = w >= 60;
    let big = if app.alive { format!("{}%", app.session_pct) } else { "--".to_string() };
    let reset = match (&app.session, app.alive) {
        (Some(s), true) => app.reset_text(s, wide),
        _ => String::new(),
    };
    let dw = big.chars().count() as i32 * 4 - 1;
    let caption = app.session_label();
    // with lanes, one headline each in the right column, as wide as the longest of their lines;
    // 15 = "100%": the split does not jump between two- and three-digit readings
    let heads: Vec<(String, String, String)> = app.lanes.iter().map(|l| app.headline(l, wide)).collect();
    let longest = heads.iter().flat_map(|(a, b, c)| [a, b, c]).map(|s| s.chars().count() as i32).max();
    let right_w = longest.unwrap_or_else(|| dw.max(reset.chars().count() as i32).max(caption.chars().count() as i32)).max(7).max(if wide { 15 } else { 0 });
    let trace_w = (wi - 2 * m - right_w - 2).max(2);
    let base = 1 + 7 + 1 + 1; // header, trace block (label + 5 + bar), sparkline, footer
    let n_win = (app.others.len() as i32).min(4).min(hi - base).max(0);
    let spare = hi - base - n_win;
    let rule = spare >= 1;
    let gap_trace = spare >= 2;
    let spark_label = spare >= 3;
    let axis = spare >= 4;
    let gap_spark = spare >= 5;
    let mut left = (spare - 6).max(0);
    let mut body_h = 5 + left.min(4); // the trace grows first, to 9 rows
    left -= left.min(4);
    let spark_h = 1 + left.min(3); // then the sparkline, to 4
    left -= left.min(3);
    body_h += left.min(3); // then the trace again, to 12: taller would dwarf the digits
    left -= left.min(3);
    let gap_trace = gap_trace as i32 + left.min(2); // what is left widens the gaps; the rest stays above the footer
    let reset_in_body = body_h >= 7; // room for the reset under the digits; the bar then spans the width
    let mut y = 1;
    let rule_y = if rule {
        y += 1;
        Some(1)
    } else {
        None
    };
    let (label_y, body_y) = (y, y + 1);
    y += 1 + body_h;
    let bar_y = y;
    y += 1 + gap_trace;
    let win_y = y;
    y += n_win + if gap_spark && n_win > 0 { 1 } else { 0 };
    let spark_label_y = if spark_label {
        y += 1;
        Some(y - 1)
    } else {
        None
    };
    let spark_y = y;
    y += spark_h;
    let axis_y = if axis { Some(y) } else { None };
    // a provider-qualified label (CLAUDE SESSION) needs 14 columns; without one 8 is plenty
    let label_w = app.others.iter().take(n_win as usize).map(|o| o.label.chars().count()).max().unwrap_or(4).clamp(4, if app.multi { 14 } else { 8 });
    app.ecg.init(trace_w as usize * 2);
    // the lanes share the label row and the body: each gets a label row and the trace rows under it, the first ones a row more
    let total = 1 + body_h;
    let n = (app.lanes.len() as i32).min(total / 2);
    let mut lanes = Vec::with_capacity(n as usize);
    let mut ly = label_y;
    for i in 0..n {
        let rows = total / n + if i < total % n { 1 } else { 0 };
        lanes.push((ly as u16, (ly + 1) as u16, (rows - 1) as u16));
        ly += rows;
    }
    for lane in app.lanes.iter_mut() {
        lane.ecg.init(trace_w as usize * 2);
    }
    Layout {
        too_small: h < MIN_ROWS || w < MIN_COLS,
        m: m as u16,
        wide,
        big,
        reset,
        caption,
        lanes,
        heads,
        right_w: right_w as u16,
        trace_x: m as u16,
        trace_w: trace_w as u16,
        n_win: n_win as usize,
        rule_y: rule_y.map(|v| v as u16),
        label_y: label_y as u16,
        body_y: body_y as u16,
        body_h: body_h as u16,
        bar_y: bar_y as u16,
        win_y: win_y as u16,
        spark_label_y: spark_label_y.map(|v| v as u16),
        spark_y: spark_y as u16,
        spark_h: spark_h as u16,
        axis_y: axis_y.map(|v| v as u16),
        footer_y: h - 1,
        label_w: label_w as u16,
        reset_in_body,
    }
}

// ---- drawing --------------------------------------------------------------------------------
fn rect(x: u16, y: u16, w: u16, h: u16, bounds: Rect) -> Rect {
    Rect::new(x, y, w, h).intersection(bounds)
}

fn bold(c: Color) -> Style {
    Style::new().fg(c).add_modifier(Modifier::BOLD)
}

impl Layout {
    /// The braille trace of `ecg` in the trace column, rows [y, y + h).
    fn trace(&self, f: &mut Frame, area: Rect, ecg: &Ecg, (y, h): (u16, u16), color: Color, truecolor: bool) {
        let shape = Trace { ecg, rows: h as usize, color, truecolor };
        let canvas = Canvas::default()
            .marker(Marker::Braille)
            .x_bounds([0.0, (self.trace_w as f64 * 2.0 - 1.0).max(1.0)])
            .y_bounds([0.0, (h as f64 * 4.0 - 1.0).max(1.0)])
            .paint(|ctx| ctx.draw(&shape));
        f.render_widget(canvas, rect(self.trace_x, y, self.trace_w, h, area));
    }
}

fn render(f: &mut Frame, app: &App, l: &Layout, t: f64) {
    let area = f.area();
    let p = palette(THEMES[app.theme]);
    let (w, _h) = (area.width, area.height);
    let (m, dim) = (l.m, Style::new().fg(app.color(p.dim)));
    if l.too_small {
        f.render_widget(Paragraph::new("TOO SMALL").alignment(Alignment::Center).style(bold(app.color(p.bad))), rect(0, area.height / 2, w, 1, area));
        return;
    }
    // one line of text at (x, y): the workhorse of this screen
    macro_rules! text {
        ($y:expr, $x:expr, $w:expr, $line:expr, $align:expr) => {
            f.render_widget(Paragraph::new($line).alignment($align), rect($x, $y, $w, 1, area))
        };
    }
    // header: title left, plan and status right
    f.render_widget(Paragraph::new(Line::styled("PULSE LIMITS", bold(app.color(p.title)))), rect(m, 0, w - 2 * m, 1, area));
    let (st, kind) = app.status_info(t);
    let st_style = match kind {
        "ok" => Style::new().fg(app.color(p.ok)),
        "warn" => Style::new().fg(app.color(p.warn)),
        "bad" => bold(app.color(p.bad)),
        _ => dim,
    };
    // the plan badge names the provider when more than one is on, as the panel does
    let mut plan = app.str("plan");
    let provider = app.str("provider");
    if !provider.is_empty() && app.d.get("providers").and_then(Value::as_array).is_some_and(|a| a.len() > 1) {
        plan = if plan.is_empty() { provider.to_ascii_uppercase() } else { format!("{} · {plan}", provider.to_ascii_uppercase()) };
    }
    let mut right = vec![Span::styled(st.clone(), st_style)];
    if !plan.is_empty() && (w as usize) >= 2 * m as usize + 14 + plan.len() + 2 + st.len() {
        right.insert(0, Span::styled(format!("{plan}  "), bold(app.color(p.accent))));
    }
    f.render_widget(Paragraph::new(Line::from(right)).alignment(Alignment::Right), rect(m + 14, 0, w.saturating_sub(2 * m + 14), 1, area));
    if let Some(y) = l.rule_y {
        f.render_widget(Paragraph::new(Line::styled("─".repeat((w - 2 * m) as usize), dim)), rect(m, y, w - 2 * m, 1, area));
    }
    // the trace, its activity label, and NO SIGNAL over a flat line when there is no reading
    let col = if !app.alive {
        app.color(p.dim)
    } else if !app.status().is_empty() {
        app.color(p.warn)
    } else {
        app.tone(&p, app.session_pct as f64)
    };
    if l.lanes.is_empty() {
        if let Some(mut lab) = app.activity_label() {
            if lab.chars().count() + 2 > l.trace_w as usize {
                lab = lab.split(" · ").next().unwrap_or_default().to_string(); // narrow: drop the session count
            }
            let live = if app.busy() { app.color(p.ok) } else { app.color(p.dim) };
            let dot = Style::new().fg(live).add_modifier(if app.ecg.beat > 0.3 { Modifier::BOLD } else { Modifier::DIM });
            let line = Line::from(vec![Span::styled("● ", dot), Span::styled(lab, Style::new().fg(live))]);
            f.render_widget(Paragraph::new(line), rect(l.trace_x, l.label_y, l.trace_w, 1, area));
        }
        l.trace(f, area, &app.ecg, (l.body_y, l.body_h), col, app.truecolor);
    }
    // one lane per provider: the dot, its name, its own rate, and its trace, coloured by its own session window
    let right_x = w - m - l.right_w;
    for ((lane, &(ly, by, bh)), (label, pct, sub)) in app.lanes.iter().zip(&l.lanes).zip(&l.heads) {
        let live = if busy_of(lane.act) { app.color(p.ok) } else { app.color(p.dim) };
        let dot = Style::new().fg(live).add_modifier(if lane.ecg.beat > 0.3 { Modifier::BOLD } else { Modifier::DIM });
        let mut spans = vec![Span::styled("● ", dot), Span::styled(lane.name.clone(), bold(app.color(p.text)))];
        if let Some(mut lab) = label_of(lane.act) {
            let room = (l.trace_w as usize).saturating_sub(4 + lane.name.chars().count()); // after "● NAME  "
            if lab.chars().count() > room {
                lab = lab.split(" · ").next().unwrap_or_default().to_string(); // narrow: drop the session count
            }
            if lab.chars().count() <= room {
                spans.push(Span::styled(format!("  {lab}"), Style::new().fg(live)));
            }
        }
        f.render_widget(Paragraph::new(Line::from(spans)), rect(l.trace_x, ly, l.trace_w, 1, area));
        let color = if lane.active { col } else { lane.pct.map(|v| app.tone(&p, v)).unwrap_or(app.color(p.dim)) };
        l.trace(f, area, &lane.ecg, (by, bh), color, app.truecolor);
        // its headline in the right column, level with the lane: the label on the name row, the percentage on the
        // first trace row (a lane always has one), the reset on the second when the lane has it
        let dead = if lane.active { !app.alive } else { lane.head.is_none() };
        text!(ly, right_x, l.right_w, Line::styled(label.clone(), dim), Alignment::Right);
        text!(by, right_x, l.right_w, Line::styled(pct.clone(), bold(color)), Alignment::Right);
        if bh >= 2 {
            text!(by + 1, right_x, l.right_w, Line::styled(sub.clone(), if dead { bold(app.color(p.bad)) } else { dim }), Alignment::Right);
        }
    }
    if !app.alive {
        let flat = l.body_y + ((l.body_h as f64 * 4.0 * 0.62) as u16) / 4; // the row the flat line runs through
        if (t * 2.0) as i64 % 2 == 0 {
            text!(flat.saturating_sub(2), l.trace_x, l.trace_w, Line::styled("NO SIGNAL", bold(app.color(p.bad))), Alignment::Center);
        }
        text!(flat.saturating_sub(1), l.trace_x, l.trace_w, Line::styled(format!("? {}", app.status()), bold(app.color(p.bad))), Alignment::Center);
        let hint = app.str("hint");
        let hint_w = if hint.chars().count() <= l.trace_w as usize { l.trace_w } else { w - 2 * m }; // the digits' rows there are blank
        text!(flat + 1, l.trace_x, hint_w, Line::styled(hint, dim), Alignment::Center);
    }
    // the session: label, big digits, reset (with lanes, each lane's headline says these), then the bar
    if l.lanes.is_empty() {
        text!(l.label_y, right_x, l.right_w, Line::styled(l.caption.clone(), dim), Alignment::Right);
        let top = l.body_y + (l.body_h - if l.reset_in_body { 6 } else { 5 }) / 2; // digits (and their reset line) centred in the body
        for r in 0..5 {
            let row = l.big.chars().map(|c| glyph(c)[r].replace('#', "█")).collect::<Vec<_>>().join(" ");
            text!(top + r as u16, right_x, l.right_w, Line::styled(row, bold(col)), Alignment::Right);
        }
        if app.alive {
            let ry = if l.reset_in_body { top + 5 } else { l.bar_y };
            text!(ry, right_x, l.right_w, Line::styled(l.reset.clone(), dim), Alignment::Right);
        }
    }
    let bw = if l.reset_in_body || !app.alive || !l.lanes.is_empty() { w - 2 * m } else { l.trace_w } as usize;
    let filled = if app.alive { ((app.session_pct as usize * bw + 50) / 100).min(bw) } else { 0 };
    let bar = Line::from(vec![Span::styled("█".repeat(filled), Style::new().fg(col)), Span::styled("░".repeat(bw - filled), dim)]);
    text!(l.bar_y, m, bw as u16, bar, Alignment::Left);
    // the other windows as bars
    for (i, win) in app.others.iter().take(l.n_win).enumerate() {
        let (y, pct) = (l.win_y + i as u16, rnd(win.pct));
        let tone = app.tone(&p, pct as f64);
        let mut reset = app.reset_text(win, l.wide);
        let mut tail = 5 + if reset.is_empty() { 0 } else { reset.len() + 2 };
        if (w as usize) < 2 * m as usize + l.label_w as usize + 1 + 6 + tail {
            reset.clear();
            tail = 5;
        }
        let gauge_w = (w as usize).saturating_sub(2 * m as usize + tail + 1) as u16;
        let label: String = win.label.chars().take(l.label_w as usize).collect();
        let gauge = LineGauge::default()
            .ratio((pct as f64 / 100.0).clamp(0.0, 1.0))
            .label(Line::styled(format!("{:<width$}", label, width = l.label_w as usize), dim))
            .filled_symbol("█")
            .unfilled_symbol("░")
            .filled_style(Style::new().fg(tone))
            .unfilled_style(dim);
        f.render_widget(gauge, rect(m, y, gauge_w, 1, area));
        let mut spans = vec![Span::styled(format!("{pct:3}%"), bold(tone))];
        if !reset.is_empty() {
            spans.push(Span::styled(format!("  {reset}"), dim));
        }
        text!(y, m + gauge_w + 1, (tail) as u16, Line::from(spans), Alignment::Right);
    }
    // 12 h of history as a sparkline
    if let Some(y) = l.spark_label_y {
        text!(y, m, w - 2 * m, Line::styled("SESSION · 12H", dim), Alignment::Left);
    }
    let buckets = app.buckets((w - 2 * m) as usize);
    let bars: Vec<SparklineBar> = buckets
        .iter()
        .map(
            |&v| {
                if v < 0.0 {
                    SparklineBar::from(None)
                } else {
                    SparklineBar::from(Some((v.max(1.0)) as u64)).style(Some(Style::new().fg(app.tone(&p, v))))
                }
            },
        )
        .collect();
    let spark = Sparkline::default().data(bars).max(100).style(dim).absent_value_symbol(" ");
    let spark_area = rect(m, l.spark_y, w - 2 * m, l.spark_h, area);
    f.render_widget(spark, spark_area);
    let baseline = spark_area.bottom().saturating_sub(1); // empty buckets: a dotted baseline, like the panel's 1 px line
    for (i, _) in buckets.iter().enumerate().filter(|(_, v)| **v < 0.0) {
        if let Some(cell) = f.buffer_mut().cell_mut((m + i as u16, baseline)) {
            cell.set_symbol("·").set_style(dim);
        }
    }
    if let Some(y) = l.axis_y {
        text!(y, m, w - 2 * m, Line::styled("-12H", dim), Alignment::Left);
        text!(y, m, w - 2 * m, Line::styled("NOW", dim), Alignment::Right);
    }
    // footer: freshness, credits, keys
    let status = app.status();
    let (left, left_style) = if !status.is_empty() && app.alive {
        (
            format!("? {status} · LAST GOOD {} AGO", short(app.age() as i64)),
            Style::new().fg(app.color(if (t * 2.0) as i64 % 2 == 1 { p.bad } else { p.warn })),
        )
    } else if app.alive {
        (format!("UPDATED {} AGO · NEXT RESET {}", short(app.age() as i64), app.next_reset()), dim)
    } else {
        (app.str("hint"), dim)
    };
    let left_w = left.chars().count();
    text!(l.footer_y, m, w - 2 * m, Line::styled(left, left_style), Alignment::Left);
    let mut right: Vec<Span> = vec![];
    let mut right_w = 0usize;
    let keys = "q quit  t theme  r reload  ? help";
    let credits = app
        .d
        .get("credits")
        .and_then(|c| Some(format!("CREDITS {:.2} {}", c.get("used")?.as_f64()?, c.get("currency").and_then(Value::as_str).unwrap_or(""))))
        .map(|s| s.trim_end().to_string());
    if let Some(c) = credits {
        if (w as usize) >= 2 * m as usize + left_w + 2 + c.len() {
            right_w = c.len();
            right.push(Span::styled(c, Style::new().fg(app.color(p.warn))));
        }
    }
    if (w as usize) >= 2 * m as usize + left_w + 2 + right_w + keys.len() + if right_w > 0 { 3 } else { 0 } {
        if right_w > 0 {
            right.push(Span::raw("   "));
        }
        right.push(Span::styled(keys, dim));
    }
    text!(l.footer_y, m, w - 2 * m, Line::from(right), Alignment::Right);
    if app.help {
        render_help(f, app, &p);
    }
}

fn render_help(f: &mut Frame, app: &App, p: &Palette) {
    let area = f.area();
    let source = match &app.feed.provider {
        Some(prov) => format!("payload   built in process every {}s, for {prov} (panel.url left alone)", PAYLOAD_EVERY.as_secs()),
        None => format!(
            "payload   {} every {}s\n          built in process every {}s (once now if that file is stale)",
            app.feed.panel_url.display(),
            PANEL_EVERY.as_secs(),
            PAYLOAD_EVERY.as_secs()
        ),
    };
    let mut lines = vec![
        "q   quit".to_string(),
        format!("t   next theme (now {})", THEMES[app.theme]),
        "r   re-read the payload now".to_string(),
        "?   close this help".to_string(),
        String::new(),
    ];
    lines.extend(source.lines().map(str::to_string));
    lines.push(format!("activity  {} every {}s", activity::projects_dir().display(), ACTIVITY_EVERY.as_secs()));
    let w = (lines.iter().map(|s| s.chars().count()).max().unwrap_or(0) as u16 + 4).min(area.width.saturating_sub(2));
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let r = Rect::new((area.width - w) / 2, (area.height - h) / 2, w, h);
    f.render_widget(Clear, r);
    let text: Vec<Line> = lines
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let style = if i < 4 { bold(app.color(p.text)) } else { Style::new().fg(app.color(p.text)) };
            Line::styled(format!(" {s}"), style)
        })
        .collect();
    let block = Block::bordered().title(" PULSE LIMITS ").border_style(Style::new().fg(app.color(p.dim))).title_style(bold(app.color(p.title)));
    f.render_widget(Paragraph::new(text).block(block), r);
}

// ---- run --------------------------------------------------------------------------------------
/// A key press; true means quit.
fn key(app: &mut App, k: KeyEvent) -> bool {
    if k.kind != KeyEventKind::Release {
        match k.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => return true,
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return true,
            KeyCode::Char('t') | KeyCode::Char('T') => app.theme = (app.theme + 1) % THEMES.len(),
            KeyCode::Char('r') | KeyCode::Char('R') => app.feed.poke.store(true, Ordering::Relaxed),
            KeyCode::Char('?') => app.help = !app.help,
            _ => {}
        }
    }
    false
}

/// One frame: fold in what the feed published, lay out for the terminal's size, advance the
/// trace by `dt` seconds and draw at time `t`.
fn frame<B: Backend>(terminal: &mut Terminal<B>, app: &mut App, dt: f64, t: f64) -> io::Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    app.apply();
    let size = terminal.size().map_err(io::Error::other)?;
    let l = layout(app, size.width, size.height);
    let (bpm, alive) = (app.bpm(), app.alive);
    app.ecg.step(dt, bpm);
    for lane in app.lanes.iter_mut() {
        let bpm = lane.bpm(alive);
        lane.ecg.step(dt, bpm);
    }
    terminal.draw(|f| render(f, app, &l, t)).map_err(io::Error::other)?;
    Ok(())
}

/// Frames at ~20 fps until q; `next_event` waits up to the given time for a terminal event.
fn run_loop<B: Backend>(terminal: &mut Terminal<B>, app: &mut App, mut next_event: impl FnMut(Duration) -> io::Result<Option<Event>>) -> io::Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let mut last = Instant::now();
    let mut next_frame = Instant::now();
    loop {
        // a resize needs nothing: draw() measures the terminal every frame
        if let Some(Event::Key(k)) = next_event(next_frame.saturating_duration_since(Instant::now()))? {
            if key(app, k) {
                return Ok(());
            }
        }
        if Instant::now() >= next_frame {
            next_frame = Instant::now() + FRAME;
            let dt = last.elapsed().as_secs_f64().min(0.1);
            last = Instant::now();
            frame(terminal, app, dt, now())?;
        }
    }
}

fn usage() -> String {
    format!(
        "usage: pulse-limits tui [{}] [--theme {}]\n  q quit, t next theme, r re-read the payload, ? help",
        providers::KNOWN.join("|"),
        THEMES.join("|")
    )
}

/// The command line: (provider, theme index), or the exit code to leave with.
fn parse(args: &[String]) -> Result<(Option<String>, Option<usize>), i32> {
    let mut provider: Option<String> = None;
    let mut theme: Option<usize> = None;
    let mut args = args.iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("{}", usage());
                return Err(0);
            }
            "--theme" => match args.next().and_then(|n| THEMES.iter().position(|t| t == n)) {
                Some(i) => theme = Some(i),
                None => {
                    eprintln!("pulse-limits tui: unknown theme (one of: {})", THEMES.join(" "));
                    return Err(64);
                }
            },
            s if s.starts_with("--theme=") => match THEMES.iter().position(|t| *t == &s[8..]) {
                Some(i) => theme = Some(i),
                None => {
                    eprintln!("pulse-limits tui: unknown theme (one of: {})", THEMES.join(" "));
                    return Err(64);
                }
            },
            s if s.starts_with('-') => {
                eprintln!("pulse-limits tui: unknown option {s}\n{}", usage());
                return Err(64);
            }
            s => {
                if !providers::known(s) {
                    eprintln!("pulse-limits tui: unknown provider {s} (one of: {})", providers::KNOWN.join(" "));
                    return Err(64);
                }
                provider = Some(s.to_string());
            }
        }
    }
    Ok((provider, theme))
}

impl App {
    /// The screen before the first payload arrives: NO DATA, the given theme.
    fn new(feed: Feed, theme: usize) -> App {
        let mut app = App {
            feed,
            ecg: Ecg::default(),
            lanes: vec![],
            multi: false,
            d: Map::new(),
            session: None,
            others: vec![],
            session_pct: 0,
            alive: false,
            act: None,
            est: None,
            seen: 0,
            theme,
            help: false,
            truecolor: matches!(std::env::var("COLORTERM").as_deref(), Ok("truecolor") | Ok("24bit")),
            tz: local_offset(),
        };
        app.d.insert("status".into(), Value::String("NO DATA".into()));
        app
    }
}

/// `pulse-limits tui [provider] [--theme NAME]`; returns the exit code.
pub fn run(args: &[String]) -> i32 {
    match parse(args) {
        Ok((provider, theme)) => launch(provider, theme),
        Err(code) => code,
    }
}

/// The terminal session: needs a real terminal, so nothing in here runs under `cargo test`.
fn launch(provider: Option<String>, theme: Option<usize>) -> i32 {
    let mut app = App::new(Feed::start(provider), theme.unwrap_or_else(saved_theme));
    let mut terminal = match ratatui::try_init() {
        // raw mode, alternate screen, and a panic hook that restores both
        Ok(t) => t,
        Err(e) => {
            eprintln!("pulse-limits tui: {e} (is this a terminal?)");
            return 1;
        }
    };
    let result = run_loop(&mut terminal, &mut app, |wait| if event::poll(wait)? { event::read().map(Some) } else { Ok(None) });
    ratatui::restore();
    if let Err(e) = result {
        eprintln!("pulse-limits tui: {e}");
        return 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use ratatui::backend::TestBackend;
    use serde_json::json;

    use super::*;
    use crate::providers::testing::{Sandbox, ENV};
    use crate::util::{iso_utc, now as now_i, write_atomic};

    /// A feed with the given documents already published: nothing is spawned.
    fn feed(payload: Option<Value>, activity: Option<Value>, provider: Option<&str>) -> Feed {
        let shared = Shared { payload, activity, version: 1 };
        Feed {
            shared: Arc::new(Mutex::new(shared)),
            poke: Arc::new(AtomicBool::new(false)),
            panel_url: PathBuf::from("/nowhere/panel.url"),
            provider: provider.map(str::to_string),
        }
    }

    /// The screen with that payload folded in, UTC, truecolor.
    fn app(payload: Value, activity: Option<Value>, provider: Option<&str>) -> App {
        let mut a = App::new(feed(Some(payload), activity, provider), 0);
        a.tz = 0;
        a.truecolor = true;
        assert!(a.apply());
        a
    }

    fn press(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    /// One frame on a w x h test terminal at time `t`; the rows as text.
    fn draw(a: &mut App, w: u16, h: u16, t: f64) -> Vec<String> {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        frame(&mut term, a, 0.05, t).unwrap();
        let buf = term.backend().buffer();
        (0..h).map(|y| (0..w).map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()).unwrap_or_default()).collect()).collect()
    }

    fn ts(offset: i64) -> String {
        iso_utc(now_i() + offset)
    }

    /// Claude at 13 % (16 % dead-reckoned), five windows, 3.4K tok/min in two sessions, credits.
    fn busy() -> Value {
        let n = now_i();
        json!({
            "provider": "claude", "plan": "MAX 20X", "source": "LIVE", "theme": "crt", "fetched": n - 30,
            "history": [[n - 3600, 9], [n - 7200, 40], [n - 100, 13], [n - 20 * 3600, 99], ["junk"], [n - 200]],
            "activity": {"tok_per_min": 3400, "idle_s": 2, "sessions": 2},
            "status": "", "hint": "",
            "windows": [
                {"label": "SESSION", "pct": 13, "resets": ts(2 * 3600 + 14 * 60 + 30)},
                {"label": "WEEK", "pct": 17.0, "resets": ts(4 * 86400 + 7 * 3600 + 30)},
                {"label": "FABLE", "pct": 30, "resets": ts(3600 + 30)},
                {"label": "COWORK", "pct": 62, "resets": null},
                {"label": "SCOPED", "pct": 90, "resets": "not a date"},
                {"pct": 5}
            ],
            "credits": {"used": 1.5, "currency": "EUR"},
            "providers": [{"name": "claude"}, {"name": "codex"}],
            "estimate": {"pct_est": 15.8, "pct_api": 13, "calibrated": true, "k": 0.001, "samples": 2, "tokens_since": 2750}
        })
    }

    /// The row the big digits start on: centred in the body, with their reset line when there is room.
    fn digits_top(l: &Layout) -> usize {
        (l.body_y + (l.body_h - if l.reset_in_body { 6 } else { 5 }) / 2) as usize
    }

    fn big_rows(big: &str) -> Vec<String> {
        (0..5).map(|r| big.chars().map(|c| glyph(c)[r].replace('#', "█")).collect::<Vec<_>>().join(" ")).collect()
    }

    #[test]
    fn busy_screen_at_every_size() {
        let mut a = app(busy(), None, None);
        assert_eq!((a.alive, a.session_pct, a.others.len(), a.estimating(), a.busy()), (true, 16, 4, true, true));
        assert_eq!(a.session.as_ref().unwrap().label, "SESSION");
        assert_eq!(a.activity_label().as_deref(), Some("3.4K TOK/MIN · 2 SESSIONS"));
        assert!(a.fresh());
        assert!((a.bpm() - (60.0 + 60.0 * 35.0f64.log10())).abs() < 1e-9);
        assert_eq!(a.buckets(4).len(), 4);
        assert_eq!((a.buckets(12)[11], a.buckets(12)[10], a.buckets(12)[9], a.buckets(12)[0]), (13.0, 9.0, 40.0, -1.0));
        assert!(!a.apply(), "nothing new in the feed");
        // 90x28: the wide layout, everything on
        let l = layout(&mut a, 90, 28);
        assert_eq!(
            (l.big.as_str(), l.reset.as_str(), l.reset_in_body, l.n_win, l.m, l.wide, l.too_small),
            ("16%", "RESET 2H 14M · EST", true, 4, 2, true, false)
        );
        assert!(l.rule_y.is_some() && l.spark_label_y.is_some() && l.axis_y.is_some());
        let rows = draw(&mut a, 90, 28, 1001.0); // an odd second: the LIVE dot is on
        assert!(rows[0].starts_with("  PULSE LIMITS"), "{}", rows[0]);
        assert!(rows[0].trim_end().ends_with("CLAUDE · MAX 20X  ● LIVE"), "{}", rows[0]);
        assert!(rows[1].trim().chars().all(|c| c == '─') && rows[1].len() > 80);
        let text = rows.join("\n");
        assert!(text.contains("● 3.4K TOK/MIN · 2 SESSIONS"), "{text}");
        let top = digits_top(&l);
        for (r, want) in big_rows("16%").iter().enumerate() {
            assert!(rows[top + r].trim_end().ends_with(want.trim_end()), "digit row {r}: {:?}", rows[top + r]);
        }
        assert!(rows[top + 5].trim_end().ends_with("RESET 2H 14M · EST"), "{}", rows[top + 5]);
        assert!(rows[l.bar_y as usize].contains("█") && rows[l.bar_y as usize].contains("░"));
        assert!((text.contains("WEEK ") || text.contains("7D ")) && text.contains(" 17%  RESET 4D 07H"), "{text}");
        assert!(text.contains("FABLE") && text.contains(" 30%  RESET 1H 00M"), "{text}");
        assert!(text.contains("COWORK") && text.contains(" 62%  RESET ?"), "{text}");
        assert!(text.contains("SCOPED") && text.contains(" 90%  RESET ?"), "{text}");
        assert!(text.contains("SESSION · 12H"), "{text}");
        let axis = &rows[l.axis_y.unwrap() as usize];
        assert!(axis.trim_start().starts_with("-12H") && axis.trim_end().ends_with("NOW"), "{axis}");
        let baseline = &rows[(l.spark_y + l.spark_h - 1) as usize];
        assert!(baseline.contains('·') && baseline.chars().any(|c| "▁▂▃▄▅▆▇█".contains(c)), "{baseline}");
        assert!(rows[27].starts_with("  UPDATED 3") && rows[27].contains("S AGO · NEXT RESET "), "{}", rows[27]);
        assert!(rows[27].trim_end().ends_with("CREDITS 1.50 EUR") && !rows[27].contains("q quit"), "no room for the keys next to the credits: {}", rows[27]);
        let rows = draw(&mut a, 100, 28, 1001.0);
        assert!(rows[27].trim_end().ends_with("CREDITS 1.50 EUR   q quit  t theme  r reload  ? help"), "{}", rows[27]);
        // an even second: the dot is off
        let rows = draw(&mut a, 90, 28, 1000.0);
        assert!(rows[0].trim_end().ends_with("MAX 20X    LIVE"), "{}", rows[0]);
        // 60x18: still wide, the reset moves next to the bar, no gaps
        let l = layout(&mut a, 60, 18);
        assert_eq!((l.m, l.wide, l.reset_in_body, l.n_win, l.body_h, l.spark_h), (1, true, false, 4, 5, 1));
        let rows = draw(&mut a, 60, 18, 1001.0);
        let text = rows.join("\n");
        assert!(rows[0].starts_with(" PULSE LIMITS"), "{}", rows[0]);
        assert!(rows[l.bar_y as usize].trim_end().ends_with("RESET 2H 14M · EST"), "{}", rows[l.bar_y as usize]);
        assert!(text.contains("SESSION") && text.contains(" 17%  RESET 4D 07H"), "{text}");
        assert!(rows[17].contains("CREDITS 1.50 EUR") && !rows[17].contains("q quit"), "no room for the keys at 60 columns: {}", rows[17]);
        // 40x12: narrow, two windows, the session count and the keys dropped
        let l = layout(&mut a, 40, 12);
        assert_eq!((l.wide, l.n_win, l.reset.as_str(), l.rule_y, l.axis_y), (false, 2, "2H 14M·EST", None, None));
        let rows = draw(&mut a, 40, 12, 1001.0);
        let text = rows.join("\n");
        assert!(text.contains("● 3.4K TOK/MIN") && !text.contains("SESSIONS"), "{text}");
        assert!(text.contains(" 17%  4D 07H"), "{text}");
        assert!(!text.contains("SCOPED") && !text.contains("q quit"), "{text}");
        assert!(rows[11].starts_with(" UPDATED 3"), "{}", rows[11]);
        // 30x10: the smallest screen that still draws; 20x5: TOO SMALL
        let rows = draw(&mut a, 30, 10, 1001.0);
        assert!(rows[0].starts_with(" PULSE LIMITS"), "{}", rows[0]);
        assert!(!layout(&mut a, 30, 10).too_small);
        let rows = draw(&mut a, 20, 5, 1001.0);
        assert_eq!(rows[2].trim(), "TOO SMALL");
        assert!(rows.iter().all(|r| !r.contains("PULSE")));
        assert!(layout(&mut a, 29, 20).too_small && layout(&mut a, 40, 9).too_small);
    }

    #[test]
    fn idle_stale_themes_help_and_keys() {
        let n = now_i();
        let mut p = busy();
        p["fetched"] = json!(n - 600);
        p["activity"] = json!({"tok_per_min": 0, "idle_s": 754, "sessions": 0});
        p["credits"] = Value::Null;
        p["estimate"] = json!({"pct_est": 13.0, "pct_api": 13, "calibrated": false});
        p["providers"] = json!([{"name": "claude"}]);
        let mut a = app(p, None, None);
        a.truecolor = false;
        assert_eq!((a.session_pct, a.estimating(), a.busy(), a.bpm(), a.fresh()), (13, false, false, 0.0, false));
        assert_eq!(a.activity_label().as_deref(), Some("IDLE 12M"));
        let rows = draw(&mut a, 90, 28, 1001.0);
        assert!(rows[0].trim_end().ends_with("MAX 20X") && !rows[0].contains("LIVE") && !rows[0].contains("CLAUDE ·"), "{}", rows[0]);
        let text = rows.join("\n");
        assert!(text.contains("● IDLE 12M"), "{text}");
        assert!(rows[27].starts_with("  UPDATED 10M AGO · NEXT RESET ") && !text.contains("CREDITS"), "{}", rows[27]);
        assert!(text.contains("RESET 2H 14M\n") || rows.iter().any(|r| r.trim_end().ends_with("RESET 2H 14M")), "no EST marker: {text}");
        // t cycles the five themes (ANSI and truecolor), ? opens the help, r pokes the feed, q and ctrl-c quit
        for (i, name) in THEMES.iter().enumerate().skip(1).chain(std::iter::once((0, &THEMES[0]))) {
            assert!(!key(&mut a, press('t')));
            assert_eq!((a.theme, THEMES[a.theme]), (i, *name));
            for truecolor in [false, true] {
                a.truecolor = truecolor;
                let rows = draw(&mut a, 60, 18, 1000.0);
                assert!(rows[0].starts_with(" PULSE LIMITS"), "{name}: {}", rows[0]);
            }
        }
        assert!(!key(&mut a, press('T')));
        assert_eq!(a.theme, 1);
        assert!(!key(&mut a, press('?')));
        assert!(a.help);
        let rows = draw(&mut a, 90, 28, 1000.0);
        let text = rows.join("\n");
        assert!(text.contains(" PULSE LIMITS ") && text.contains("q   quit") && text.contains("t   next theme (now modern)"), "{text}");
        assert!(text.contains("r   re-read the payload now") && text.contains("?   close this help"), "{text}");
        assert!(
            text.contains("payload   /nowhere/panel.url every 5s") && text.contains("built in process every 120s (once now if that file is stale)"),
            "{text}"
        );
        assert!(text.contains("│ activity  "), "{text}");
        assert!(!key(&mut a, press('?')));
        assert!(!a.help);
        assert!(!key(&mut a, press('R')));
        assert!(a.feed.poke.load(Ordering::Relaxed));
        assert!(!key(&mut a, press('x')));
        assert!(!key(&mut a, KeyEvent::new_with_kind(KeyCode::Char('q'), KeyModifiers::NONE, KeyEventKind::Release)));
        assert!(key(&mut a, press('q')) && key(&mut a, press('Q')));
        assert!(key(&mut a, KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)));
        assert!(!key(&mut a, press('c')));
        // a named provider: the help says so
        let mut a = app(busy(), None, Some("codex"));
        a.help = true;
        let text = draw(&mut a, 90, 28, 1000.0).join("\n");
        assert!(text.contains("payload   built in process every 120s, for codex (panel.url left alone)"), "{text}");
    }

    #[test]
    fn no_login_no_provider_and_a_weekly_pool() {
        let n = now_i();
        let hint = "NO CLAUDE.AI LOGIN FOUND. RUN: pulse-limits doctor";
        let p = json!({"provider": "claude", "plan": "", "source": "", "theme": "crt", "fetched": 0, "history": [],
            "activity": {"tok_per_min": 0, "idle_s": 5, "sessions": 0}, "status": "NO LOGIN", "hint": hint,
            "windows": [], "credits": null, "providers": [{"name": "claude"}]});
        let mut a = app(p, None, None);
        assert!(!a.alive && a.session.is_none() && a.bpm() == 0.0 && !a.fresh());
        assert_eq!(a.next_reset(), "?");
        assert_eq!(a.status_info(1.0), ("? NO LOGIN".to_string(), "bad"));
        let l = layout(&mut a, 60, 18);
        assert_eq!((l.big.as_str(), l.reset.as_str(), l.n_win), ("--", "", 0));
        let rows = draw(&mut a, 60, 18, 1000.0); // (t * 2) even: NO SIGNAL is on
        let text = rows.join("\n");
        assert!(rows[0].trim_end().ends_with("? NO LOGIN"), "{}", rows[0]);
        assert!(text.contains("NO SIGNAL") && text.contains("? NO LOGIN"), "{text}");
        assert!(text.contains(hint), "the hint is wider than the trace and spans the screen: {text}");
        let top = digits_top(&l);
        for (r, want) in big_rows("--").iter().enumerate() {
            assert!(rows[top + r].trim_end().ends_with(want.trim_end()), "digit row {r}: {:?}", rows[top + r]);
        }
        assert!(rows[l.bar_y as usize].contains("░") && !rows[l.bar_y as usize].contains("█"));
        assert!(rows[17].trim_start().starts_with(hint), "{}", rows[17]);
        let text = draw(&mut a, 60, 18, 1000.5).join("\n");
        assert!(!text.contains("NO SIGNAL") && text.contains("? NO LOGIN"), "{text}");
        // no provider at all
        let p = json!({"provider": "", "plan": "", "status": "NO PROVIDER SELECTED", "hint": "", "windows": [], "providers": []});
        let mut a = app(p, None, None);
        let text = draw(&mut a, 90, 28, 1000.0).join("\n");
        assert!(text.contains("? NO PROVIDER SELECTED"), "{text}");
        // a weekly pool only, from a second provider: its first window is the big number, no EST
        let p = json!({"provider": "grok", "plan": "X PREMIUM+", "fetched": n - 10, "status": "", "hint": "", "history": [],
            "windows": [{"label": "WEEK", "pct": 37.5, "resets": ts(3 * 86400 + 30)}],
            "providers": [{"name": "claude"}, {"name": "grok"}], "activity": {"tok_per_min": 0, "idle_s": 0, "sessions": 0}});
        let mut a = app(p, Some(json!({"tok_per_min": 120, "idle_s": 0, "sessions": 1})), None);
        assert_eq!((a.session.as_ref().map(|w| w.label.as_str()), a.session_pct, a.others.len()), (Some("WEEK"), 38, 0));
        assert_eq!(a.activity_label().as_deref(), Some("120 TOK/MIN"), "the feed's activity beats the payload's");
        let rows = draw(&mut a, 90, 28, 1001.0);
        assert!(rows[0].trim_end().ends_with("GROK · X PREMIUM+  ● LIVE"), "{}", rows[0]);
        let text = rows.join("\n");
        assert!(text.contains("RESET 3D 00H") && !text.contains("EST"), "{text}");
        // a failed refresh over a cached reading: the warning and the last-good age, blinking
        let mut p = busy();
        p["status"] = json!("TOKEN EXPIRED");
        p["hint"] = json!("OPEN CLAUDE CODE ONCE, IT REFRESHES THE TOKEN");
        p["fetched"] = json!(n - 300);
        let mut a = app(p, None, None);
        assert_eq!(a.status_info(1.0).1, "warn");
        for t in [1000.0, 1000.5] {
            let rows = draw(&mut a, 90, 28, t);
            assert!(rows[0].trim_end().ends_with("○ TOKEN EXPIRED"), "{}", rows[0]);
            assert!(rows[27].starts_with("  ? TOKEN EXPIRED · LAST GOOD 5M AGO"), "{}", rows[27]);
        }
        // a payload that is not an object changes nothing; a missing activity reading is fine
        let mut a = App::new(feed(Some(json!([1, 2])), None, None), 4);
        assert!(a.apply());
        assert_eq!((a.status().as_str(), a.alive, a.act, a.theme), ("NO DATA", false, None, 4));
        assert_eq!(a.activity_label(), None);
        assert_eq!(a.bpm(), 0.0);
        let mut a = App::new(feed(Some(json!({"windows": [{"label": "SESSION", "pct": 1}], "status": ""})), None, None), 0);
        assert!(a.apply());
        assert_eq!(a.bpm(), 60.0, "no reading yet: a resting pulse");
    }

    /// Claude active with its three windows, grok beside it (and codex when asked), every provider carrying an
    /// activity reading of its own, as the payload will once it measures each of them.
    fn several(codex: bool, grok_activity: Value) -> Value {
        let claude_windows = json!([
            {"label": "SESSION", "pct": 13, "resets": ts(2 * 3600 + 14 * 60 + 30)},
            {"label": "WEEK", "pct": 17.0, "resets": ts(4 * 86400 + 7 * 3600 + 30)},
            {"label": "FABLE", "pct": 30, "resets": ts(3600 + 30)}
        ]);
        let mut providers = vec![json!({"name": "claude", "plan": "MAX 20X", "status": "", "windows": claude_windows.clone(),
            "activity": {"tok_per_min": 3400, "idle_s": 2, "sessions": 2}})];
        if codex {
            providers.push(json!({"name": "codex", "plan": "PLUS", "status": "", "activity": {"tok_per_min": 0, "idle_s": 754, "sessions": 0},
                "windows": [{"label": "SESSION", "pct": 64, "resets": ts(5400)}, {"label": "GPT-5", "pct": 41, "resets": ts(2 * 86400)}]}));
        }
        providers.push(json!({"name": "grok", "plan": "X PREMIUM+", "status": "", "activity": grok_activity,
            "windows": [{"label": "WEEK", "pct": 37.5, "resets": ts(3 * 86400 + 30)}]}));
        let mut p = busy();
        p["windows"] = claude_windows;
        p["credits"] = Value::Null;
        p["providers"] = Value::Array(providers);
        p
    }

    fn labels(a: &App) -> Vec<String> {
        a.others.iter().map(|w| w.label.clone()).collect()
    }

    /// The labels of the bars on screen: the label column of each window row.
    fn bars(rows: &[String], l: &Layout) -> Vec<String> {
        (0..l.n_win)
            .map(|i| rows[(l.win_y + i as u16) as usize].chars().skip(l.m as usize).take(l.label_w as usize).collect::<String>().trim_end().to_string())
            .collect()
    }

    /// Row `y` without its trailing blanks: what the right column ends with.
    fn right(rows: &[String], y: u16) -> String {
        rows[y as usize].trim_end().to_string()
    }

    fn braille(row: &str) -> bool {
        row.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c))
    }

    #[test]
    fn several_providers_name_every_window() {
        let grok = json!({"tok_per_min": 610, "idle_s": 0, "sessions": 1});
        let a = app(several(true, grok.clone()), None, None);
        assert!(a.multi);
        // codex and grok have lanes: their headline windows (CODEX SESSION, GROK 7D) are drawn beside their traces, not as bars
        assert_eq!(labels(&a), ["CLAUDE 7D", "FABLE", "GPT-5"]);
        assert_eq!(a.session_label(), "CLAUDE SESSION");
        assert_eq!(a.session.as_ref().unwrap().label, "SESSION", "the window itself keeps its name: the EST marker still finds it");
        assert_eq!(a.reset_text(a.session.as_ref().unwrap(), true), "RESET 2H 14M · EST");
        // grok active on the same monitor: its weekly pool is the big number, GROK 7D; claude's windows join the bars
        let mut p = several(false, Value::Null);
        p["provider"] = json!("grok");
        p["windows"] = json!([{"label": "WEEK", "pct": 37.5, "resets": ts(3 * 86400 + 30)}]);
        p["estimate"] = Value::Null;
        let a = app(p, None, None);
        assert_eq!((a.session_label().as_str(), a.session_pct), ("GROK 7D", 38));
        assert_eq!(labels(&a), ["CLAUDE SESSION", "CLAUDE 7D", "FABLE"]);
        // no reading at all with two providers on: the caption still says whose the missing session is
        let mut p = several(false, Value::Null);
        p["windows"] = json!([]);
        let a = app(p, None, None);
        assert_eq!((a.alive, a.session_label().as_str()), (false, "CLAUDE SESSION"));
        // one provider: bare names, as before
        let mut p = several(false, Value::Null);
        p["providers"] = json!([{"name": "claude"}]);
        let a = app(p, None, None);
        assert!(!a.multi);
        assert_eq!((a.session_label().as_str(), labels(&a)), ("SESSION", vec!["WEEK".to_string(), "FABLE".to_string()]));
        // drawn: the caption at the right of the label row, the names in the bars, at every size; grok's lane keeps GROK 7D out of the bars
        let mut a = app(several(false, grok), None, None);
        for (w, h) in [(90u16, 28u16), (60, 18), (40, 12)] {
            let l = layout(&mut a, w, h);
            let rows = draw(&mut a, w, h, 1001.0);
            assert!(rows[l.label_y as usize].trim_end().ends_with("CLAUDE SESSION"), "{w}x{h}: {}", rows[l.label_y as usize]);
            let text = rows.join("\n");
            assert!(text.contains("CLAUDE 7D ") && text.contains(" 17%  ") && text.contains("FABLE"), "{w}x{h}: {text}");
            assert_eq!(bars(&rows, &l), ["CLAUDE 7D", "FABLE"], "{w}x{h}: {text}");
            assert!(text.contains("GROK 7D") && text.contains("38%"), "{w}x{h}: {text}");
        }
        // narrow: the right column widens to the caption, the label column to the longest name
        let l = layout(&mut a, 40, 12);
        assert_eq!((l.right_w, l.label_w, l.trace_w, l.n_win), (14, 9, 22, 2));
    }

    #[test]
    fn a_lane_per_provider_with_its_own_activity() {
        let grok = json!({"tok_per_min": 610, "idle_s": 0, "sessions": 1});
        let mut a = app(several(false, grok.clone()), None, None);
        assert_eq!(a.lanes.iter().map(|l| (l.name.as_str(), l.active, l.pct)).collect::<Vec<_>>(), [("CLAUDE", true, Some(13.0)), ("GROK", false, Some(37.5))]);
        assert_eq!((a.lanes[0].act, a.lanes[1].act), (a.act, Some((610.0, 0.0, 1))), "the active provider's lane follows the feed");
        assert!((a.lanes[1].bpm(true) - (60.0 + 60.0 * 7.1f64.log10())).abs() < 1e-9);
        assert_eq!((a.lanes[0].bpm(true), a.lanes[0].bpm(false), a.lanes[1].bpm(false)), (a.bpm(), 0.0, a.lanes[1].bpm(true)));
        // the headline of each lane: the active lane's is the big number's (estimate, EST marker), grok's its weekly pool
        assert_eq!(a.headline(&a.lanes[0], true), ("CLAUDE SESSION".to_string(), "16%".to_string(), "RESET 2H 14M · EST".to_string()));
        assert_eq!(a.headline(&a.lanes[1], true), ("GROK 7D".to_string(), "38%".to_string(), "RESET 3D 00H".to_string()));
        assert_eq!(a.headline(&a.lanes[1], false), ("GROK 7D".to_string(), "38%".to_string(), "3D 00H".to_string()));
        assert_eq!(labels(&a), ["CLAUDE 7D", "FABLE"], "GROK 7D is a headline, not a bar");
        // the label row and the body split into two lanes at every size (at 90x28 a row taller than with three bars: GROK 7D
        // left them); both names and rates on screen, a trace in each, and each lane's headline level with it on the right
        for (w, h, want, reset) in [
            (90u16, 28u16, [(2u16, 3u16, 6u16), (9, 10, 5)], ["RESET 2H 14M · EST", "RESET 3D 00H"]),
            (60, 18, [(2, 3, 2), (5, 6, 2)], ["RESET 2H 14M · EST", "RESET 3D 00H"]),
            (40, 12, [(1, 2, 2), (4, 5, 2)], ["2H 14M·EST", "3D 00H"]),
        ] {
            let l = layout(&mut a, w, h);
            assert_eq!(l.lanes, want, "{w}x{h}");
            assert_eq!((l.lanes[0].0, l.lanes[1].1 + l.lanes[1].2), (l.label_y, l.body_y + l.body_h), "{w}x{h}: the lanes fill the block");
            let rows = draw(&mut a, w, h, 1001.0);
            let (m, top) = (l.m as usize, want[0].0 as usize);
            assert!(rows[top][m..].starts_with("● CLAUDE  3.4K TOK/MIN"), "{w}x{h}: {}", rows[top]);
            assert_eq!(rows[top].contains("2 SESSIONS"), w >= 60, "{w}x{h}: the session count goes first when narrow: {}", rows[top]);
            assert!(rows[top].trim_end().ends_with("CLAUDE SESSION"), "{w}x{h}: {}", rows[top]);
            assert!(rows[want[1].0 as usize][m..].starts_with("● GROK  610 TOK/MIN"), "{w}x{h}: {}", rows[want[1].0 as usize]);
            for &(_, by, bh) in &l.lanes {
                assert!((by..by + bh).any(|y| braille(&rows[y as usize])), "{w}x{h}: no trace in rows {by}..{}", by + bh);
            }
            // each headline: the label on the name row, the percentage on the first trace row, the reset on the second
            let text = rows.join("\n");
            for (i, (label, pct)) in [("CLAUDE SESSION", "16%"), ("GROK 7D", "38%")].iter().enumerate() {
                let (ly, by, _) = want[i];
                assert!(right(&rows, ly).ends_with(label) && right(&rows, by).ends_with(pct) && right(&rows, by + 1).ends_with(reset[i]), "{w}x{h}:\n{text}");
            }
            // the headline windows are not bars too, and the bar under the block spans the width with no reset beside it
            assert_eq!(bars(&rows, &l), ["CLAUDE 7D", "FABLE"], "{w}x{h}:\n{text}");
            let bar = &rows[l.bar_y as usize];
            assert!(bar.chars().skip(m).take(w as usize - 2 * m).all(|c| c == '█' || c == '░') && bar.contains('█'), "{w}x{h}: {bar}");
        }
        // the active provider without a reading: its lane's headline says NO SIGNAL, the other's stands, the bar is empty
        let mut p = several(false, grok.clone());
        p["windows"] = json!([]);
        p["status"] = json!("NO LOGIN");
        let mut dead = app(p, None, None);
        assert_eq!(dead.headline(&dead.lanes[0], true), ("CLAUDE SESSION".to_string(), "--".to_string(), "NO SIGNAL".to_string()));
        let l = layout(&mut dead, 90, 28);
        assert_eq!((l.n_win, l.lanes.len()), (0, 2));
        let rows = draw(&mut dead, 90, 28, 1000.0);
        let (ly, by, _) = l.lanes[0];
        assert!(
            right(&rows, ly).ends_with("CLAUDE SESSION") && right(&rows, by).ends_with("--") && right(&rows, by + 1).ends_with("NO SIGNAL"),
            "{}",
            rows.join("\n")
        );
        assert!(right(&rows, l.lanes[1].0).ends_with("GROK 7D") && right(&rows, l.lanes[1].1).ends_with("38%"), "{}", rows.join("\n"));
        assert!(rows[l.bar_y as usize].contains('░') && !rows[l.bar_y as usize].contains('█'), "{}", rows[l.bar_y as usize]);
        // the traces keep running across a payload refresh: the same lanes come back with their columns
        for _ in 0..3 {
            draw(&mut a, 90, 28, 1001.0);
        }
        let head = a.lanes[0].ecg.head;
        assert!(head > 0);
        a.feed.shared.lock().unwrap().version += 1;
        assert!(a.apply());
        assert_eq!((a.lanes.len(), a.lanes[0].ecg.head), (2, head));
        // grok without a reading, or without the key at all: the single trace and its label, the bar still there
        let mut p = several(false, Value::Null);
        let mut a = app(p.clone(), None, None);
        assert!(a.lanes.is_empty() && layout(&mut a, 90, 28).lanes.is_empty());
        let text = draw(&mut a, 90, 28, 1001.0).join("\n");
        assert!(text.contains("● 3.4K TOK/MIN · 2 SESSIONS") && !text.contains("● CLAUDE") && !text.contains("● GROK"), "{text}");
        assert!(text.contains("GROK 7D "), "{text}");
        for i in 0..2 {
            p["providers"][i].as_object_mut().unwrap().remove("activity");
        }
        assert!(app(p, None, None).lanes.is_empty(), "no activity key at all: the payload of today");
        assert_eq!(act_of(Some(&json!({"tok_per_min": "x"}))), None);
        // three providers: 4 + 4 + 3 of the 11 rows; a provider with a reading but no windows draws in the dim colour
        let mut p = several(true, grok);
        p["providers"][2]["windows"] = json!([]);
        let mut a = app(p.clone(), None, None);
        assert_eq!(a.lanes.iter().map(|l| (l.name.as_str(), l.pct)).collect::<Vec<_>>(), [("CLAUDE", Some(13.0)), ("CODEX", Some(64.0)), ("GROK", None)]);
        assert_eq!((a.lanes[1].bpm(true), busy_of(a.lanes[1].act), label_of(a.lanes[1].act).as_deref()), (0.0, false, Some("IDLE 12M")));
        let l = layout(&mut a, 90, 28);
        assert_eq!((l.lanes.to_vec(), l.body_h), (vec![(2, 3, 3), (6, 7, 3), (10, 11, 3)], 11));
        let rows = draw(&mut a, 90, 28, 1001.0);
        let text = rows.join("\n");
        assert!(text.contains("● CLAUDE  3.4K TOK/MIN · 2 SESSIONS") && text.contains("● CODEX  IDLE 12M") && text.contains("● GROK  610 TOK/MIN"), "{text}");
        // three headlines: codex's session (out of the bars), and grok's says why it has no window
        assert!(right(&rows, 6).ends_with("CODEX SESSION") && right(&rows, 7).ends_with("64%") && right(&rows, 8).contains("RESET 1H "), "{text}");
        assert!(right(&rows, 10).ends_with("GROK SESSION") && right(&rows, 11).ends_with("--") && right(&rows, 12).ends_with("NO DATA"), "{text}");
        assert_eq!(bars(&rows, &l), ["CLAUDE 7D", "FABLE", "GPT-5"], "{text}");
        // 30x10: six rows, two per lane; the rates no longer fit, the names do; the headlines drop their reset line
        let l = layout(&mut a, 30, 10);
        assert_eq!((l.lanes.to_vec(), l.trace_w), (vec![(1, 2, 1), (3, 4, 1), (5, 6, 1)], 12));
        let rows = draw(&mut a, 30, 10, 1001.0);
        let text = rows.join("\n");
        assert!(rows[1].starts_with(" ● CLAUDE  ") && rows[3].starts_with(" ● CODEX  ") && rows[5].starts_with(" ● GROK  "), "{text}");
        assert!(!text.contains("TOK") && rows[1].trim_end().ends_with("CLAUDE SESSION"), "{text}");
        assert!(right(&rows, 2).ends_with("16%") && right(&rows, 3).ends_with("CODEX SESSION") && right(&rows, 4).ends_with("64%"), "{text}");
        assert!(right(&rows, 5).ends_with("GROK SESSION") && right(&rows, 6).ends_with("--"), "{text}");
        assert!(!text.contains("2H 14M") && !text.contains("1H 30M") && !text.contains("NO DATA"), "the reset lines went first: {text}");
        // a fourth lane needs an eighth row: on six it waits for a taller terminal
        p["providers"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name": "gemini", "activity": {"tok_per_min": 5, "idle_s": 0, "sessions": 1}, "windows": []}));
        let mut a = app(p, None, None);
        assert_eq!(a.lanes.len(), 4);
        assert_eq!(layout(&mut a, 30, 10).lanes.len(), 3);
        let text = draw(&mut a, 30, 10, 1001.0).join("\n");
        assert!(text.contains("● GROK") && !text.contains("GEMINI"), "{text}");
        assert_eq!(layout(&mut a, 90, 28).lanes, [(2, 3, 2), (5, 6, 2), (8, 9, 2), (11, 12, 2)]);
        let text = draw(&mut a, 90, 28, 1001.0).join("\n");
        assert!(text.contains("GEMINI SESSION") && text.contains("NO DATA"), "a fourth lane with no windows says why: {text}");
    }

    #[test]
    fn the_trace_and_its_colours() {
        let mut e = Ecg::default();
        e.step(1.0, 60.0); // no columns yet: nothing to do
        e.init(40);
        assert_eq!(e.cols.len(), 40);
        e.init(40);
        let mut beat = false;
        for _ in 0..200 {
            e.step(0.02, 120.0);
            beat |= e.beat > 0.0;
        }
        assert!(beat, "the R spike fired");
        assert!(e.cols.iter().any(|c| *c > 0.5), "{:?}", e.cols);
        assert!(Ecg::shape(0.335) > 0.9 && Ecg::shape(0.0).abs() < 0.01);
        e.step(5.0, 0.0);
        assert!(e.cols.iter().all(|c| *c == 0.0), "a flat line at 0 bpm");
        e.init(20);
        assert_eq!((e.cols.len(), e.head), (20, 0));
        use Color::*;
        assert_eq!(faded(Rgb(100, 100, 100), 0.5, true), Rgb(50, 50, 50));
        assert_eq!(faded(LightGreen, 0.7, false), LightGreen);
        for (from, to) in [
            (LightGreen, Green),
            (LightRed, Red),
            (LightYellow, Yellow),
            (LightCyan, Cyan),
            (LightMagenta, Magenta),
            (LightBlue, Blue),
            (White, Gray),
            (Gray, DarkGray),
            (Blue, Blue),
        ] {
            assert_eq!(faded(from, 0.4, false), to);
        }
        assert_eq!(rgb(0x8fd3ff), Rgb(0x8f, 0xd3, 0xff));
        assert_eq!(glyph('x')[2], "###");
    }

    #[test]
    fn the_loop_draws_frames_until_q() {
        let mut a = app(busy(), None, None);
        let mut term = Terminal::new(TestBackend::new(30, 10)).unwrap();
        let mut script: VecDeque<Option<Event>> =
            [None, Some(Event::Key(press('t'))), Some(Event::Resize(30, 10)), None, Some(Event::Key(press('?'))), Some(Event::Key(press('r'))), None].into();
        let mut polls = 0;
        run_loop(&mut term, &mut a, |wait| {
            polls += 1;
            thread::sleep(wait.min(Duration::from_millis(60)));
            Ok(script.pop_front().unwrap_or(Some(Event::Key(press('q')))))
        })
        .unwrap();
        assert_eq!((polls, a.theme, a.help, a.feed.poke.load(Ordering::Relaxed)), (8, 1, true, true));
        assert!(!a.ecg.cols.is_empty() && a.seen == 1);
        let buf = term.backend().buffer();
        let first: String = (0..30).map(|x| buf.cell((x, 0)).map(|c| c.symbol().to_string()).unwrap_or_default()).collect();
        assert!(first.starts_with(" PULSE LIMITS"), "{first}");
    }

    #[test]
    fn the_feed_reads_the_panel_url_or_builds_the_payload() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Sandbox::new("tui-feed");
        let url = s.scratch.cache().join("panel.url");
        assert!(age_of(&url).is_none());
        // a fresh panel.url a bar wrote: read (once per mtime), and no build of our own
        let b = payload::assemble(vec![], vec![], "", "crt", json!({"tok_per_min": 0, "idle_s": 1, "sessions": 0}));
        write_atomic(&url, b.panel_url(&s.scratch.0.join("lib")).as_bytes()).unwrap();
        assert!(age_of(&url).unwrap() < Duration::from_secs(5));
        let shared = Arc::new(Mutex::new(Shared::default()));
        let poke = Arc::new(AtomicBool::new(false));
        let mut p = Poller::new(shared.clone(), poke.clone(), url.clone(), None);
        assert!(!p.stale());
        p.tick();
        {
            let sh = shared.lock().unwrap();
            assert_eq!((sh.version, sh.payload.as_ref().unwrap()["status"].as_str()), (2, Some("NO PROVIDER SELECTED")));
            assert_eq!(sh.activity.as_ref().unwrap()["sessions"], 0);
        }
        p.tick();
        assert_eq!(shared.lock().unwrap().version, 2, "nothing moved: nothing published");
        poke.store(true, Ordering::Relaxed);
        p.tick();
        assert_eq!(shared.lock().unwrap().version, 3, "r re-reads the file; it is fresh, so no build");
        // a named provider: no panel.url, a build now (grok has no login in the sandbox), the file left alone
        let before = std::fs::read_to_string(&url).unwrap();
        let mut p = Poller::new(shared.clone(), poke.clone(), url.clone(), Some("grok".into()));
        assert!(p.stale());
        p.tick();
        {
            let sh = shared.lock().unwrap();
            let v = sh.payload.as_ref().unwrap();
            assert_eq!((v["provider"].as_str(), v["status"].as_str()), (Some("grok"), Some("NO LOGIN")));
        }
        assert_eq!(std::fs::read_to_string(&url).unwrap(), before);
        // no panel.url at all: stale, so the first tick builds and writes it
        std::fs::remove_file(&url).unwrap();
        let mut p = Poller::new(shared.clone(), poke, url.clone(), None);
        assert!(p.stale());
        p.tick();
        assert!(url.is_file());
        assert_eq!(shared.lock().unwrap().payload.as_ref().unwrap()["status"], "NO PROVIDER SELECTED");
        // what counts as a panel.url
        assert!(read_panel_url(Path::new("/nowhere/panel.url")).is_none());
        for text in ["no hash at all", "file:///x.html#!!!not base64!!!", &format!("file:///x.html#{}", crate::util::base64_encode(b"{\"a\":1}"))] {
            write_atomic(&url, text.as_bytes()).unwrap();
            assert!(read_panel_url(&url).is_none(), "{text}");
        }
        write_atomic(&url, format!("file:///x.html#{}\n", crate::util::base64_encode(b"{\"windows\":[]}")).as_bytes()).unwrap();
        assert_eq!(read_panel_url(&url), Some(json!({"windows": []})));
        // the saved theme and the terminal's colour depth
        assert_eq!(saved_theme(), 0);
        write_atomic(&config_dir().join("theme"), b"cyber\n").unwrap();
        assert_eq!(saved_theme(), 2);
        assert!(!App::new(feed(None, None, None), 3).truecolor);
        std::env::set_var("COLORTERM", "truecolor");
        let a = App::new(feed(None, None, None), 3);
        assert!(a.truecolor && a.theme == 3 && a.status() == "NO DATA");
        std::env::set_var("COLORTERM", "24bit");
        assert!(App::new(feed(None, None, None), 0).truecolor);
        // the command line
        let args = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(parse(&args(&[])), Ok((None, None)));
        assert_eq!(parse(&args(&["--theme=cyber", "codex"])), Ok((Some("codex".into()), Some(2))));
        assert_eq!(parse(&args(&["--theme", "synth"])), Ok((None, Some(3))));
        for bad in [vec!["--theme", "neon"], vec!["--theme"], vec!["--theme=neon"], vec!["--bogus"], vec!["gemini"], vec!["claude", "-x"]] {
            assert_eq!(parse(&args(&bad)), Err(64), "{bad:?}");
        }
        assert_eq!(parse(&args(&["--help"])), Err(0));
        assert_eq!(parse(&args(&["-h"])), Err(0));
        assert_eq!(run(&args(&["--bogus"])), 64);
        assert!(usage().starts_with("usage: pulse-limits tui [grok|claude|codex] [--theme crt|modern|cyber|synth|analog]"));
        drop(s);
    }
}
