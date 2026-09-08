#!/usr/bin/env python3
"""What Claude Code is doing right now, from the transcripts it writes locally.

A port of `pulse-popover --activity` and `pulse-popover --estimate` for hosts with
no Swift (Linux). Same numbers, same calib.json; the plugin runs whichever exists.

  pulse-activity.py --activity              {"tok_per_min": N, "idle_s": N, "sessions": N}
  pulse-activity.py --estimate PCT FETCHED  {"pct_est": .., "pct_api": .., "calibrated": .., "k": .., "samples": N, "tokens_since": N}

Env: CLAUDE_PROJECTS_DIR (transcripts, default ~/.claude/projects) and XDG_CACHE_HOME
(calib.json lives in $XDG_CACHE_HOME/pulse-limits, default ~/.cache/pulse-limits).
Standard library only.
"""
import json
import math
import os
import re
import sys
import time
from datetime import datetime, timezone

PROJECTS = os.environ.get("CLAUDE_PROJECTS_DIR") or os.path.expanduser("~/.claude/projects")
CACHE_DIR = os.path.join(os.environ.get("XDG_CACHE_HOME") or os.path.expanduser("~/.cache"), "pulse-limits")
CALIB = os.path.join(CACHE_DIR, "calib.json")

# 2026-09-08T13:40:00.075Z or ...+00:00; a zone is required, as with ISO8601DateFormatter
ISO = re.compile(r"^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.(\d+))?(Z|[+-]\d{2}:?\d{2})$")


def epoch_of(ts):
    """ISO-8601 timestamp -> epoch seconds (float), None if it does not parse."""
    m = ISO.match(ts)
    if not m:
        return None
    y, mo, d, h, mi, s, frac, tz = m.groups()
    try:
        t = datetime(int(y), int(mo), int(d), int(h), int(mi), int(s), tzinfo=timezone.utc).timestamp()
    except ValueError:
        return None
    if frac:
        t += int(frac[:9].ljust(9, "0")) / 1e9
    if tz != "Z":
        sign = 1 if tz[0] == "+" else -1
        t -= sign * (int(tz[1:3]) * 3600 + int(tz[-2:]) * 60)
    return t


def transcripts():
    """(path, mtime) of every *.jsonl under the projects dir; symlinks are not followed."""
    stack = [PROJECTS]
    while stack:
        try:
            entries = list(os.scandir(stack.pop()))
        except OSError:
            continue
        for e in entries:
            try:
                if e.is_dir(follow_symlinks=False):
                    stack.append(e.path)
                elif e.name.endswith(".jsonl") and e.is_file(follow_symlinks=False):
                    yield e.path, e.stat(follow_symlinks=False).st_mtime
            except OSError:
                continue


def output_tokens(frm, to):
    """Output tokens of assistant turns stamped in (frm, to], across every transcript touched
    since frm, deduplicated by message id (a streaming reply is written several times)."""
    per_message = {}
    # ISO minute prefixes covering the window, so big lines can be skipped before a JSON parse
    prefixes = []
    t = frm - 60
    while t <= to + 60:
        prefixes.append(datetime.fromtimestamp(t, timezone.utc).strftime("%Y-%m-%dT%H:%M"))
        t += 60
    span = to - frm
    tail = 131072 if span <= 120 else 1048576 if span <= 900 else 4194304
    for path, mtime in transcripts():
        if mtime < frm:
            continue
        try:
            with open(path, "rb") as fh:
                size = fh.seek(0, os.SEEK_END)
                start = size - tail if size > tail else 0
                fh.seek(start)
                data = fh.read()
        except OSError:
            continue
        lines = [ln for ln in data.decode("utf-8", "replace").split("\n") if ln]
        if start > 0 and lines:
            lines.pop(0)  # a partial line
        for line in lines:
            if '"type":"assistant"' not in line or "output_tokens" not in line:
                continue
            if not any(p in line for p in prefixes):
                continue
            try:
                obj = json.loads(line)
            except ValueError:
                continue
            ts = obj.get("timestamp") if isinstance(obj, dict) else None
            when = epoch_of(ts) if isinstance(ts, str) else None
            if when is None or not (frm < when <= to):
                continue
            msg = obj.get("message")
            if not isinstance(msg, dict):
                continue
            mid, usage = msg.get("id"), msg.get("usage")
            if not isinstance(mid, str) or not isinstance(usage, dict):
                continue
            out = usage.get("output_tokens")
            if isinstance(out, float) and out.is_integer():
                out = int(out)
            if isinstance(out, bool) or not isinstance(out, int):
                continue
            per_message[mid] = max(per_message.get(mid, 0), out)
    return sum(per_message.values())


def measure_activity():
    now = time.time()
    newest = None
    sessions = 0
    for _, mtime in transcripts():
        if newest is None or mtime > newest:
            newest = mtime
        if now - mtime <= 300:
            sessions += 1
    idle = int(now - newest) if newest is not None else 86400 * 365
    return {"tok_per_min": output_tokens(now - 60, now), "idle_s": idle, "sessions": sessions}


# ---- dead reckoning: the session % between two API readings -----------------------------
# Each API reading is an anchor (pct at time). When a new reading arrives, the tokens produced
# between the two anchors calibrate k = percent per output token (smoothed). Between readings
# the estimate is anchor + k * tokens since the anchor, snapping back at the next reading.
def load_calib():
    try:
        with open(CALIB) as fh:
            c = json.load(fh)
        if isinstance(c["anchorAt"], bool) or not isinstance(c["anchorAt"], int):
            return None  # the Swift side decodes anchorAt as Int and would start over too
        return {"anchorPct": float(c["anchorPct"]), "anchorAt": c["anchorAt"],
                "k": float(c["k"]), "samples": int(c["samples"])}
    except (OSError, ValueError, KeyError, TypeError):
        return None


def save_calib(c):
    try:
        os.makedirs(CACHE_DIR, exist_ok=True)
        tmp = CALIB + ".tmp"
        with open(tmp, "w") as fh:
            json.dump(c, fh, separators=(",", ":"))
        os.replace(tmp, CALIB)
    except OSError:
        pass


def estimate(pct, fetched):
    c = load_calib()
    if c is None:
        c = {"anchorPct": pct, "anchorAt": fetched, "k": 0.0, "samples": 0}
        save_calib(c)
    if fetched != c["anchorAt"]:  # a new API reading
        if fetched > c["anchorAt"] and pct >= c["anchorPct"]:
            tokens = output_tokens(float(c["anchorAt"]), float(fetched))
            dpct = pct - c["anchorPct"]
            if tokens >= 1000 and dpct >= 1:
                kobs = dpct / tokens
                if 1e-8 < kobs < 1e-2:
                    c["k"] = 0.6 * c["k"] + 0.4 * kobs if c["k"] > 0 else kobs
                    c["samples"] += 1
        c["anchorPct"], c["anchorAt"] = pct, fetched
        save_calib(c)
    est, since = pct, 0
    if c["k"] > 0:
        since = output_tokens(float(fetched), time.time())
        est = min(100.0, pct + c["k"] * since)
    return {"pct_est": math.floor(est * 10 + 0.5) / 10, "pct_api": pct, "calibrated": c["k"] > 0,
            "k": c["k"], "samples": c["samples"], "tokens_since": since}


def main(argv):
    if len(argv) == 2 and argv[1] == "--activity":
        out = measure_activity()
    elif len(argv) == 4 and argv[1] == "--estimate":
        try:
            out = estimate(float(argv[2]), int(argv[3]))
        except ValueError:
            sys.exit("pulse-activity: --estimate needs a percentage and an epoch")
    else:
        sys.stderr.write("usage: pulse-activity.py --activity | --estimate PCT FETCHED\n")
        return 64
    print(json.dumps(out, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
