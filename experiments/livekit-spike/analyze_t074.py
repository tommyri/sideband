"""T-074 figures from one run directory of run.sh's t074-* scenarios.

Usage: python3 -I analyze_t074.py <run-dir>/<scenario>

Reads the per-frame lines the t074 viewers print ({"f": rendition}), their
group events, the publisher's per-second windows and the impairment
schedule, and prints the acceptance figures as JSON.
"""

import json
import statistics
import sys
from pathlib import Path


def lines(path):
    if not path.exists():
        return []
    out = []
    for line in path.read_text().splitlines():
        try:
            out.append(json.loads(line))
        except json.JSONDecodeError:
            pass
    return out


def viewer(d, name, t1, t2):
    rows = lines(d / f"{name}.log")
    frames = [(r["t_ms"], r["f"]) for r in rows if "f" in r]
    windows = [r for r in rows if "fps" in r]
    groups = [r["detail"] for r in rows if r.get("event") == "group"]
    gaps = [r["detail"] for r in rows if r.get("event") == "group_gap"]
    out = {"frames": len(frames), "group_gaps": gaps}
    ids = [g["id"] for g in groups if g["rendition"] == 0]
    out["full_groups"] = len(ids)
    out["full_groups_skipped"] = sum(b - a - 1 for a, b in zip(ids, ids[1:]) if b > a + 1)
    if t1 is None:
        steady = [w["fps"] for w in windows[3:-1]]
    else:
        steady = [w["fps"] for w in windows if t1 <= w["t_ms"] <= t2]
    out["fps_min"] = min(steady) if steady else None
    out["fps_median"] = statistics.median(steady) if steady else None
    out["low_frames"] = sum(1 for _, f in frames if f == 1)
    lat = [r["lat"] for r in rows if "f" in r and r.get("lat") is not None]
    if lat:
        out["latency_p50_ms"] = statistics.median(lat)
    if t1 is None:
        return out
    after = [(t, f) for t, f in frames if t >= t1]
    switch = next((t for t, f in after if f == 1), None)
    out["switch_s"] = None if switch is None else round((switch - t1) / 1000, 2)
    before = [t for t, _ in frames if t < t1]
    if before:
        out["freeze_before_switch_ms"] = (switch or t2) - before[-1]
    if switch is not None:
        seq = [t for t, _ in frames if switch <= t <= t2 + 30_000]
        g = [b - a for a, b in zip(seq, seq[1:])]
        out["max_gap_after_switch_ms"] = max(g) if g else None
        out["gaps_over_1s_after_switch"] = sum(1 for x in g if x > 1000)
        during = [t for t, _ in frames if switch <= t <= t2]
        dg = [b - a for a, b in zip(during, during[1:])]
        out["max_gap_during_impairment_ms"] = max(dg) if dg else None
    # Back on full for good: the first full frame after the end of the
    # impairment from which no low frame follows.
    post = [(t, f) for t, f in frames if t >= t2]
    back = None
    for i, (t, f) in enumerate(post):
        if f == 0 and all(ff == 0 for _, ff in post[i:]):
            back = t
            break
    out["back_on_full_s"] = None if back is None else round((back - t2) / 1000, 2)
    windows_during = [w for w in windows if switch and switch <= w["t_ms"] <= t2]
    if windows_during:
        out["kbps_during_median"] = statistics.median(w["kbps"] for w in windows_during)
    return out


def main():
    d = Path(sys.argv[1])
    phases = [r for r in lines(d / "impair.log") if r.get("event") == "phase"]
    t1 = phases[1]["t_ms"] if len(phases) > 1 else None
    t2 = phases[2]["t_ms"] if len(phases) > 2 else None
    pub = [r for r in lines(d / "publisher.log") if "fps" in r]
    result = {"scenario": d.name}
    for name in ("viewer1", "viewer2"):
        result[name] = viewer(d, name, t1, t2)
    if pub:
        totals = pub[-1].get("totals")
        if totals:
            low = totals[1]
            full = totals[0]
            result["low_keyframe_share"] = (
                round(low["keyframe_bytes"] / low["bytes"], 3) if low["bytes"] else None
            )
            result["full_keyframe_share"] = (
                round(full["keyframe_bytes"] / full["bytes"], 3) if full["bytes"] else None
            )
            result["low_keyframes"] = low["keyframes"]
        steady = pub[3:-2]
        result["sender_fps_min"] = min(w["fps"] for w in steady) if steady else None
        result["sender_full_frames_per_s_min"] = (
            min(w["full"]["frames"] for w in steady if "full" in w) if steady else None
        )
        both = [w for w in steady if w.get("low", {}).get("frames", 0) > 0]
        result["sender_cpu_pct_median_both_on"] = (
            statistics.median(w["cpu_pct"] for w in both) if both else None
        )
        result["sender_cpu_pct_median"] = (
            statistics.median(w["cpu_pct"] for w in steady) if steady else None
        )
        result["encode_ms_median"] = (
            statistics.median(w["encode_ms"] for w in steady) if steady else None
        )
        full_kbps = [w["full"]["kbps"] for w in steady if "full" in w]
        low_kbps = [w["low"]["kbps"] for w in both]
        result["full_kbps_median"] = statistics.median(full_kbps) if full_kbps else None
        result["low_kbps_median"] = statistics.median(low_kbps) if low_kbps else None
        targets = [w["target_bps"] for w in pub if "target_bps" in w]
        result["target_bps_min"] = min(targets) if targets else None
    events = [r for r in lines(d / "publisher.log") if r.get("event") == "bitrate"]
    result["bitrate_changes"] = len(events)
    server = [r for r in lines(d / "server.log") if "bytes_full" in r]
    if len(server) > 2:
        span = (server[-1]["t_ms"] - server[1]["t_ms"]) / 1000
        result["server_kbps_full"] = round(
            (server[-1]["bytes_full"] - server[1]["bytes_full"]) * 8 / span / 1000
        )
        result["server_kbps_low"] = round(
            (server[-1]["bytes_low"] - server[1]["bytes_low"]) * 8 / span / 1000
        )
        result["server_cpu_pct_median"] = statistics.median(r["cpu_pct"] for r in server)
        result["switches"] = {"down": server[-1]["down"], "up": server[-1]["up"]}
    print(json.dumps(result, indent=1))


main()
