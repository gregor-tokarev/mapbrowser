#!/usr/bin/env python3
"""Summarize a fixed, foreground-verified interval of benchmark_metrics output."""
import argparse
import json
from pathlib import Path
import statistics


def summarize(report, bundle_id, settle=30, duration=30):
    if report.get("interrupted"):
        raise ValueError("Measurement was interrupted")
    samples = [s for s in report["samples"]
               if settle <= s["elapsed_seconds"] < settle + duration]
    interval = report["requested_interval_seconds"]
    if (len(samples) < duration / interval - 1
            or not samples
            or samples[0]["elapsed_seconds"] > settle + interval
            or samples[-1]["elapsed_seconds"] < settle + duration - interval * 2):
        raise ValueError("Incomplete measurement interval")
    if any(s.get("frontmost_application", {}).get("bundle_identifier") != bundle_id
           for s in samples):
        raise ValueError("Browser was not verified foreground throughout the interval")
    if any(s.get("read_errors") for s in samples):
        raise ValueError("Process metrics were unreadable")
    roots = set(report["root_pids"])
    groups = {"whole_browser": [], "main": [], "renderers": [], "other_helpers": []}
    for sample in samples:
        totals = {key: 0 for key in groups}
        for process in sample["processes"]:
            size = process["phys_footprint_bytes"] / 1048576
            group = ("main" if process["pid"] in roots else
                     "renderers" if "(Renderer)" in process["command"] else "other_helpers")
            totals[group] += size
            totals["whole_browser"] += size
        for key, value in totals.items():
            groups[key].append(value)
    return {
        "label": report["label"],
        "bundle_identifier": bundle_id,
        "interval_seconds": [settle, settle + duration],
        "sample_count": len(samples),
        "footprint_mib": {key: {"mean": statistics.mean(values),
                                "min": min(values), "max": max(values)}
                          for key, values in groups.items()},
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path)
    parser.add_argument("--bundle-id", required=True)
    parser.add_argument("--settle", type=float, default=30)
    parser.add_argument("--duration", type=float, default=30)
    args = parser.parse_args()
    if args.settle < 0 or args.duration <= 0:
        parser.error("Settle must be nonnegative and duration positive")
    try:
        result = summarize(json.loads(args.report.read_text()), args.bundle_id,
                           args.settle, args.duration)
    except (ValueError, KeyError) as error:
        parser.error(str(error))
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
