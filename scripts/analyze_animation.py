#!/usr/bin/env python3
"""Analyze ANIMATION JSON log records. Render callbacks do not prove frame presentation."""

import argparse
import json
import math
from pathlib import Path
import re
import statistics
import sys


MARKER = re.compile(r"(?:^|\s)ANIMATION\s+(.*)$")
OUTLIER_MS = 2000.0


def number(value, name):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError(f"{name} must be a number")
    result = float(value)
    if not math.isfinite(result) or result < 0:
        raise ValueError(f"{name} must be finite and nonnegative")
    return result


def percentile(values, fraction):
    ordered = sorted(values)
    position = (len(ordered) - 1) * fraction
    lower = math.floor(position)
    upper = math.ceil(position)
    return ordered[lower] + (ordered[upper] - ordered[lower]) * (position - lower)


def distribution(values):
    if not values:
        return {"count": 0, "min_ms": None, "mean_ms": None, "p50_ms": None, "p95_ms": None, "max_ms": None}
    return {
        "count": len(values),
        "min_ms": round(min(values), 4),
        "mean_ms": round(statistics.mean(values), 4),
        "p50_ms": round(percentile(values, 0.50), 4),
        "p95_ms": round(percentile(values, 0.95), 4),
        "max_ms": round(max(values), 4),
    }


def gap_distribution(values):
    return {
        **distribution(values),
        "gaps_over_16_7_ms": sum(value > 16.7 for value in values),
        "gaps_over_33_3_ms": sum(value > 33.3 for value in values),
    }


def parse_transition(payload, source_line, index):
    data = json.loads(payload)
    if not isinstance(data, dict):
        raise ValueError("Animation record must be a JSON object")
    elapsed = number(data.get("elapsed_ms"), "elapsed_ms")
    raw_frames = data.get("frames")
    if not isinstance(raw_frames, list) or not raw_frames:
        raise ValueError("frames must be a nonempty list of elapsed render timestamps")
    frames = [number(value, f"frames[{i}]") for i, value in enumerate(raw_frames)]
    if any(later < earlier for earlier, later in zip(frames, frames[1:])):
        raise ValueError("frames must be in nondecreasing timestamp order")
    if frames[-1] > elapsed + 0.000001:
        raise ValueError("The final frame timestamp exceeds elapsed_ms")
    gaps = [later - earlier for earlier, later in zip(frames, frames[1:])]
    outlier = elapsed > OUTLIER_MS
    return {
        "index": index,
        "source_line": source_line,
        "from": data.get("from"),
        "to": data.get("to"),
        "duration_ms": elapsed,
        "render_callback_count": len(frames),
        "first_render_delay_ms": frames[0],
        "render_timestamps_ms": frames,
        "frame_gaps_ms": gaps,
        "frame_gap_summary": gap_distribution(gaps),
        "excluded_from_primary_summary": outlier,
        "exclusion_reason": "Duration exceeds 2000 ms. An occluded window can delay render callbacks; the log alone does not establish the cause." if outlier else None,
    }


def summarize(transitions):
    return {
        "transition_count": len(transitions),
        "duration": distribution([item["duration_ms"] for item in transitions]),
        "first_render_delay": distribution([item["first_render_delay_ms"] for item in transitions]),
        "frame_gaps": gap_distribution([gap for item in transitions for gap in item["frame_gaps_ms"]]),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("logfile", type=Path, help="Log captured with MAPBROWSER_TRACE_ANIMATION=1")
    parser.add_argument("--output", type=Path, help="Write JSON here; otherwise write it to stdout")
    args = parser.parse_args()
    try:
        lines = args.logfile.read_text(errors="replace").splitlines()
    except OSError as error:
        parser.error(str(error))

    transitions = []
    invalid = []
    record_count = 0
    for source_line, line in enumerate(lines, 1):
        match = MARKER.search(line)
        if match is None:
            continue
        record_count += 1
        try:
            transitions.append(parse_transition(match.group(1), source_line, record_count))
        except (ValueError, TypeError, OverflowError) as error:
            invalid.append({"source_line": source_line, "error": str(error)})

    included = [item for item in transitions if not item["excluded_from_primary_summary"]]
    outliers = [item for item in transitions if item["excluded_from_primary_summary"]]
    report = {
        "source": str(args.logfile.resolve()),
        "animation_record_count": record_count,
        "valid_record_count": len(transitions),
        "invalid_records": invalid,
        "measurement": "GPUI render callback elapsed timestamps; not actual display presentation timestamps",
        "percentile_method": "Linear interpolation between adjacent sorted observations at position (count - 1) * percentile",
        "outlier_policy": {
            "duration_threshold_ms": OUTLIER_MS,
            "comparison": "strictly greater than",
            "excluded_count": len(outliers),
            "explanation": "Transitions over 2 seconds are reported separately and excluded from the primary summary. Their cause is unknown; occluded windows can delay render callbacks.",
        },
        "primary_summary": summarize(included),
        "outlier_summary": summarize(outliers),
        "all_transitions_summary": summarize(transitions),
        "transitions": transitions,
        "limitations": [
            "A render callback can occur without a frame being presented. These timestamps cannot prove visible smoothness, absence of dropped display frames, or the display refresh rate.",
            "Frame gaps are differences between consecutive logged render timestamps. Initial delay before the first render is reported separately.",
            "Threshold counts use strict >16.7 ms and >33.3 ms comparisons. They are callback gap counts, not counts of dropped display frames.",
            "Only completed transitions with ANIMATION records can be analyzed. Canceled, interrupted, or unfinished transitions may not be logged.",
            "Duration outliers remain in the per-transition records and all-transitions summary. The log does not reveal whether window occlusion, scheduler delay, or an animation defect caused them.",
            "Pooled frame-gap statistics give each observed gap equal weight; transitions with more callbacks contribute more observations.",
        ],
    }
    encoded = json.dumps(report, indent=2, allow_nan=False) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(encoded)
        print(f"Wrote {args.output}: {len(included)} included transitions, {len(outliers)} duration outliers, {len(invalid)} invalid records.", file=sys.stderr)
    else:
        sys.stdout.write(encoded)
    if not transitions:
        print("No valid ANIMATION records found.", file=sys.stderr)
        return 2
    if invalid:
        print("Some ANIMATION records were invalid; inspect invalid_records before using the report.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
