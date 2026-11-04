"""Summarise a CutCap.PlaybackProbe --ui report: render cadence per source.

Usage: python -I scripts/native-ui/cadence.py <report.json> [<report.json> ...]
Gaps are between distinct FrameView.DrawImage generations (render callbacks),
not physical scan-out.
"""
import json
import sys
from collections import Counter


def pct(values, p):
    ordered = sorted(values)
    if not ordered:
        return float("nan")
    return ordered[min(len(ordered) - 1, int(round(p / 100 * (len(ordered) - 1))))]


for path in sys.argv[1:]:
    report = json.load(open(path, encoding="utf-8"))
    print(f"== {path}  error={report.get('error')}")
    by_source = {}
    for run in report["runs"]:
        by_source.setdefault(run["source"], []).append(run)
    for source, runs in by_source.items():
        gaps = [g for run in runs for g in run["gapsMs"]]
        fps = [run["distinctRenderFps"] for run in runs]
        renders = [run["rendered"] for run in runs]
        bins = Counter(min(int(g // 8.33), 9) for g in gaps)
        hist = " ".join(f"{b * 8:>2}+:{bins.get(b, 0)}" for b in range(10))
        print(
            f"{source:>5}: renders {renders} fps {min(fps):.2f}-{max(fps):.2f} | gap p50 {pct(gaps, 50):.1f} "
            f"p95 {pct(gaps, 95):.1f} max {max(gaps):.1f} ms | >40ms {sum(g > 40 for g in gaps)}/{len(gaps)} "
            f"| dropped {[r['Dropped'] for r in runs]} stalls {[r['BufferStalls'] for r in runs]} "
            f"| maxDispatcherGap {max(r['maxDispatcherGapMs'] for r in runs):.1f}"
        )
        print(f"       gap histogram (ms buckets): {hist}")
