"""Summarize run.sh logs.

Usage: python3 -I analyze.py <out-dir> [scenario...]
       python3 -I analyze.py <out-dir> --impair <impair-scenario>...
"""
import json, statistics, sys, os

def lines(path):
    if not os.path.exists(path):
        return []
    out = []
    for raw in open(path):
        raw = raw.strip()
        if raw.startswith("{"):
            try:
                out.append(json.loads(raw))
            except json.JSONDecodeError:
                pass
    return out

def med(xs):
    xs = [x for x in xs if x is not None]
    return round(statistics.median(xs), 1) if xs else None

def pct(xs, p):
    xs = sorted(x for x in xs if x is not None)
    return round(xs[min(len(xs) - 1, int(round((len(xs) - 1) * p)))], 1) if xs else None

def window(rows, lo=5, hi=None):
    """Steady-state rows: skip the first `lo` seconds (join, BWE ramp)."""
    return [r for r in rows if "fps" in r and r.get("since_s", 0) >= lo and (hi is None or r["since_s"] < hi)]

def role_summary(path, lo=5):
    rows = window(lines(path), lo)
    if not rows:
        return {}
    return {
        "fps": med(r["fps"] for r in rows),
        "kbps": med(r["kbps"] for r in rows),
        "kbps_mean": round(statistics.mean(r["kbps"] for r in rows)),
        "lat_p50": med(r["lat_p50_ms"] for r in rows),
        "lat_p95": pct([r["lat_p95_ms"] for r in rows], 0.95),
        "max_gap": max(r["max_gap_ms"] for r in rows),
        "cpu": med(r["cpu_pct"] for r in rows),
        "mem": max(r["mem_mib"] for r in rows),
        "size": f'{rows[-1]["w"]}x{rows[-1]["h"]}',
    }

def sampler(path, key, conv=float):
    vals = []
    for r in lines(path):
        v = r.get(key)
        if isinstance(v, str):
            v = v.split()[0].rstrip("%").replace("MiB", "")
        try:
            vals.append(conv(v))
        except (TypeError, ValueError):
            pass
    return med(vals)

def main():
    out = sys.argv[1]
    names = sys.argv[2:] or sorted(os.listdir(out))
    for name in names:
        d = os.path.join(out, name)
        if not os.path.isdir(d):
            continue
        print(f"## {name}")
        for role in ["publisher", "viewer1", "viewer2", "server"]:
            p = os.path.join(d, f"{role}.log")
            if role == "server":
                rows = [r for r in lines(p) if r.get("role") == "server" and r.get("active")]
                if rows:
                    print(f"  server(ss-server): cpu {med(r['cpu_pct'] for r in rows)}% mem {max(r['mem_mib'] for r in rows)} MiB resets {rows[-1]['resets']}")
                continue
            s = role_summary(p)
            if s:
                print(f"  {role}: {s}")
            for e in lines(p):
                if "event" in e and e["event"] not in ("connection_state",):
                    print(f"    event {e['event']} {json.dumps(e.get('detail'))[:200]} @{e['t_ms']}")
        lk = os.path.join(d, "livekit-server.log")
        if os.path.exists(lk):
            print(f"  livekit-server(container): cpu {sampler(lk, 'cpu')}% mem {sampler(lk, 'mem')} MiB")
        print(f"  gpu device util median {sampler(os.path.join(d, 'gpu.log'), 'device_pct')}%")



def impairment(out, name):
    """Per-phase view of an impair scenario, keyed off the proxy's phase events."""
    d = os.path.join(out, name)
    phases = [e for e in lines(os.path.join(d, "impair.log")) if e.get("event") == "phase"]
    if len(phases) < 3:
        return
    start, end = phases[1]["t_ms"], phases[2]["t_ms"]
    print(f"## {name}: impaired {(end - start) / 1000:.0f} s from +{(start - phases[0]['t_ms']) / 1000:.0f} s")
    for role in ["viewer1", "viewer2"]:
        rows = [r for r in lines(os.path.join(d, f"{role}.log")) if "fps" in r]
        pre = [r for r in rows if start - 12000 <= r["t_ms"] < start]
        during = [r for r in rows if start <= r["t_ms"] < end]
        post = [r for r in rows if r["t_ms"] >= end]
        full_w = max((r["w"] for r in pre), default=0)

        def s(rs):
            return {
                "fps": med(r["fps"] for r in rs),
                "kbps": med(r["kbps"] for r in rs),
                "lat_p50": med(r["lat_p50_ms"] for r in rs),
                "max_gap": max((r["max_gap_ms"] for r in rs), default=None),
                "widths": sorted({r["w"] for r in rs}),
                "freeze_s": sum(1 for r in rs if r["max_gap_ms"] > 1000),
            }

        first_low = next((r for r in during if r["w"] and r["w"] < full_w), None)
        # Recovered: 3 consecutive seconds at full width, >= 90 % of the pre-impairment
        # frame rate, and latency within 2x (or +30 ms of) the pre-impairment median.
        base_lat = med(r["lat_p50_ms"] for r in pre) or 0
        base_fps = 0.9 * (med(r["fps"] for r in pre) or 0)
        recovered = None
        for i in range(len(post) - 2):
            win = post[i:i + 3]
            if all(r["w"] == full_w and r["fps"] >= base_fps and (r["lat_p50_ms"] or 1e9) <= max(2 * base_lat, base_lat + 30) for r in win):
                recovered = win[0]
                break
        print(f"  {role} pre {s(pre)}")
        print(f"  {role} during {s(during)}")
        print(f"  {role} post {s(post)}")
        if first_low:
            print(f"  {role} first lower layer after {(first_low['t_ms'] - start) / 1000:.1f} s ({first_low['w']} wide)")
        print(f"  {role} recovered to full quality {'after %.1f s' % ((recovered['t_ms'] - end) / 1000) if recovered else 'not within run'}")


if __name__ == "__main__":
    if len(sys.argv) > 2 and sys.argv[2] == "--impair":
        for n in sys.argv[3:]:
            impairment(sys.argv[1], n)
    else:
        main()
