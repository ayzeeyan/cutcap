"""Summarize the opt-in real-editor probe; do not interpret DrawImage as scanout."""
import argparse
import json
import math
from pathlib import Path


def distribution(values):
    values = sorted(values)
    if not values:
        return None
    return {"n": len(values), "min": round(values[0], 3),
            **{f"p{p}": round(values[max(0, math.ceil(len(values) * p / 100) - 1)], 3)
               for p in (50, 95, 99)}, "max": round(values[-1], 3)}


def summarize(document):
    if document.get("error") is not None or len(document["runs"]) != 15:
        raise ValueError("Probe must complete all 15 episodes without an error")
    groups = []
    for name in ("720p", "1080p", "4k", "vfr", "large"):
        runs = [r for r in document["runs"] if r["source"] == name]
        frames = [f for r in runs for f in r["frames"]]
        gaps, cuts = [], []
        for run in runs:
            for a, b in zip(run["frames"], run["frames"][1:]):
                gap = b["WallMs"] - a["WallMs"]
                gaps.append(gap)
                if a["ClipId"] != b["ClipId"]:
                    cuts.append(gap)
        groups.append({
            "source": name, "episodes": len(runs), "clip_count": runs[0]["clipCount"],
            "client": runs[0]["client"], "scale": runs[0]["scale"],
            "seconds": [r["seconds"] for r in runs],
            "distinct_render_count": [r["rendered"] for r in runs],
            "distinct_render_fps_including_startup": distribution([r["distinctRenderFps"] for r in runs]),
            "play_to_first_render_ms": distribution([r["frames"][0]["WallMs"] for r in runs]),
            "render_gap_ms": distribution(gaps),
            "last_outgoing_to_first_incoming_render_ms": distribution(cuts),
            "seek_to_render_ms": distribution([s["renderLatencyMs"] for r in runs for s in r["seeks"]]),
            "frame_copy_us": distribution([f["CopyUs"] for f in frames]),
            "sampled_timeline_draw_us": distribution([f["TimelineDrawUs"] for f in frames]),
            "diagnostic_dispatcher_gap_ms": distribution([g for r in runs for g in r["dispatcherGapsMs"]]),
            "counters_by_episode": {key: [r[key] for r in runs] for key in (
                "Decoded", "Offered", "Copied", "Dropped", "BufferStalls", "Handoffs", "DecoderStarts")},
            "peak_host_plus_children_working_bytes": max(r["peakHostAndChildrenWorkingSetBytes"] for r in runs),
            "peak_host_plus_children_private_bytes": max(r["peakHostAndChildrenPrivateBytes"] for r in runs),
            "host_working_peaks_by_episode": [r["peakHostWorkingSetBytes"] for r in runs],
            "peak_observed_children": max(r["peakObservedChildren"] for r in runs),
            "observed_cpu_machine_percent_lower_bound": distribution([r["cpuObservedMachinePercent"] for r in runs]),
            "peak_view_bytes": max(r["peakViewBytes"] for r in runs),
            "timeline_bytes": max(r["timelineBytes"] for r in runs),
            "timeline_document_reads_during_play": [r["timelineReadsDuringPlay"] for r in runs],
        })
    return {"date_utc": document["date"], "method": document["method"],
            "percentiles": "nearest rank, pooled within source; three Release episodes per source/project",
            "hardware": "Core Ultra 7 255H; Intel integrated graphics; 16 GB; 1920x1200 at 150%; 16 logical processors",
            "maximum_rgba_frame_bytes": 1280 * 720 * 4, "groups": groups}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    result = summarize(json.loads(args.input.read_text(encoding="utf-8-sig")))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
