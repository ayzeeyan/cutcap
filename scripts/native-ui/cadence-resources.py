"""Memory, CPU and seek-to-render summary of CutCap.PlaybackProbe --ui reports.

Usage: python -I scripts/native-ui/cadence-resources.py <report.json> [...]
Working set / private bytes are sampled maxima for the host plus its decoder child.
"""
import json
import sys

for path in sys.argv[1:]:
    report = json.load(open(path, encoding="utf-8"))
    for source in ("720p", "1080p", "4k"):
        runs = [run for run in report["runs"] if run["source"] == source]
        if not runs:
            continue
        working = max(run["peakHostAndChildWorkingSetBytes"] for run in runs) / 2**20
        private = max(run["peakHostAndChildPrivateBytes"] for run in runs) / 2**20
        cpu = [round(run["cpuMachinePercent"], 1) for run in runs]
        seeks = sorted(sample["renderLatencyMs"] for run in runs for sample in run["seeks"])
        median = seeks[len(seeks) // 2] if seeks else float("nan")
        print(f"{path} {source:>5}: peak WS {working:6.1f} MiB, private {private:6.1f} MiB, CPU % of machine {cpu}, "
              f"seek-to-render median {median:.0f} [{seeks[0]:.0f}, {seeks[-1]:.0f}] ms (n={len(seeks)})")
