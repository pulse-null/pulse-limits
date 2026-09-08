//! Dead reckoning: the session % between two API readings. Each reading is an anchor (pct at
//! time). When a new reading arrives, the tokens produced between the two anchors calibrate
//! k = percent per output token (smoothed). Between readings the estimate is anchor + k *
//! tokens since the anchor, snapping back at the next reading. calib.json is shared with the
//! Swift helper that did this before: same fields, same rules.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::activity::{output_tokens, projects_dir};
use crate::util::{cache_dir, now_f, write_atomic};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Calib {
    pub anchor_pct: f64,
    pub anchor_at: i64,
    pub k: f64,
    pub samples: i64,
}

pub fn calib_file() -> PathBuf {
    cache_dir().join("calib.json")
}

pub fn load(file: &Path) -> Option<Calib> {
    serde_json::from_slice(&std::fs::read(file).ok()?).ok()
}

pub fn save(file: &Path, c: &Calib) {
    if let Ok(text) = serde_json::to_string(c) {
        let _ = write_atomic(file, text.as_bytes());
    }
}

pub struct Estimate {
    pub pct_est: f64,
    pub pct_api: f64,
    pub calibrated: bool,
    pub k: f64,
    pub samples: i64,
    pub tokens_since: i64,
}

impl Estimate {
    /// `pct_api` as the reading wrote it (13 stays 13) when the caller has it.
    pub fn to_json(&self, pct_api: Option<Value>) -> Value {
        json!({ "pct_est": self.pct_est, "pct_api": pct_api.unwrap_or(json!(self.pct_api)), "calibrated": self.calibrated,
                "k": self.k, "samples": self.samples, "tokens_since": self.tokens_since })
    }
}

/// The calibration step alone: what a reading (pct, fetched) does to the saved anchor.
pub fn update(c: &mut Calib, pct: f64, fetched: i64, tokens_between: impl Fn(i64, i64) -> i64) -> bool {
    if fetched == c.anchor_at {
        return false;
    }
    if fetched > c.anchor_at && pct >= c.anchor_pct {
        let tokens = tokens_between(c.anchor_at, fetched);
        let dpct = pct - c.anchor_pct;
        if tokens >= 1000 && dpct >= 1.0 {
            let kobs = dpct / tokens as f64;
            if kobs > 1e-8 && kobs < 1e-2 {
                c.k = if c.k > 0.0 { 0.6 * c.k + 0.4 * kobs } else { kobs };
                c.samples += 1;
            }
        }
    }
    c.anchor_pct = pct;
    c.anchor_at = fetched;
    true
}

pub fn estimate_with(file: &Path, projects: &Path, pct: f64, fetched: i64, now: f64) -> Estimate {
    let mut c = match load(file) {
        Some(c) => c,
        None => {
            let c = Calib { anchor_pct: pct, anchor_at: fetched, k: 0.0, samples: 0 };
            save(file, &c);
            c
        }
    };
    if update(&mut c, pct, fetched, |a, b| output_tokens(projects, a as f64, b as f64)) {
        save(file, &c);
    }
    let (mut est, mut since) = (pct, 0);
    if c.k > 0.0 {
        since = output_tokens(projects, fetched as f64, now);
        est = (pct + c.k * since as f64).min(100.0);
    }
    Estimate {
        pct_est: (est * 10.0).round() / 10.0,
        pct_api: pct,
        calibrated: c.k > 0.0,
        k: c.k,
        samples: c.samples,
        tokens_since: since,
    }
}

pub fn estimate(pct: f64, fetched: i64) -> Estimate {
    estimate_with(&calib_file(), &projects_dir(), pct, fetched, now_f())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::testing::{Scratch, ENV};
    use crate::util::testing::Vars;

    #[test]
    fn calib_rules() {
        let mut c = Calib { anchor_pct: 10.0, anchor_at: 1000, k: 0.0, samples: 0 };
        // same reading: nothing moves
        assert!(!update(&mut c, 10.0, 1000, |_, _| 5000));
        // too few tokens: anchor moves, no calibration
        assert!(update(&mut c, 12.0, 1300, |_, _| 999));
        assert_eq!((c.anchor_pct, c.anchor_at, c.k, c.samples), (12.0, 1300, 0.0, 0));
        // too small a rise
        update(&mut c, 12.5, 1600, |_, _| 5000);
        assert_eq!((c.k, c.samples), (0.0, 0));
        // a reset (pct fell): anchor only
        update(&mut c, 3.0, 1900, |_, _| 5000);
        assert_eq!((c.anchor_pct, c.k), (3.0, 0.0));
        // first calibration: 2 % over 4000 tokens
        update(&mut c, 5.0, 2200, |a, b| {
            assert_eq!((a, b), (1900, 2200));
            4000
        });
        assert_eq!((c.k, c.samples), (0.0005, 1));
        // smoothing: 0.6 * old + 0.4 * observed
        update(&mut c, 9.0, 2500, |_, _| 8000); // kobs = 0.0005
        assert!((c.k - 0.0005).abs() < 1e-12);
        update(&mut c, 19.0, 2800, |_, _| 10000); // kobs = 0.001
        assert!((c.k - (0.6 * 0.0005 + 0.4 * 0.001)).abs() < 1e-12);
        assert_eq!(c.samples, 3);
        // out-of-range observations are ignored (anchor still moves)
        update(&mut c, 99.0, 3100, |_, _| 1000); // kobs = 0.08 > 1e-2
        assert_eq!(c.samples, 3);
        assert_eq!(c.anchor_at, 3100);
        // an older reading than the anchor only moves the anchor
        update(&mut c, 50.0, 100, |_, _| panic!("no scan backwards"));
        assert_eq!((c.anchor_pct, c.anchor_at), (50.0, 100));
    }

    #[test]
    fn file_round_trip_and_estimate() {
        let d = std::env::temp_dir().join(format!("pl-rust-est-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("calib.json");
        // the Swift helper's key order and integer pct read fine
        std::fs::write(&f, "{\"anchorAt\":1788876097,\"k\":0.00013920772164224593,\"samples\":21,\"anchorPct\":13}").unwrap();
        assert_eq!(load(&f), Some(Calib { anchor_pct: 13.0, anchor_at: 1788876097, k: 0.00013920772164224593, samples: 21 }));
        assert_eq!(load(&d.join("none")), None);
        std::fs::write(&f, "{\"anchorAt\":true,\"k\":0,\"samples\":0,\"anchorPct\":1}").unwrap();
        assert_eq!(load(&f), None, "a bad file starts over, as the Swift side would");
        std::fs::remove_file(&f).unwrap();
        // first reading: an anchor with k = 0, no estimate
        let e = estimate_with(&f, &d, 13.0, 1788876097, 1788876200.0);
        assert_eq!((e.pct_est, e.pct_api, e.calibrated, e.k, e.samples, e.tokens_since), (13.0, 13.0, false, 0.0, 0, 0));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "{\"anchorPct\":13.0,\"anchorAt\":1788876097,\"k\":0.0,\"samples\":0}");
        assert_eq!(e.to_json(Some(json!(13))).to_string(), "{\"pct_est\":13.0,\"pct_api\":13,\"calibrated\":false,\"k\":0.0,\"samples\":0,\"tokens_since\":0}");
        // calibrated: tokens since the anchor move the estimate, capped at 100 and rounded to a tenth
        save(&f, &Calib { anchor_pct: 13.0, anchor_at: 1788876097, k: 0.001, samples: 2 });
        std::fs::create_dir_all(d.join("p")).unwrap();
        std::fs::write(
            d.join("p").join("s.jsonl"),
            "{\"type\":\"assistant\",\"timestamp\":\"2026-09-08T14:03:00Z\",\"message\":{\"id\":\"m\",\"usage\":{\"output_tokens\":2750}}}\n",
        )
        .unwrap();
        let e = estimate_with(&f, &d, 13.0, 1788876097, 1788876300.0);
        assert_eq!((e.pct_est, e.calibrated, e.tokens_since), (15.8, true, 2750)); // 13 + 2.75 = 15.75 -> 15.8
        let e = estimate_with(&f, &d, 99.0, 1788876097, 1788876300.0);
        assert_eq!(e.pct_est, 100.0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn corrupt_file_bounds_and_defaults() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let s = Scratch::new("estimate");
        let mut vars = Vars::default();
        vars.set("CLAUDE_PROJECTS_DIR", s.0.join("none"));
        assert_eq!(calib_file(), s.cache().join("calib.json"));
        // a corrupt file starts over: a fresh anchor is written
        std::fs::write(calib_file(), "{not json").unwrap();
        let e = estimate(13.0, 1788876097);
        assert_eq!((e.pct_est, e.calibrated, e.samples), (13.0, false, 0));
        assert_eq!(load(&calib_file()), Some(Calib { anchor_pct: 13.0, anchor_at: 1788876097, k: 0.0, samples: 0 }));
        // a later reading moves the anchor on disk
        let e = estimate(14.0, 1788876397);
        assert_eq!(load(&calib_file()).unwrap().anchor_at, 1788876397);
        assert_eq!(e.to_json(None).to_string(), "{\"pct_est\":14.0,\"pct_api\":14.0,\"calibrated\":false,\"k\":0.0,\"samples\":0,\"tokens_since\":0}");
        // k bounds: a slope too flat is not a sample; a drop never scans
        let mut c = Calib { anchor_pct: 10.0, anchor_at: 1000, k: 0.0, samples: 0 };
        assert!(update(&mut c, 11.0, 1300, |_, _| 1_000_000_000)); // 1e-9
        assert_eq!((c.k, c.samples), (0.0, 0));
        update(&mut c, 12.0, 1600, |_, _| 5000);
        assert_eq!(c.samples, 1);
        update(&mut c, 1.0, 1900, |_, _| panic!("a drop never scans"));
        assert_eq!((c.anchor_pct, c.anchor_at, c.samples), (1.0, 1900, 1));
    }
}
