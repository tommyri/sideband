#!/usr/bin/env python3
"""Scrape ss-server /metrics plus process stats to CSV, then graph and summarize.

Standard library only. Graphs are hand-drawn SVG so no plotting package is
needed on the VPS.

  load-metrics.py scrape --pid PID [--url URL] [--interval 5] [--duration S] --out run.csv
  load-metrics.py plot run.csv --out run.svg [--title TEXT]
  load-metrics.py summary run.csv [--warmup 60]

`scrape` stops after --duration seconds, when the process exits, or on Ctrl-C.
CPU is the process's own CPU time over wall time (100 = one core). RSS comes
from `ps`. Open descriptors come from /proc on Linux and `lsof` elsewhere.
"""

import argparse
import csv
import math
import os
import re
import signal
import subprocess
import sys
import time
import urllib.request
from xml.sax.saxutils import escape

COUNTERS = {
    "socialspeak_connections": "connections",
    "socialspeak_voice_members": "voice_members",
    "socialspeak_datagrams_received_total": "received",
    "socialspeak_datagrams_forwarded_total": "forwarded",
    "socialspeak_datagrams_dropped_total": "dropped",
    "socialspeak_datagrams_rejected_total": "rejected",
    "socialspeak_messages_total": "messages",
    "socialspeak_sessions_total": "sessions",
    "socialspeak_slow_disconnects_total": "slow_disconnects",
    "socialspeak_db_seconds_count": "db_count",
    "socialspeak_db_seconds_sum": "db_sum",
}
FIELDS = ["t_s", "rss_kb", "cpu_s", "cpu_pct", "fds"] + list(COUNTERS.values()) + [
    "db_p99_ms",
    "db_max_ms",
]
LINE = re.compile(r'^([a-zA-Z_:][a-zA-Z0-9_:]*)(\{[^}]*\})?\s+(\S+)')


def parse_metrics(text):
    values = {}
    for line in text.splitlines():
        if line.startswith("#"):
            continue
        match = LINE.match(line)
        if not match:
            continue
        name, labels, value = match.groups()
        try:
            number = float(value)
        except ValueError:
            continue
        if name in COUNTERS and not labels:
            values[COUNTERS[name]] = number
        elif name == "socialspeak_db_seconds" and labels:
            if 'quantile="0.99"' in labels:
                values["db_p99_ms"] = number * 1000.0
            elif 'quantile="1"' in labels:
                values["db_max_ms"] = number * 1000.0
    return values


def cpu_seconds(text):
    """`ps -o time=` is [[dd-]hh:]mm:ss[.ss]."""
    text = text.strip()
    days = 0
    if "-" in text:
        day, text = text.split("-", 1)
        days = int(day)
    parts = [float(part) for part in text.split(":")]
    seconds = 0.0
    for part in parts:
        seconds = seconds * 60 + part
    return days * 86400 + seconds


def process_sample(pid):
    out = subprocess.run(
        ["ps", "-o", "rss=,time=", "-p", str(pid)],
        capture_output=True,
        text=True,
        check=False,
    ).stdout.split()
    if len(out) < 2:
        return None
    rss_kb = int(out[0])
    cpu = cpu_seconds(out[1])
    proc = f"/proc/{pid}/fd"
    if os.path.isdir(proc):
        fds = len(os.listdir(proc))
    else:
        listing = subprocess.run(
            ["lsof", "-n", "-P", "-p", str(pid)],
            capture_output=True,
            text=True,
            check=False,
        ).stdout
        fds = max(0, len(listing.splitlines()) - 1)
    return rss_kb, cpu, fds


def stop_on_term(_signum, _frame):
    raise KeyboardInterrupt


def scrape(args):
    signal.signal(signal.SIGTERM, stop_on_term)
    started = time.monotonic()
    previous = None
    with open(args.out, "w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=FIELDS)
        writer.writeheader()
        handle.flush()
        try:
            while args.duration is None or time.monotonic() - started <= args.duration:
                tick = time.monotonic()
                sample = process_sample(args.pid)
                if sample is None:
                    print("process exited", file=sys.stderr)
                    break
                try:
                    with urllib.request.urlopen(args.url, timeout=5) as response:
                        metrics = parse_metrics(response.read().decode())
                except OSError as error:
                    print(f"scrape failed: {error}", file=sys.stderr)
                    metrics = {}
                rss_kb, cpu, fds = sample
                cpu_pct = ""
                if previous is not None and tick > previous[0]:
                    cpu_pct = f"{(cpu - previous[1]) * 100.0 / (tick - previous[0]):.1f}"
                previous = (tick, cpu)
                row = {"t_s": f"{tick - started:.1f}", "rss_kb": rss_kb, "cpu_s": f"{cpu:.2f}",
                       "cpu_pct": cpu_pct, "fds": fds}
                for field in FIELDS[5:]:
                    value = metrics.get(field)
                    row[field] = "" if value is None else f"{value:g}"
                writer.writerow(row)
                handle.flush()
                time.sleep(max(0.0, args.interval - (time.monotonic() - tick)))
        except KeyboardInterrupt:
            pass


def load_rows(path):
    with open(path, newline="") as handle:
        rows = list(csv.DictReader(handle))
    def number(row, key):
        value = row.get(key, "")
        return float(value) if value not in ("", None) else None
    return [{key: number(row, key) for key in row} for row in rows]


def rates(rows, key):
    """Per-second rate between consecutive samples."""
    points = []
    for before, after in zip(rows, rows[1:]):
        if before[key] is None or after[key] is None:
            continue
        span = after["t_s"] - before["t_s"]
        if span > 0:
            points.append((after["t_s"], max(0.0, after[key] - before[key]) / span))
    return points


def interval_mean_ms(rows):
    points = []
    for before, after in zip(rows, rows[1:]):
        if None in (before["db_count"], after["db_count"], before["db_sum"], after["db_sum"]):
            continue
        count = after["db_count"] - before["db_count"]
        if count > 0:
            points.append((after["t_s"], (after["db_sum"] - before["db_sum"]) * 1000.0 / count))
    return points


def series(rows, key, scale=1.0):
    return [(row["t_s"], row[key] * scale) for row in rows if row[key] is not None]


def thin(points, limit):
    """Average consecutive points so a long run stays a small file."""
    if limit <= 0 or len(points) <= limit:
        return points
    size = math.ceil(len(points) / limit)
    out = []
    for start in range(0, len(points), size):
        chunk = points[start:start + size]
        out.append((chunk[-1][0], sum(v for _, v in chunk) / len(chunk)))
    return out


def nice_max(value):
    if value <= 0:
        return 1.0
    exponent = math.floor(math.log10(value))
    for step in (1, 2, 2.5, 5, 10):
        top = step * 10 ** exponent
        if top >= value:
            return top
    return 10 ** (exponent + 1)


def fmt(value):
    if value >= 1000:
        return f"{value / 1000:g}k"
    return f"{value:g}"


TIME_STEPS = [5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200]


def time_label(seconds, t_max):
    if t_max >= 600:
        return f"{seconds / 60:.0f}m"
    return f"{seconds:.0f}s"


def panel(x0, y0, width, height, title, lines, t_max):
    """One small chart. `lines` is [(label, css_class, points)]."""
    parts = []
    left, right, top, bottom = 44, 8, 22, 22
    plot_w, plot_h = width - left - right, height - top - bottom
    values = [v for _, _, points in lines for _, v in points]
    y_max = nice_max(max(values) if values else 1.0)
    parts.append(f'<text class="title" x="{x0 + left}" y="{y0 + 14}">{escape(title)}</text>')
    for index in range(5):
        value = y_max * index / 4
        y = y0 + top + plot_h - plot_h * index / 4
        parts.append(f'<line class="grid" x1="{x0 + left}" x2="{x0 + left + plot_w}" y1="{y:.1f}" y2="{y:.1f}"/>')
        parts.append(f'<text class="tick" x="{x0 + left - 4}" y="{y + 3:.1f}" text-anchor="end">{fmt(value)}</text>')
    step = next((step for step in TIME_STEPS if t_max / step <= 5), TIME_STEPS[-1])
    for index in range(int(t_max // step) + 1):
        t = step * index
        x = x0 + left + plot_w * t / t_max
        parts.append(f'<text class="tick" x="{x:.1f}" y="{y0 + top + plot_h + 14}" text-anchor="middle">{time_label(t, t_max)}</text>')
    legend_x = x0 + width - right
    for label, css, points in lines:
        if not points:
            continue
        path = " ".join(
            f"{x0 + left + plot_w * t / t_max:.1f},{y0 + top + plot_h - plot_h * min(v, y_max) / y_max:.1f}"
            for t, v in points
        )
        parts.append(f'<polyline class="{css}" points="{path}"/>')
        if len(lines) > 1:
            parts.append(f'<text class="legend" x="{legend_x}" y="{y0 + 14}" text-anchor="end">{escape(label)}</text>')
            legend_x -= 5 * len(label) + 4
            parts.append(f'<line class="{css}" x1="{legend_x - 12}" x2="{legend_x}" y1="{y0 + 11}" y2="{y0 + 11}"/>')
            legend_x -= 22
    return "\n".join(parts)


def plot(args):
    rows = load_rows(args.csv)
    if len(rows) < 2:
        sys.exit("need at least two samples")
    t_max = max(row["t_s"] for row in rows) or 1.0
    panels = [
        ("CPU, % of one core", [("cpu", "s1", series(rows, "cpu_pct"))]),
        ("RSS, MiB", [("rss", "s1", series(rows, "rss_kb", 1 / 1024))]),
        ("Open file descriptors", [("fds", "s1", series(rows, "fds"))]),
        ("Datagrams per second", [("forwarded", "s1", rates(rows, "forwarded")),
                                  ("dropped", "s2", rates(rows, "dropped"))]),
        ("Live connections", [("connections", "s1", series(rows, "connections"))]),
        ("SQLite write lock, ms", [("interval mean", "s1", interval_mean_ms(rows)),
                                   ("p99", "s2", series(rows, "db_p99_ms"))]),
    ]
    panels = [(title, [(label, css, thin(points, args.max_points)) for label, css, points in lines])
              for title, lines in panels]
    width, height, columns = 300, 170, 3
    total_w = width * columns
    header = 28
    total_h = header + height * math.ceil(len(panels) / columns)
    body = []
    for index, (title, lines) in enumerate(panels):
        x0 = (index % columns) * width
        y0 = header + (index // columns) * height
        body.append(panel(x0, y0, width, height, title, lines, t_max))
    svg = f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {total_w} {total_h}" width="{total_w}" height="{total_h}" font-family="system-ui, -apple-system, sans-serif">
<style>
  .bg {{ fill: #fcfcfb; }}
  .head {{ font-size: 13px; font-weight: 600; fill: #0b0b0b; }}
  .title {{ font-size: 11px; fill: #0b0b0b; }}
  .tick, .legend {{ font-size: 9px; fill: #52514e; }}
  .grid {{ stroke: #e4e3df; stroke-width: 1; }}
  .s1 {{ fill: none; stroke: #2a78d6; stroke-width: 2; stroke-linejoin: round; }}
  .s2 {{ fill: none; stroke: #eb6834; stroke-width: 2; stroke-linejoin: round; stroke-dasharray: 4 2; }}
  @media (prefers-color-scheme: dark) {{
    .bg {{ fill: #1a1a19; }}
    .head, .title {{ fill: #f0efec; }}
    .tick, .legend {{ fill: #c3c2b7; }}
    .grid {{ stroke: #3a3a37; }}
    .s1 {{ stroke: #3987e5; }}
    .s2 {{ stroke: #d95926; }}
  }}
</style>
<rect class="bg" width="100%" height="100%"/>
<text class="head" x="12" y="18">{escape(args.title or os.path.basename(args.csv))}</text>
{chr(10).join(body)}
</svg>
"""
    with open(args.out, "w") as handle:
        handle.write(svg)


def slope_per_hour(points):
    if len(points) < 2:
        return 0.0
    n = len(points)
    mean_t = sum(t for t, _ in points) / n
    mean_v = sum(v for _, v in points) / n
    var = sum((t - mean_t) ** 2 for t, _ in points)
    if var == 0:
        return 0.0
    return sum((t - mean_t) * (v - mean_v) for t, v in points) / var * 3600.0


def summary(args):
    rows = load_rows(args.csv)
    if len(rows) < 2:
        sys.exit("need at least two samples")
    first, last = rows[0], rows[-1]
    steady = [row for row in rows if row["t_s"] >= args.warmup] or rows
    base = steady[0]
    rss = [(row["t_s"], row["rss_kb"]) for row in steady]
    cpu = [row["cpu_pct"] for row in rows if row["cpu_pct"] is not None]
    def delta(key):
        if first[key] is None or last[key] is None:
            return 0.0
        return last[key] - first[key]
    forwarded, dropped = delta("forwarded"), delta("dropped")
    attempts = forwarded + dropped
    db = interval_mean_ms(rows)
    db_p99 = [row["db_p99_ms"] for row in rows if row["db_p99_ms"] is not None]
    db_max = [row["db_max_ms"] for row in rows if row["db_max_ms"] is not None]
    fds = [row["fds"] for row in rows if row["fds"] is not None]
    span = last["t_s"] - first["t_s"]
    growth = (last["rss_kb"] - base["rss_kb"]) * 100.0 / base["rss_kb"] if base["rss_kb"] else 0.0
    db_writes = delta("db_count")
    db_mean = delta("db_sum") * 1000.0 / db_writes if db_writes else 0.0
    report = {
        "duration_s": f"{span:.0f}",
        "rss_start_mib": f"{first['rss_kb'] / 1024:.1f}",
        "rss_after_warmup_mib": f"{base['rss_kb'] / 1024:.1f}",
        "rss_end_mib": f"{last['rss_kb'] / 1024:.1f}",
        "rss_max_mib": f"{max(r['rss_kb'] for r in rows) / 1024:.1f}",
        "rss_growth_after_warmup_pct": f"{growth:.1f}",
        "rss_slope_mib_per_h": f"{slope_per_hour(rss) / 1024:.2f}",
        "cpu_mean_pct": f"{sum(cpu) / len(cpu):.1f}" if cpu else "",
        "cpu_max_pct": f"{max(cpu):.1f}" if cpu else "",
        "forwarded_per_s": f"{forwarded / span:.0f}" if span else "",
        "dropped": f"{dropped:.0f}",
        "drop_pct": f"{dropped * 100.0 / attempts:.4f}" if attempts else "0",
        "rejected": f"{delta('rejected'):.0f}",
        "messages": f"{delta('messages'):.0f}",
        "db_writes": f"{db_writes:.0f}",
        "db_mean_ms": f"{db_mean:.3f}",
        "db_interval_mean_max_ms": f"{max(v for _, v in db):.3f}" if db else "",
        "db_p99_max_ms": f"{max(db_p99):.2f}" if db_p99 else "",
        "db_max_ms": f"{max(db_max):.2f}" if db_max else "",
        "fds_start": f"{fds[0]:.0f}" if fds else "",
        "fds_end": f"{fds[-1]:.0f}" if fds else "",
        "fds_max": f"{max(fds):.0f}" if fds else "",
        "sessions_opened": f"{delta('sessions'):.0f}",
        "slow_disconnects": f"{delta('slow_disconnects'):.0f}",
    }
    for key, value in report.items():
        print(f"{key}={value}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    scrape_cmd = commands.add_parser("scrape", help="sample /metrics and the process to CSV")
    scrape_cmd.add_argument("--pid", type=int, required=True)
    scrape_cmd.add_argument("--url", default="http://127.0.0.1:9101/metrics")
    scrape_cmd.add_argument("--interval", type=float, default=5.0)
    scrape_cmd.add_argument("--duration", type=float)
    scrape_cmd.add_argument("--out", required=True)
    scrape_cmd.set_defaults(run=scrape)
    plot_cmd = commands.add_parser("plot", help="draw an SVG from a scrape CSV")
    plot_cmd.add_argument("csv")
    plot_cmd.add_argument("--out", required=True)
    plot_cmd.add_argument("--title")
    plot_cmd.add_argument("--max-points", type=int, default=150,
                          help="average down to this many points per line (0 keeps all)")
    plot_cmd.set_defaults(run=plot)
    summary_cmd = commands.add_parser("summary", help="print key=value totals for a scrape CSV")
    summary_cmd.add_argument("csv")
    summary_cmd.add_argument("--warmup", type=float, default=60.0,
                             help="seconds excluded from the RSS growth baseline")
    summary_cmd.set_defaults(run=summary)
    args = parser.parse_args()
    args.run(args)


if __name__ == "__main__":
    main()
