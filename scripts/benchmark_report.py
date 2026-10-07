#!/usr/bin/env python3
"""Write a compact local benchmark report from validated analyzer output.

No browser access or new measurement occurs. Optional Activity Monitor readings
use the capture timestamp, never their informal phase label, for inclusion.
"""
from __future__ import annotations

import argparse
import csv
import json
import math
from datetime import datetime, timezone
from pathlib import Path
import re
import statistics


NAMES = {"chrome": "Chrome", "safari": "Safari", "arc": "Arc", "mapbrowser": "Mapbrowser", "map": "Mapbrowser"}
ORDER = {"chrome": 0, "safari": 1, "arc": 2, "mapbrowser": 3}


def browser_key(value):
    value = str(value or "").lower()
    return "mapbrowser" if value == "map" else value


def finite(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def numeric(value):
    """Parse an AX decimal, including decimal comma and Unicode grouping spaces."""
    if finite(value):
        return float(value)
    if not isinstance(value, str):
        return None
    value = re.sub(r"\s+", "", value)
    if not value or value.lower() in {"-", "—", "–", "null", "none", "n/a"}:
        return None
    if "," in value and "." in value:
        # The last punctuation is the decimal separator in localized AX values.
        if value.rfind(",") > value.rfind("."):
            value = value.replace(".", "").replace(",", ".")
        else:
            value = value.replace(",", "")
    else:
        value = value.replace(",", ".")
    try:
        result = float(value)
        return result if math.isfinite(result) and result >= 0 else None
    except ValueError:
        return None


def timestamp_ns(value):
    """Read an explicitly zoned ISO timestamp. Ambiguous local times are rejected."""
    if not isinstance(value, str):
        return None
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
        if parsed.tzinfo is None:
            return None
        parsed = parsed.astimezone(timezone.utc)
        epoch = datetime(1970, 1, 1, tzinfo=timezone.utc)
        delta = parsed - epoch
        return (delta.days * 86400 + delta.seconds) * 1_000_000_000 + delta.microseconds * 1000
    except ValueError:
        return None


def accepted_runs(summary):
    return [run for run in summary.get("runs", [])
            if run.get("valid") is True and (run.get("summary") or {}).get("valid") is True
            and (run.get("workload") == "speedometer-3.1"
                 or (run.get("workload") == "synthetic-idle" and run.get("expected_tabs") in {1, 10}))]


def match_energy(readings, runs):
    """Only one unambiguous valid run may receive a numeric capture."""
    matches = {run["run"]: [] for run in runs}
    diagnostics = {"total_readings": len(readings), "included_readings": 0,
                   "excluded_readings": 0, "outside_valid_windows": 0,
                   "unavailable_or_malformed": 0, "ambiguous_windows": 0,
                   "unverified_process_scope": 0}
    for reading in readings:
        if not isinstance(reading, dict):
            diagnostics["unavailable_or_malformed"] += 1
            continue
        captured = timestamp_ns(reading.get("timestamp"))
        # Never substitute row[2], the separate 12-hour power column.
        value = numeric(reading.get("energyImpact"))
        if captured is None or value is None:
            diagnostics["unavailable_or_malformed"] += 1
            continue
        if reading.get("process_scope") != "whole_browser_verified":
            diagnostics["unverified_process_scope"] += 1
            continue
        candidates = []
        for run in runs:
            bounds = run["summary"]
            start, end = bounds.get("requested_start_epoch_ns"), bounds.get("requested_end_epoch_ns")
            if (finite(start) and finite(end) and start <= captured <= end
                    and browser_key(reading.get("browser")) == browser_key(run.get("browser"))):
                candidates.append(run["run"])
        if len(candidates) == 1:
            matches[candidates[0]].append(value)
            diagnostics["included_readings"] += 1
        elif candidates:
            diagnostics["ambiguous_windows"] += 1
        else:
            diagnostics["outside_valid_windows"] += 1
    diagnostics["excluded_readings"] = diagnostics["total_readings"] - diagnostics["included_readings"]
    return matches, diagnostics


def stats(values):
    values = [value for value in values if finite(value)]
    return {"n": len(values), "median": statistics.median(values) if values else None,
            "min": min(values) if values else None, "max": max(values) if values else None}


def format_number(value, digits=2):
    return f"{value:.{digits}f}" if finite(value) else "unavailable"


def format_stats(values, digits=2):
    result = stats(values)
    if not result["n"]:
        return "unavailable"
    center = format_number(result["median"], digits)
    return center if result["n"] == 1 else f"{center} [{result['min']:.{digits}f} to {result['max']:.{digits}f}]"


def markdown(value):
    return str(value).replace("|", "\\|").replace("\n", " ")


def table(headers, rows):
    return "\n".join(["| " + " | ".join(headers) + " |",
                       "| " + " | ".join(["---"] * len(headers)) + " |",
                       *["| " + " | ".join(markdown(cell) for cell in row) + " |" for row in rows]])


CSV_FIELDS = [
    "browser", "workload", "tabs", "valid_run_count", "run_ids", "aggregation",
    "score_median", "score_min", "score_max", "mean_phys_footprint_mib_median",
    "mean_phys_footprint_mib_min", "mean_phys_footprint_mib_max",
    "mean_cpu_core_percent_median", "mean_cpu_core_percent_min", "mean_cpu_core_percent_max",
    "mean_cpu_watts_trial_count", "mean_cpu_watts_median", "mean_cpu_watts_min", "mean_cpu_watts_max",
    "coverage_fraction_min", "coverage_fraction_max", "covered_seconds_min", "covered_seconds_max",
    "energy_impact_count", "energy_impact_median", "energy_impact_min", "energy_impact_max",
    "energy_impact_counts_by_run", "warnings",
    "energy_impact_scope",
]


def comparison_rows(runs, energy):
    groups = []
    for browser in sorted({browser_key(run["browser"]) for run in runs}, key=lambda b: (ORDER.get(b, 9), b)):
        selected = [run for run in runs if browser_key(run["browser"]) == browser]
        groups.extend([[run] for run in selected if run["workload"] == "synthetic-idle"])
        active = [run for run in selected if run["workload"] == "speedometer-3.1"]
        if active:
            groups.append(active)
    rows = []
    for group in groups:
        first = group[0]
        row = {"browser": browser_key(first["browser"]), "workload": first["workload"],
               "tabs": first["expected_tabs"], "valid_run_count": len(group),
               "run_ids": ";".join(run["run"] for run in group),
               "aggregation": "single_run_mean" if first["workload"] == "synthetic-idle" else "median_of_trial_means"}
        fields = {"score": lambda run: (run.get("score") or {}).get("score"),
                  "mean_phys_footprint_mib": lambda run: run["summary"].get("mean_phys_footprint_sum_mib"),
                  "mean_cpu_core_percent": lambda run: run["summary"].get("mean_cpu_core_percent"),
                  "mean_cpu_watts": lambda run: run["summary"].get("mean_observed_cpu_power_watts"),
                  "coverage_fraction": lambda run: run["summary"].get("coverage_fraction"),
                  "covered_seconds": lambda run: run["summary"].get("covered_duration_seconds")}
        for name, getter in fields.items():
            result = stats([getter(run) for run in group])
            if name == "mean_cpu_watts":
                row["mean_cpu_watts_trial_count"] = result["n"]
            for key in ["median", "min", "max"]:
                if name not in {"coverage_fraction", "covered_seconds"} or key != "median":
                    row[f"{name}_{key}"] = result[key]
        readings = [value for run in group for value in energy[run["run"]]]
        result = stats(readings)
        row.update({f"energy_impact_{key}": result[key] for key in ["median", "min", "max"]})
        row["energy_impact_count"] = result["n"]
        row["energy_impact_scope"] = "whole_browser_verified" if result["n"] else "unavailable"
        row["energy_impact_counts_by_run"] = ";".join(f"{run['run']}:{len(energy[run['run']])}" for run in group)
        row["warnings"] = " | ".join(dict.fromkeys(warning for run in group for warning in run.get("warnings", [])))
        rows.append(row)
    return rows


def report_text(summary, environment, runs, energy, diagnostics, energy_available, map_startup_blocked=False):
    lines = ["# Local browser diagnostic", "",
             "Correction: the original Activity Monitor captures recorded only the top-level app row. Helper-process Energy Impact was not captured, and collapsing the group was not verified to aggregate it. The earlier whole-browser Energy Impact comparison is withdrawn. Whole-browser Energy Impact is unavailable from these recordings. Raw app-row values remain saved for traceability. This correction does not change the separately collected process CPU counters or memory sums, which included attributed helper processes.", "",
             "These measurements describe this machine and these local workloads. Battery consumption and battery life were not measured. Reported CPU watts are kernel CPU-energy counters divided by covered time; they exclude whole-device GPU, display, networking, storage, and battery energy.", "",
             "See [methodology](METHODOLOGY.md), [raw summary](summary.json), [environment](environment.json), and [comparison CSV](comparison.csv). Only valid completed runs appear below, including valid runs with disclosed warnings.", ""]
    if map_startup_blocked and not any(browser_key(run.get("browser")) == "mapbrowser" for run in runs):
        lines.extend(["Mapbrowser has no qualifying workload measurement yet; see the invalid runs below and [startup history](startup-blocker.json).", ""])
    hardware = environment.get("hardware", {})
    power = " ".join(str(environment.get("power", "unrecorded")).split())
    lines.extend([f"Recorded hardware: {hardware.get('chip', 'unrecorded')}, {hardware.get('memory_gib', 'unrecorded')} GiB memory. Power: {power}", ""])
    browsers = sorted({browser_key(run["browser"]) for run in runs}, key=lambda b: (ORDER.get(b, 9), b))
    versions = environment.get("versions", {})
    version_rows = []
    for browser in browsers:
        name = NAMES.get(browser, browser)
        version = next((value for key, value in versions.items() if browser_key(key) == browser), {})
        version_rows.append([name, version.get("CFBundleShortVersionString", "unrecorded"), version.get("CFBundleVersion", "unrecorded")])
    if version_rows:
        lines.extend([table(["Browser with valid results", "App version", "Build"], version_rows), ""])
    idle = [run for run in runs if run["workload"] == "synthetic-idle"]
    if idle:
        lines.extend(["Static-tab values are run means over the central 180-second window. The three 60-second subwindows are contiguous observations within one launch, not independent starts. One-tab idle precedes the untimed Speedometer warmup; ten-tab idle follows it.", ""])
        rows = []
        for run in idle:
            s = run["summary"]
            rows.append([run["run"], run["expected_tabs"], format_number(s.get("mean_phys_footprint_sum_mib"), 1),
                         format_number(s.get("mean_cpu_core_percent"), 3), format_number(s.get("mean_observed_cpu_power_watts"), 6),
                         format_number(s.get("coverage_fraction", 0) * 100, 2) + "%"])
        lines.extend([table(["Run", "Loaded static tabs", "Mean footprint MiB", "Mean CPU %", "Mean CPU W", "Counter coverage"], rows), ""])
    active = [run for run in runs if run["workload"] == "speedometer-3.1"]
    if active:
        lines.extend(["Speedometer results show the median and observed range across completed default ten-iteration trials. CPU percentages use one core as 100%. Active CPU watts are the median and range of each trial's mean over observed intervals. Whole-trial energy is not inferred or prorated across missing boundary intervals.", ""])
        rows = []
        for browser in browsers:
            group = [run for run in active if browser_key(run["browser"]) == browser]
            if not group:
                continue
            cover = [run["summary"]["coverage_fraction"] * 100 for run in group]
            seconds = [run["summary"]["covered_duration_seconds"] for run in group]
            watts = [run["summary"].get("mean_observed_cpu_power_watts") for run in group]
            rows.append([NAMES.get(browser, browser), len(group), format_stats([(run.get("score") or {}).get("score") for run in group]),
                         format_stats([run["summary"].get("mean_cpu_core_percent") for run in group]),
                         format_stats(watts, 3) + f"; n={stats(watts)['n']}",
                         f"{min(cover):.2f}% to {max(cover):.2f}%", f"{min(seconds):.3f} to {max(seconds):.3f}"])
        lines.extend([table(["Browser", "Trials", "Score median [range]", "CPU % median [range]", "CPU W median [range]", "Coverage range", "Covered seconds"], rows), ""])
    if not runs:
        lines.extend(["No valid completed runs are available.", ""])
    lines.extend(["Physical footprint is the sum of kernel process-ledger values, not summed RSS or a deduplicated measurement of all system memory. Process attribution includes descendants and services in the browser's resource coalition. Processes that start and exit between samples, and final work before a process exits, can be missed.", ""])
    suspended = [(run["run"], [str(tab["tab"]) for tab in run.get("suspended_or_unobserved_idle_tabs", [])]) for run in idle]
    for name, tabs in suspended:
        if tabs:
            lines.extend([f"{name} permits suspended or unobserved timer delivery for tabs {', '.join(tabs)}. This is a comparison of loaded static tabs under each browser's background policy; it does not establish ten actively executing JavaScript pages. See the saved postcheck and raw callbacks for the suspension evidence.", ""])
    if "mapbrowser" in browsers:
        lines.extend(["Mapbrowser used an isolated benchmark directory, but its accepted replacement idle runs followed prior Speedometer and ten-tab attempts. Its one-tab memory figure is a settled, warmed-process observation rather than a cold-start baseline. Earlier Mapbrowser recordings failed foreground checks and remain excluded. This difference in initialization history limits direct memory comparisons. See the [process memory breakdown](memory-breakdown.md) for where the recorded memory was charged.", ""])
    if "safari" in browsers:
        lines.extend(["Safari used its existing profile, with no installed extensions found in its Extensions settings. Its fresh launch showed only the Start Page, which was replaced by a benchmark page. Only test pages were loaded during measurement. The two suspended tabs in the ten-tab run were benchmark tabs 1 and 2.", ""])
    if "arc" in browsers:
        lines.extend(["Arc retained its existing signed-in profile and dormant user tabs after ignoring the separate user-data-directory argument. An extension renderer remained despite the disable-extensions flag; browser components were installed, and absence of extension activity was not established. Arc's Chromium 153 differs from Chrome and Mapbrowser's Chromium 152. These are material comparison limits.", ""])
    if any(run["run"] == "chrome-idle-2" for run in idle):
        lines.extend(["Chrome's one-tab idle includes a short Chrome Helper and Metal compiler burst. The two processes exited before their final counters could be read. The observed burst is retained; its aggregate is not a permanent idle baseline.", ""])
    lines.extend(["Hiddify VPN remained running with observed load around 30% to 45% of one core. Codex, T3, the workload server, sampler, Activity Monitor, and computer-use monitoring added background work. Browser attribution excludes unrelated processes where possible, but contention and thermal effects remain. Automatic brightness, window dimensions, caches, retained state, and fixed run order also limit comparison. Small differences from these local samples do not establish a general ranking.", ""])
    native_missing = [run["run"] for run in runs if not (run.get("native_foreground") or {}).get("verified_expected_application")]
    if native_missing:
        lines.extend(["Native foreground confirmation is unavailable for " + ", ".join(native_missing) + ". Their accepted document visibility/focus checks remain in the raw summary. Native samples for other accepted runs establish only the application observed at capture times, not continuous foreground coverage between samples.", ""])
    else:
        lines.extend(["Native foreground evidence is recorded in the raw summary. Sampling cannot exclude brief application switches between observations.", ""])
    if energy_available:
        lines.extend(["Activity Monitor Energy Impact is a separate relative score. Only readings with verified whole-browser process scope can enter this comparison. The original captures contain only app rows and are excluded. Neither the 12-hour column nor CPU watts can replace missing helper-process Energy Impact.", ""])
        am_rows = []
        for run in runs:
            values = energy[run["run"]]
            am_rows.append([run["run"], len(values), format_stats(values)])
        if am_rows and diagnostics['included_readings']:
            lines.extend([table(["Run", "Energy Impact samples", "Median [range]"], am_rows), ""])
        lines.extend([f"Energy Impact readings: {diagnostics['included_readings']} included, {diagnostics['excluded_readings']} excluded from {diagnostics['total_readings']} total. Exclusions include {diagnostics['unverified_process_scope']} without verified whole-browser scope, {diagnostics['outside_valid_windows']} outside valid windows, {diagnostics['unavailable_or_malformed']} unavailable or malformed readings, and {diagnostics['ambiguous_windows']} ambiguous matches. Missing values remain unavailable. See [raw Energy Impact readings](energy-readings.json).", ""])
    else:
        lines.extend(["Activity Monitor Energy Impact readings are unavailable in this report. CPU watts are not substituted for that metric.", ""])
    warnings = [(run["run"], "; ".join(dict.fromkeys(run.get("warnings", [])))) for run in runs if run.get("warnings")]
    if warnings:
        lines.extend(["Accepted-run warnings from the analyzer:", "", table(["Run", "Warnings"], warnings), ""])
    excluded = summary.get("excluded_run_patterns", [])
    lines.extend(["Excluded run patterns: " + (", ".join(f"`{value}`" for value in excluded) or "none") + ".", ""])
    invalid = [run for run in summary.get("runs", []) if run.get("valid") is not True
               or (run.get("summary") or {}).get("valid") is not True]
    if invalid:
        lines.extend(["Incomplete or invalid runs are omitted from comparison values:", "", table(["Run", "Reason"], [[run.get("run", "unknown"), "; ".join(run.get("invalid_reasons", []))] for run in invalid]), ""])
    lines.append("Generated from analyzer output dated " + str(summary.get("generated_at_utc", "unrecorded")) + ".")
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, required=True, help="Directory containing summary.json and environment.json")
    parser.add_argument("--output", type=Path, help="Markdown output, default INPUT/REPORT.md")
    parser.add_argument("--csv", type=Path, help="CSV output, default INPUT/comparison.csv")
    args = parser.parse_args()
    try:
        summary = json.loads((args.input / "summary.json").read_text())
        environment = json.loads((args.input / "environment.json").read_text())
        energy_path = args.input / "energy-readings.json"
        readings = json.loads(energy_path.read_text()) if energy_path.is_file() else []
        if not isinstance(readings, list):
            raise ValueError("energy-readings.json must contain an array of timestamped capture objects")
        runs = accepted_runs(summary)
        energy, diagnostics = match_energy(readings, runs)
        output, csv_path = args.output or args.input / "REPORT.md", args.csv or args.input / "comparison.csv"
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(report_text(summary, environment, runs, energy, diagnostics, energy_path.is_file(),
                                      (args.input / "startup-blocker.json").is_file()))
        csv_path.parent.mkdir(parents=True, exist_ok=True)
        with csv_path.open("w", newline="") as stream:
            writer = csv.DictWriter(stream, fieldnames=CSV_FIELDS)
            writer.writeheader()
            writer.writerows(comparison_rows(runs, energy))
        print(json.dumps({"report": str(output), "csv": str(csv_path), "valid_runs": len(runs), "energy_readings": diagnostics}))
    except (OSError, ValueError, KeyError, TypeError) as error:
        parser.exit(1, f"benchmark report: {error}\n")


if __name__ == "__main__":
    main()
