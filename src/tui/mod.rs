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

struct App {
    feed: Feed,
    ecg: Ecg,
    d: Map<String, Value>, // the payload, merged like Object.assign in the panel
    session: Option<Win>,
    others: Vec<Win>,
    session_pct: i64,
    alive: bool,
    act: Option<(f64, f64, i64)>, // tokens/min, idle seconds, sessions
    est: Option<(f64, f64)>,      // dead-reckoned %, API %
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
        let windows: Vec<Win> = self
            .d
            .get("windows")
            .and_then(Value::as_array)
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
            .unwrap_or_default();
        // SESSION is the big number; a provider with no session window (a weekly pool only) shows its first one there
        let session_i = windows.iter().position(|w| w.label == "SESSION").or(if windows.is_empty() { None } else { Some(0) });
        self.session = session_i.map(|i| windows[i].clone());
        self.others = windows.iter().enumerate().filter(|(i, _)| Some(*i) != session_i).map(|(_, w)| w.clone()).collect();
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
        self.act = act.and_then(|a| {
            Some((
                a.get("tok_per_min")?.as_f64()?,
                a.get("idle_s").and_then(Value::as_f64).unwrap_or(0.0),
                a.get("sessions").and_then(Value::as_i64).unwrap_or(0),
            ))
        });
        true
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
            Some((tok, _, _)) if tok > 0.0 => (60.0 + 60.0 * (1.0 + tok / 100.0).log10()).min(180.0),
            Some(_) => 0.0,
        }
    }

    fn busy(&self) -> bool {
        matches!(self.act, Some((tok, _, _)) if tok > 0.0)
    }

    fn activity_label(&self) -> Option<String> {
        let (tok, idle, sessions) = self.act?;
        Some(if tok > 0.0 {
            format!("{} TOK/MIN{}", fmt_k(tok as i64), if sessions > 1 { format!(" · {sessions} SESSIONS") } else { String::new() })
        } else {
            format!("IDLE {}", short(idle as i64))
        })
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
    // 15 = "100%": the split does not jump between two- and three-digit readings
    let right_w = dw.max(reset.chars().count() as i32).max(7).max(if wide { 15 } else { 0 });
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
    let label_w = app.others.iter().take(n_win as usize).map(|o| o.label.chars().count()).max().unwrap_or(4).clamp(4, 8);
    app.ecg.init(trace_w as usize * 2);
    Layout {
        too_small: h < MIN_ROWS || w < MIN_COLS,
        m: m as u16,
        wide,
        big,
        reset,
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
    if let Some(mut lab) = app.activity_label() {
        if lab.chars().count() + 2 > l.trace_w as usize {
            lab = lab.split(" · ").next().unwrap_or_default().to_string(); // narrow: drop the session count
        }
        let live = if app.busy() { app.color(p.ok) } else { app.color(p.dim) };
        let dot = Style::new().fg(live).add_modifier(if app.ecg.beat > 0.3 { Modifier::BOLD } else { Modifier::DIM });
        let line = Line::from(vec![Span::styled("● ", dot), Span::styled(lab, Style::new().fg(live))]);
        f.render_widget(Paragraph::new(line), rect(l.trace_x, l.label_y, l.trace_w, 1, area));
    }
    let body = rect(l.trace_x, l.body_y, l.trace_w, l.body_h, area);
    let trace = Trace { ecg: &app.ecg, rows: l.body_h as usize, color: col, truecolor: app.truecolor };
    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds([0.0, (l.trace_w as f64 * 2.0 - 1.0).max(1.0)])
        .y_bounds([0.0, (l.body_h as f64 * 4.0 - 1.0).max(1.0)])
        .paint(|ctx| ctx.draw(&trace));
    f.render_widget(canvas, body);
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
    // the session: label, big digits, reset, bar
    let right_x = w - m - l.right_w;
    text!(l.label_y, right_x, l.right_w, Line::styled("SESSION", dim), Alignment::Right);
    let top = l.body_y + (l.body_h - if l.reset_in_body { 6 } else { 5 }) / 2; // digits (and their reset line) centred in the body
    for r in 0..5 {
        let row = l.big.chars().map(|c| glyph(c)[r].replace('#', "█")).collect::<Vec<_>>().join(" ");
        text!(top + r as u16, right_x, l.right_w, Line::styled(row, bold(col)), Alignment::Right);
    }
    if app.alive {
        let ry = if l.reset_in_body { top + 5 } else { l.bar_y };
        text!(ry, right_x, l.right_w, Line::styled(l.reset.clone(), dim), Alignment::Right);
    }
    let bw = if l.reset_in_body || !app.alive { w - 2 * m } else { l.trace_w } as usize;
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
fn frame<B: Backend<Error = io::Error>>(terminal: &mut Terminal<B>, app: &mut App, dt: f64, t: f64) -> io::Result<()> {
    app.apply();
    let size = terminal.size()?;
    let l = layout(app, size.width, size.height);
    let bpm = app.bpm();
    app.ecg.step(dt, bpm);
    terminal.draw(|f| render(f, app, &l, t))?;
    Ok(())
}

/// Frames at ~20 fps until q; `next_event` waits up to the given time for a terminal event.
fn run_loop<B: Backend<Error = io::Error>>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    mut next_event: impl FnMut(Duration) -> io::Result<Option<Event>>,
) -> io::Result<()> {
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
