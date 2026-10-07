#!/usr/bin/env python3
"""Align benchmark callbacks with whole captured process intervals; never interpolate."""

from __future__ import annotations

import argparse
import csv
from datetime import datetime, timezone
import fnmatch
import json
import math
from pathlib import Path
import re
import statistics
from urllib.parse import parse_qs, urlsplit


COUNTERS = (
    "user_cpu_ns", "system_cpu_ns", "cpu_energy_nj", "performance_core_cpu_energy_nj",
    "package_idle_wakeups", "interrupt_wakeups", "disk_read_bytes", "disk_written_bytes",
)

BROWSER_BUNDLES = {
    "chrome": "com.google.Chrome",
    "safari": "com.apple.Safari",
    "arc": "company.thebrowser.Browser",
    "mapbrowser": "dev.mapbrowser.browser",
    "map": "dev.mapbrowser.browser",
}


def finite(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def ms_to_ns(value):
    return round(value * 1_000_000) if finite(value) else None


def unique(values):
    return list(dict.fromkeys(values))


def load_json(path, errors):
    try:
        value = json.loads(path.read_text())
        if not isinstance(value, dict):
            raise ValueError("Expected a JSON object")
        return value
    except (OSError, ValueError) as error:
        errors.append({"path": str(path), "error": str(error)})
        return None


def interval_summary(report, start_ns, end_ns, expected_completed_stop=False, manual_completed_stop=False):
    """Include deltas only when the entire counter-capture envelope is in bounds."""
    samples = report.get("samples", [])
    selected = []
    gauges = []
    excluded_boundary = 0
    malformed = []
    previous = None
    for index, sample in enumerate(samples):
        epoch = sample.get("epoch_time_ns")
        capture_start = sample.get("capture_started_epoch_ns", epoch)
        if not finite(epoch) or not finite(capture_start):
            malformed.append({"sample": index, "error": "Missing epoch/capture timestamp"})
            previous = None
            continue
        if capture_start >= start_ns and epoch <= end_ns:
            gauges.append(sample)
        delta = sample.get("interval_deltas")
        if previous is not None and delta is None and previous.get("epoch_time_ns", end_ns) < end_ns and epoch > start_ns:
            malformed.append({"sample": index, "error": "Missing interval counters"})
        if previous is not None and delta is not None:
            previous_epoch = previous.get("epoch_time_ns")
            envelope_start = previous.get("capture_started_epoch_ns", previous_epoch)
            seconds = sample.get("interval_seconds")
            if not finite(seconds) or seconds <= 0 or epoch <= previous_epoch:
                malformed.append({"sample": index, "error": "Nonpositive or unordered interval"})
            elif envelope_start >= start_ns and epoch <= end_ns:
                missing = [key for key in COUNTERS if not finite(delta.get(key)) or delta[key] < 0]
                if missing:
                    malformed.append({"sample": index, "error": "Missing or invalid interval counters", "counters": missing})
                else:
                    selected.append({"sample_index": index, "sample": sample, "seconds": seconds,
                                     "start_epoch_ns": previous_epoch, "end_epoch_ns": epoch,
                                     "capture_envelope_start_epoch_ns": envelope_start})
            elif previous_epoch < end_ns and epoch > start_ns:
                excluded_boundary += 1
        previous = sample

    covered = sum(item["seconds"] for item in selected)
    totals = {key: sum(item["sample"]["interval_deltas"][key] for item in selected) for key in COUNTERS}
    cpu_seconds = (totals["user_cpu_ns"] + totals["system_cpu_ns"]) / 1e9
    energy_available = bool(report.get("summary", {}).get("cpu_energy_counter_observed_nonzero")) or any(
        process.get("cpu_energy_nj", 0) > 0 for sample in gauges for process in sample.get("processes", [])
    )
    footprint = [sample["phys_footprint_sum_mib"] for sample in gauges if finite(sample.get("phys_footprint_sum_mib"))]
    resident = [sample["resident_sum_mib"] for sample in gauges if finite(sample.get("resident_sum_mib"))]
    errors = []
    regressions = []
    returning = []
    missing_processes = []
    late = []
    new = []
    long_intervals = []
    missing_counter_intervals = []
    expected_interval = report.get("requested_interval_seconds", 1)
    for item in selected:
        sample = item["sample"]
        sample_id = item["sample_index"]
        errors.extend({"sample": sample_id, **error} for error in sample.get("read_errors", []))
        regressions.extend({"sample": sample_id, **error} for error in sample.get("counter_regressions", []))
        for key, output in [
            ("processes_returning_after_sampling_gap", returning),
            ("previously_attributed_processes_missing", missing_processes),
            ("existing_processes_first_attributed", late),
            ("new_processes_counted_from_birth", new),
        ]:
            output.extend({"sample": sample_id, "pid": pid} for pid in sample.get(key, []))
        if finite(expected_interval) and item["seconds"] > expected_interval * 1.5:
            long_intervals.append({"sample": sample_id, "seconds": item["seconds"]})
    # Detect breaks between selected nominal intervals. Boundary clipping is separate.
    for before, after in zip(selected, selected[1:]):
        if after["sample_index"] != before["sample_index"] + 1:
            missing_counter_intervals.append({
                "after_sample": before["sample_index"], "before_sample": after["sample_index"],
                "gap_seconds": max(0, (after["start_epoch_ns"] - before["end_epoch_ns"]) / 1e9),
            })
    requested = (end_ns - start_ns) / 1e9
    issues = []
    if not selected:
        issues.append("No complete counter interval lies inside the requested bounds")
    if not gauges or not footprint:
        issues.append("No physical-footprint samples lie fully inside the requested bounds")
    if malformed:
        issues.append("Malformed timestamps or interval counters exist in the sampler data")
    warnings = []
    for values, message in [
        (errors, "Process counter reads failed"),
        (regressions, "Some process counters regressed"),
        (returning, "Returning processes can assign work from before this interval to the current interval"),
        (missing_processes, "Previously attributed processes disappeared; final work can be missed"),
        (late, "Existing processes were first attributed after the baseline"),
        (long_intervals, "Sampler intervals exceeded 1.5 times the requested interval"),
        (missing_counter_intervals, "The selected interval series has internal gaps"),
    ]:
        if values:
            warnings.append(message)
    if not energy_available:
        warnings.append("CPU-energy counter support is unconfirmed; energy is null")
    if report.get("interrupted") and manual_completed_stop:
        warnings.append("Sampler was manually stopped after static foreground completion; the controller confirms a normal flush")
    elif report.get("interrupted") and not expected_completed_stop:
        warnings.append("The sampler was interrupted without a verified completed controller stop; this window may still be covered")
    if selected:
        nominal_first = selected[0]["start_epoch_ns"]
        nominal_last = selected[-1]["end_epoch_ns"]
        leading = max(0, (nominal_first - start_ns) / 1e9)
        trailing = max(0, (end_ns - nominal_last) / 1e9)
    else:
        nominal_first = nominal_last = None
        leading = trailing = None
    # Allow normal whole-interval losses at both boundaries, but flag a truncated sample.
    boundary_allowance = 2.5 * expected_interval if finite(expected_interval) else 2.5
    if selected and (leading > boundary_allowance or trailing > boundary_allowance):
        issues.append("Sampler coverage is truncated beyond normal boundary exclusions")

    return {
        "valid": not issues,
        "status": "invalid" if issues else "valid_with_warnings" if warnings else "valid",
        "invalid_reasons": issues,
        "warnings": warnings,
        "sampler_stop": "manual_after_foreground_completion" if report.get("interrupted") and manual_completed_stop else "expected_after_completion" if report.get("interrupted") and expected_completed_stop else "unverified_interruption" if report.get("interrupted") else "natural_completion",
        "requested_start_epoch_ns": start_ns,
        "requested_end_epoch_ns": end_ns,
        "requested_duration_seconds": requested,
        "included_interval_count": len(selected),
        "included_interval_indices": [item["sample_index"] for item in selected],
        "covered_duration_seconds": covered,
        "coverage_fraction": covered / requested if requested > 0 else 0,
        "first_included_start_epoch_ns": nominal_first,
        "last_included_end_epoch_ns": nominal_last,
        "leading_uncovered_seconds": leading,
        "trailing_uncovered_seconds": trailing,
        "boundary_intervals_excluded": excluded_boundary,
        "observed_cpu_seconds": cpu_seconds if selected else None,
        "mean_cpu_core_percent": cpu_seconds / covered * 100 if covered else None,
        "cpu_energy_counter_observed_nonzero": energy_available,
        "observed_cpu_energy_joules": totals["cpu_energy_nj"] / 1e9 if energy_available and selected else None,
        "mean_observed_cpu_power_watts": totals["cpu_energy_nj"] / 1e9 / covered if energy_available and covered else None,
        "observed_performance_core_cpu_energy_joules": totals["performance_core_cpu_energy_nj"] / 1e9 if energy_available and selected else None,
        "footprint_sample_count": len(footprint),
        "mean_phys_footprint_sum_mib": statistics.mean(footprint) if footprint else None,
        "peak_phys_footprint_sum_mib": max(footprint, default=None),
        "peak_resident_sum_mib": max(resident, default=None),
        "observed_package_idle_wakeups": totals["package_idle_wakeups"] if selected else None,
        "observed_interrupt_wakeups": totals["interrupt_wakeups"] if selected else None,
        "total_interval_deltas": totals,
        "diagnostics": {
            "read_errors": errors, "counter_regressions": regressions,
            "processes_returning_after_sampling_gap": returning,
            "previously_attributed_processes_missing": missing_processes,
            "existing_processes_first_attributed": late,
            "new_processes_counted_from_birth": new,
            "long_intervals": long_intervals, "internal_gaps": missing_counter_intervals,
            "malformed_samples": malformed,
            "coalition_lookup_failure_count": sum(item["sample"].get("coalition_lookup_failures", 0) for item in selected),
        },
    }


def select_metrics(run, browser, bounds, sources):
    phase = re.sub(r"-\d+$", "", run)
    options = []
    for path, report in sources:
        name = path.name.removesuffix("-metrics.json")
        if not (name == run or name == phase or name.startswith(browser + "-")):
            continue
        samples = report.get("samples", [])
        times = [sample.get("epoch_time_ns") for sample in samples if finite(sample.get("epoch_time_ns"))]
        if not times:
            overlap = 0
        else:
            overlap = max(0, min(max(times), bounds[1]) - max(min(times), bounds[0]))
        exactness = 2 if name == run else 1 if name == phase else 0
        if exactness or overlap:
            options.append((exactness, overlap, str(path), path, report))
    return max(options, key=lambda item: item[:3])[3:] if options else (None, None)


def tab_state(completed, started, at_ms):
    state = completed.get("start_visibility", started.get("visibility"))
    focused = completed.get("start_focused", started.get("focused"))
    for event in sorted(completed.get("visibility_events", []), key=lambda event: event.get("epoch_ms", 0)):
        if event.get("epoch_ms", math.inf) > at_ms:
            break
        state = event.get("state", state)
        focused = event.get("focused", focused)
    return {"visibility": state, "focused": focused}


def verify_controller_stop(controller, run, metrics_path, callbacks_path, completed_tabs, last_finished_ms, allow_partial_idle=False):
    reasons = []
    accepted_results = {"completed", "completed_with_suspended_idle_tabs", "interrupted"} if allow_partial_idle else {"completed"}
    if controller.get("run") != run or controller.get("result") not in accepted_results:
        reasons.append("Controller does not confirm this run completed")
    if controller.get("sampler_returncode") != 0 or controller.get("sampler_forced_termination") is not False:
        reasons.append("Controller does not confirm a normal sampler flush")
    if controller.get("errors"):
        reasons.append("Controller reported errors")
    recorded_tabs = set(map(str, controller.get("completed_tabs", [])))
    tabs_match = set(completed_tabs) <= recorded_tabs if allow_partial_idle else recorded_tabs == set(completed_tabs)
    if not tabs_match:
        reasons.append("Controller completion tab IDs do not match the callbacks")
    for field, expected in [("metrics_path", metrics_path), ("status_path", callbacks_path)]:
        value = controller.get(field)
        if not isinstance(value, str) or Path(value).resolve() != expected.resolve():
            reasons.append(f"Controller {field} does not match this run's data")
    try:
        finished = datetime.fromisoformat(controller["controller_finished_at_utc"].replace("Z", "+00:00"))
        if finished.tzinfo is None or finished.timestamp() * 1000 < last_finished_ms:
            reasons.append("Controller stop predates workload completion or lacks a timezone")
    except (KeyError, ValueError, TypeError, AttributeError):
        reasons.append("Controller finish timestamp is missing or invalid")
    return reasons


def native_foreground_evidence(report, browser, start_ns, end_ns):
    expected = BROWSER_BUNDLES.get(browser.lower())
    observations = []
    unknown = []
    for index, sample in enumerate(report.get("samples", [])):
        foreground = sample.get("frontmost_application")
        if isinstance(foreground, dict) and finite(foreground.get("captured_at_epoch_ns")):
            timestamp = foreground["captured_at_epoch_ns"]
            if not start_ns <= timestamp <= end_ns:
                continue
            observation = {"sample": index, **foreground}
            if foreground.get("error") or not foreground.get("bundle_identifier"):
                unknown.append(observation)
            else:
                observations.append(observation)
        elif start_ns <= sample.get("epoch_time_ns", -math.inf) <= end_ns:
            unknown.append({"sample": index, "error": "Native foreground sample is missing or has no timestamp"})
    mismatches = [item for item in observations if item["bundle_identifier"] != expected]
    times = sorted(item["captured_at_epoch_ns"] for item in observations)
    return {
        "expected_bundle_identifier": expected,
        "verified_expected_application": bool(expected and observations and not mismatches and not unknown),
        "observation_count": len(observations), "unknown_count": len(unknown),
        "mismatch_count": len(mismatches), "mismatches": mismatches, "unknown": unknown,
        "observed_bundle_identifiers": sorted({item["bundle_identifier"] for item in observations}),
        "first_observation_epoch_ns": times[0] if times else None,
        "last_observation_epoch_ns": times[-1] if times else None,
        "max_observation_gap_seconds": max(((after - before) / 1e9 for before, after in zip(times, times[1:])), default=None),
        "limitation": "Native foreground samples show the frontmost application at capture times; brief switches between samples can remain unobserved.",
    }


def analyze_run(path, status, sources, allow_suspended_idle_tabs=False):
    run = status.get("run") or path.parent.name
    events = status.get("events", [])
    errors = []
    warnings = []
    if not events:
        errors.append("Run contains no callback events")
    browser = next((event.get("browser") for event in events if event.get("browser")), run.split("-")[0])
    workload = next((event.get("workload") for event in events if event.get("workload")), "unknown")
    speedometer = workload == "speedometer-3.1"
    allow_static = allow_suspended_idle_tabs and workload == "synthetic-idle"
    scheduled_start = status.get("start_epoch_ms")
    expected_match = re.search(r"(?:idle|mixed|canvas)(\d+)(?:-|$)", run)
    expected_tabs = 1 if speedometer else int(expected_match.group(1)) if expected_match else 1
    by_tab = {}
    for event in events:
        by_tab.setdefault(str(event.get("tab", "1")), {}).setdefault(event.get("event"), []).append(event)
        if event.get("event") == "error":
            errors.append("Workload error: " + str(event.get("message", "unspecified")))
    if len(by_tab) != expected_tabs:
        errors.append(f"Expected {expected_tabs} tabs, observed {len(by_tab)} distinct callback tab IDs")
    tabs = []
    for tab, kinds in sorted(by_tab.items()):
        for kind in ("ready", "started", "completed"):
            count = len(kinds.get(kind, []))
            if count > 1 or (count != 1 and (not allow_static or kind == "ready")):
                errors.append(f"Tab {tab} has {len(kinds.get(kind, []))} {kind} events; expected one")
        ready = kinds.get("ready", [{}])[-1]
        started = kinds.get("started", [{}])[-1]
        completed = kinds.get("completed", [{}])[-1]
        if ready.get("browser", browser) != browser or completed.get("browser", browser) != browser:
            errors.append(f"Tab {tab} browser label changed")
        actual_start = completed.get("started_epoch_ms")
        actual_finish = completed.get("finished_epoch_ms")
        if (not finite(actual_start) or not finite(actual_finish) or actual_finish <= actual_start) and (not allow_static or completed):
            errors.append(f"Tab {tab} lacks valid actual start/finish timestamps")
        if not speedometer and completed.get("requested_duration_seconds", ready.get("requested_duration_seconds")) != 210:
            errors.append(f"Tab {tab} was not configured for the 210-second steady-state protocol")
        if allow_static:
            ready_time = ready.get("server_epoch_ms", ready.get("epoch_ms"))
            if ready.get("mode") != "idle" or ready.get("workload") != "synthetic-idle":
                errors.append(f"Tab {tab} is not the static idle fixture")
            if not finite(scheduled_start) or not finite(ready_time) or ready_time > scheduled_start:
                errors.append(f"Tab {tab} was not confirmed ready before the scheduled start")
        tabs.append({"tab": tab, "ready": ready, "started": started, "completed": completed,
                     "actual_start_epoch_ms": actual_start, "actual_finish_epoch_ms": actual_finish})
    valid_times = [tab for tab in tabs if finite(tab["actual_start_epoch_ms"]) and finite(tab["actual_finish_epoch_ms"])]
    bounds = None
    score = None
    completion_focus_needs_native = False
    idle_foreground = None
    unobserved_idle_tabs = []
    if allow_static:
        candidates = [tab for tab in valid_times
                      if tab["completed"].get("start_visibility") == "visible"
                      and tab["completed"].get("start_focused") is True
                      and finite(scheduled_start)
                      and scheduled_start <= tab["actual_start_epoch_ms"] <= scheduled_start + 15000]
        if len(candidates) != 1:
            errors.append(f"Expected one completed foreground idle tab starting near the shared start, observed {len(candidates)}")
        else:
            idle_foreground = candidates[0]
            if not idle_foreground["started"]:
                errors.append("The foreground idle tab lacks its started callback")
            actual_start = idle_foreground["actual_start_epoch_ms"]
            bounds = (ms_to_ns(actual_start + 15000), ms_to_ns(actual_start + 195000))
            if idle_foreground["actual_finish_epoch_ms"] < actual_start + 195000:
                errors.append("The foreground tab finished before the full steady-state window ended")
    elif valid_times:
        actual_start = max(tab["actual_start_epoch_ms"] for tab in valid_times)
        actual_finish = min(tab["actual_finish_epoch_ms"] for tab in valid_times)
        if speedometer:
            bounds = (ms_to_ns(actual_start), ms_to_ns(actual_finish))
        else:
            bounds = (ms_to_ns(actual_start + 15000), ms_to_ns(actual_start + 195000))
            if actual_finish < actual_start + 195000:
                errors.append("At least one tab finished before the full steady-state window ended")
        if bounds[1] <= bounds[0]:
            errors.append("Run has empty or reversed measurement bounds")
            bounds = None
    else:
        errors.append("No completed workload provides measurement bounds")

    if speedometer and tabs:
        tab = tabs[0]
        completed = tab["completed"]
        official = completed.get("metrics", {})
        score_metric = official.get("Score", {})
        score_values = score_metric.get("values", [])
        score = {
            "score": score_metric.get("mean"), "score_delta": score_metric.get("delta"),
            "score_percent_delta": score_metric.get("percentDelta"), "score_values": score_values,
            "geomean_ms": official.get("Geomean", {}).get("mean"),
            "displayed_score": completed.get("displayed_score"),
            "iterations": completed.get("iterations"), "official_valid": completed.get("valid"),
            "source_commit": tab["ready"].get("source_commit"),
        }
        if completed.get("iterations") != 10 or len(score_values) != 10:
            errors.append("Speedometer must report ten iterations and ten official Score values")
        if completed.get("valid") is not True:
            errors.append("Speedometer did not report a valid official score")
        if not finite(score["score"]) or score["score"] <= 0 or any(not finite(value) or value <= 0 for value in score_values):
            errors.append("Speedometer score or iteration values are missing/nonpositive")
        if not score_metric:
            errors.append("Full official Score metrics are missing")
        if not isinstance(completed.get("visibility_events"), list):
            errors.append("Speedometer visibility/focus history is missing")
        custom = set(parse_qs(urlsplit(completed.get("location", "")).query, keep_blank_values=True)) & {
            "suite", "suites", "tags", "viewport", "useWarmupSuite", "waitBeforeSync",
            "warmupBeforeSync", "measurementMethod", "shuffleSeed", "developerMode",
        }
        if custom:
            errors.append("Speedometer URL changes the standard workload: " + ", ".join(sorted(custom)))
        for event_name, event in [("started", tab["started"]), ("completed", completed)]:
            if event.get("visibility") != "visible":
                errors.append(f"Speedometer was not visible at {event_name}")
            if event_name == "started" and event.get("focused") is not True:
                errors.append("Speedometer was not focused at the start")
            if event_name == "completed":
                if event.get("focused") is False:
                    completion_focus_needs_native = True
                elif event.get("focused") is not True:
                    errors.append("Speedometer completion focus metadata is missing")
            viewport = event.get("viewport", {})
            if viewport.get("width", 0) < 850 or viewport.get("height", 0) < 650:
                errors.append(f"Speedometer viewport was below 850×650 at {event_name}")
        focus_history = completed.get("visibility_events") if isinstance(completed.get("visibility_events"), list) else []
        focus_start = tab["actual_start_epoch_ms"] if finite(tab["actual_start_epoch_ms"]) else -math.inf
        focus_end = tab["actual_finish_epoch_ms"] if finite(tab["actual_finish_epoch_ms"]) else math.inf
        measured_focus_events = [event for event in focus_history
                                 if not finite(event.get("epoch_ms")) or focus_start <= event["epoch_ms"] <= focus_end]
        if any(event.get("state") != "visible" for event in measured_focus_events):
            errors.append("Speedometer lost visibility during the measured run")
        if any(event.get("focused") is not True for event in measured_focus_events):
            errors.append("Speedometer reported document focus loss during the measured run")
        if any(event.get("event") == "blur" and event.get("focused") is True for event in measured_focus_events):
            warnings.append("A parent-window blur retained document focus, consistent with an iframe focus transition")
    elif bounds and tabs:
        start_ms, end_ms = bounds[0] / 1e6, bounds[1] / 1e6
        foreground = []
        for tab in tabs:
            if allow_static and tab is not idle_foreground:
                # Ready proves the static document rendered. Missing or late
                # timer callbacks do not prove it was unloaded or kept running.
                started_at = tab["started"].get("epoch_ms")
                completed_at = tab["completed"].get("epoch_ms")
                changes = [event for event in tab["completed"].get("visibility_events", [])
                           if start_ms <= event.get("epoch_ms", -math.inf) <= end_ms]
                if changes:
                    errors.append(f"Background tab {tab['tab']} focus/visibility changed during steady-state measurement")
                for event in [tab["started"], tab["completed"]]:
                    timestamp = event.get("epoch_ms")
                    if finite(timestamp) and start_ms <= timestamp <= end_ms and event.get("focused") is True:
                        errors.append(f"Background tab {tab['tab']} became focused during steady-state measurement")
                if finite(started_at) and started_at <= start_ms:
                    state = tab_state(tab["completed"], tab["started"], start_ms)
                    tab["last_reported_state_before_window"] = state
                    if state == {"visibility": "visible", "focused": True}:
                        errors.append(f"Background tab {tab['tab']} was also foreground at steady-state start")
                else:
                    tab["last_reported_state_before_window"] = {
                        "visibility": tab["ready"].get("visibility"), "focused": tab["ready"].get("focused"),
                    }
                if not finite(started_at) or started_at > end_ms or not tab["completed"]:
                    diagnostic = {
                        "tab": tab["tab"], "classification": "suspended_or_unobserved_timer_delivery",
                        "ready_epoch_ms": tab["ready"].get("epoch_ms"),
                        "started_callback_epoch_ms": started_at, "completed_callback_epoch_ms": completed_at,
                        "late_start_after_window": finite(started_at) and started_at > end_ms,
                        "scope": "Loaded static document; active background JavaScript execution is not established",
                    }
                    unobserved_idle_tabs.append(diagnostic)
                    tab["idle_timer_delivery"] = diagnostic
                continue
            tab["steady_start_state"] = tab_state(tab["completed"], tab["started"], start_ms)
            tab["steady_end_state"] = tab_state(tab["completed"], tab["started"], end_ms)
            if tab["steady_start_state"] == {"visibility": "visible", "focused": True}:
                foreground.append(tab["tab"])
            changes = [event for event in tab["completed"].get("visibility_events", [])
                       if start_ms <= event.get("epoch_ms", -math.inf) <= end_ms]
            if changes:
                errors.append(f"Tab {tab['tab']} focus/visibility changed during steady-state measurement")
            if tab["steady_start_state"] != tab["steady_end_state"]:
                errors.append(f"Tab {tab['tab']} start/end visibility states differ")
        if len(foreground) != 1:
            errors.append(f"Expected one focused visible foreground tab, observed {len(foreground)}")
        if allow_static:
            warnings.append("Opt-in static-tab protocol anchors the window to the foreground tab and permits suspended background timer delivery; it does not establish active background execution or battery life")
            if unobserved_idle_tabs:
                warnings.append("Background timer delivery was suspended or unobserved for tabs " + ", ".join(item["tab"] for item in unobserved_idle_tabs))

    metrics_path = None
    metrics = None
    summary = None
    windows = []
    controller_stop = None
    native_foreground = None
    postcheck_path = None
    if bounds:
        metrics_path, metrics = select_metrics(run, browser, bounds, sources)
        if metrics is None:
            errors.append("No matching process metrics file covers this run")
            if completion_focus_needs_native:
                errors.append("Completion document focus was false and native foreground confirmation is missing")
        else:
            native_foreground = native_foreground_evidence(metrics, browser, *bounds)
            if native_foreground["mismatch_count"]:
                errors.append("Native foreground sampling observed a different application during the measured workload")
            if speedometer:
                if completion_focus_needs_native:
                    if native_foreground["verified_expected_application"]:
                        warnings.append("Completion document focus was false, consistent with iframe removal; every observed native foreground sample matched the browser. Brief switches between samples remain possible")
                    else:
                        errors.append("Completion document focus was false without complete matching native foreground observations")
            expected_stop = False
            manual_stop = False
            controller_path = metrics_path.parent / f"{run}-controller.json"
            if metrics.get("interrupted"):
                verification_errors = []
                if controller_path.is_file():
                    controller = load_json(controller_path, verification_errors)
                    if controller is not None:
                        controller_tabs = [idle_foreground["tab"]] if allow_static and idle_foreground else [tab["tab"] for tab in valid_times]
                        controller_finish = idle_foreground["actual_finish_epoch_ms"] if allow_static and idle_foreground else max((tab["actual_finish_epoch_ms"] for tab in valid_times), default=math.inf)
                        verification_errors.extend(verify_controller_stop(
                            controller, run, metrics_path, path,
                            controller_tabs, controller_finish,
                            allow_partial_idle=bool(allow_static and idle_foreground),
                        ))
                        manual_stop = not verification_errors and controller.get("result") == "interrupted"
                        expected_stop = not verification_errors and not manual_stop
                else:
                    verification_errors.append("No controller completion report is available")
                controller_stop = {"path": str(controller_path), "verified_expected_stop": expected_stop,
                                   "verified_manual_stop_after_foreground_completion": manual_stop,
                                   "verification_errors": verification_errors}
            summary = interval_summary(metrics, *bounds, expected_completed_stop=expected_stop, manual_completed_stop=manual_stop)
            errors.extend(summary["invalid_reasons"])
            warnings.extend(summary["warnings"])
            if not speedometer:
                for index in range(3):
                    start = bounds[0] + index * 60_000_000_000
                    window = interval_summary(metrics, start, start + 60_000_000_000, expected_completed_stop=expected_stop, manual_completed_stop=manual_stop)
                    window["window"] = index + 1
                    windows.append(window)
                    if not window["valid"]:
                        errors.append(f"Steady-state window {index + 1} is invalid")
            if allow_static:
                for name in [f"{run}-postcheck.json", re.sub(r"-\d+$", "", run) + "-postcheck.json"]:
                    candidate = metrics_path.parent / name
                    if candidate.is_file():
                        postcheck_path = str(candidate)
                        break
    if any(tab["completed"].get("mode") == "mixed" for tab in tabs):
        warnings.append("Compare delivered tick counts; normal background throttling can change completed work")
    return {
        "run": run, "browser": browser, "workload": workload, "expected_tabs": expected_tabs,
        "valid": not errors, "invalid_reasons": unique(errors), "warnings": unique(warnings),
        "status": "invalid" if errors else "valid_with_warnings" if warnings else "valid",
        "callbacks_path": str(path), "metrics_path": str(metrics_path) if metrics_path else None,
        "score": score, "summary": summary, "windows": windows,
        "controller_stop": controller_stop,
        "native_foreground": native_foreground,
        "allow_suspended_idle_tabs": allow_static,
        "idle_foreground_tab": idle_foreground["tab"] if idle_foreground else None,
        "suspended_or_unobserved_idle_tabs": unobserved_idle_tabs,
        "background_postcheck_path": postcheck_path,
        "comparison_scope": "Loaded static tabs with native background policy; not an active-background or battery-life test" if allow_static else "All configured workload callbacks required",
        "sampler_attribution_notes": metrics.get("attribution_notes", []) if metrics else [],
        "sampler_root_pids": metrics.get("root_pids", []) if metrics else [],
        "sampler_accepted_resource_coalitions": metrics.get("accepted_root_resource_coalitions", {}) if metrics else {},
        "tabs": [{key: value for key, value in tab.items() if key not in {"ready", "completed"}} | {
            "ready": {key: value for key, value in tab["ready"].items() if key != "metrics"},
            "completed": {key: value for key, value in tab["completed"].items() if key != "metrics"},
        } for tab in tabs],
    }


def group_scores(runs):
    groups = []
    browsers = sorted({run["browser"] for run in runs if run["workload"] == "speedometer-3.1"})
    for browser in browsers:
        candidates = [run for run in runs if run["browser"] == browser and run["workload"] == "speedometer-3.1"]
        accepted = [run for run in candidates if run["valid"] and run["score"] and finite(run["score"]["score"])]
        values = [run["score"]["score"] for run in accepted]
        groups.append({
            "browser": browser, "valid_run_count": len(accepted), "total_run_count": len(candidates),
            "run_ids": [run["run"] for run in accepted], "score_values": values,
            "score_arithmetic_mean": statistics.mean(values) if values else None,
            "score_geometric_mean": math.exp(statistics.mean(math.log(value) for value in values)) if values else None,
            "score_sample_standard_deviation": statistics.stdev(values) if len(values) > 1 else None,
            "score_min": min(values, default=None), "score_max": max(values, default=None),
        })
    return groups


CSV_FIELDS = [
    "run", "browser", "workload", "window", "valid", "invalid_reasons", "warnings", "score",
    "allow_suspended_idle_tabs", "idle_foreground_tab", "suspended_idle_tab_count", "comparison_scope", "sampler_stop",
    "requested_duration_seconds", "covered_duration_seconds", "coverage_fraction", "included_interval_count",
    "observed_cpu_seconds", "mean_cpu_core_percent", "observed_cpu_energy_joules", "mean_observed_cpu_power_watts",
    "mean_phys_footprint_sum_mib", "peak_phys_footprint_sum_mib", "peak_resident_sum_mib",
    "observed_package_idle_wakeups", "observed_interrupt_wakeups", "read_error_count", "counter_regression_count",
    "returning_process_count", "missing_process_count", "late_attributed_process_count", "internal_gap_count",
    "metrics_path", "callbacks_path",
]


def write_csv(path, runs):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=CSV_FIELDS)
        writer.writeheader()
        for run in runs:
            for window in [run.get("summary") or {}, *run.get("windows", [])]:
                row = {key: run.get(key) for key in ("run", "browser", "workload", "metrics_path", "callbacks_path", "allow_suspended_idle_tabs", "idle_foreground_tab", "comparison_scope")}
                row["suspended_idle_tab_count"] = len(run.get("suspended_or_unobserved_idle_tabs", []))
                row.update({key: window.get(key) for key in CSV_FIELDS if key in window})
                row["window"] = window.get("window", "full")
                row["valid"] = run["valid"] and window.get("valid", False)
                row["invalid_reasons"] = " | ".join(unique(run["invalid_reasons"] + window.get("invalid_reasons", [])))
                row["warnings"] = " | ".join(unique(run.get("warnings", []) + window.get("warnings", [])))
                row["score"] = (run.get("score") or {}).get("score")
                diagnostics = window.get("diagnostics", {})
                for output, key in [
                    ("read_error_count", "read_errors"), ("counter_regression_count", "counter_regressions"),
                    ("returning_process_count", "processes_returning_after_sampling_gap"),
                    ("missing_process_count", "previously_attributed_processes_missing"),
                    ("late_attributed_process_count", "existing_processes_first_attributed"),
                    ("internal_gap_count", "internal_gaps"),
                ]:
                    row[output] = len(diagnostics.get(key, []))
                writer.writerow(row)


def analyze(directory, excludes=(), allow_suspended_idle_tabs=False):
    read_errors = []
    sources = []
    for path in sorted(directory.glob("*-metrics.json")):
        if any(fnmatch.fnmatch(path.name.removesuffix("-metrics.json"), pattern) for pattern in excludes):
            continue
        report = load_json(path, read_errors)
        if report is not None:
            sources.append((path, report))
    runs = []
    for path in sorted((directory / "workloads").glob("*/status.json")):
        if any(fnmatch.fnmatch(path.parent.name, pattern) for pattern in excludes):
            continue
        status = load_json(path, read_errors)
        if status is not None:
            runs.append(analyze_run(path, status, sources, allow_suspended_idle_tabs=allow_suspended_idle_tabs))
    used = {run["metrics_path"] for run in runs if run["metrics_path"]}
    orphaned = [{"path": str(path), "valid": False, "invalid_reasons": ["No completed callback run was matched to this metrics file"]}
                for path, _ in sources if str(path) not in used]
    return {
        "schema_version": 1,
        "generated_at_utc": datetime.now(timezone.utc).isoformat(),
        "input_directory": str(directory.resolve()),
        "excluded_run_patterns": list(excludes),
        "allow_suspended_idle_tabs": allow_suspended_idle_tabs,
        "valid": bool(runs) and all(run["valid"] for run in runs) and not read_errors and not orphaned,
        "run_count": len(runs), "valid_run_count": sum(run["valid"] for run in runs),
        "read_errors": read_errors, "orphaned_metrics": orphaned,
        "runs": runs, "speedometer_groups": group_scores(runs),
        "method": {
            "steady_state": "For 210-second synthetic runs, use latest actual tab start +15s through +195s; report one 180s aggregate and three consecutive 60s windows.",
            "suspended_idle_opt_in": "When explicitly enabled for static idle only, use the completed stable foreground tab's actual start +15s through +195s, require every expected document ready before shared start, and retain missing/late background callbacks as suspended or unobserved timer delivery. Late post-measurement starts never extend the bounds. This compares loaded static documents under browser background policies, not active JavaScript execution or battery life.",
            "speedometer": "Use actual start/finish timestamps and require ten official iterations, valid score, focus and visibility.",
            "interval_selection": "An interval is included only if the previous sample's capture start and the current sample's capture end both lie within the requested window. No interpolation or prorating is performed.",
            "coverage": "Covered duration sums included monotonic interval durations. Whole boundary intervals are excluded; full-window totals can exceed the sum of subwindows because intervals crossing internal boundaries are omitted from subwindows.",
            "memory": "Footprint mean is the arithmetic mean of complete point captures inside the requested bounds; peak is the maximum of those point captures.",
            "energy": "Reported process CPU energy only, excluding whole-device GPU/display/network/storage/battery energy. Performance-core energy is a subset and is not added to CPU energy.",
            "gaps": "Sampler lifetime-counter catch-up, process read errors, disappearances, late attribution and counter regressions remain explicit diagnostics. They can undercount work or assign work from an earlier interval; no estimates repair them.",
            "repeat_interpretation": "Three steady-state windows from one browser run are consecutive observations, not three independent browser launches.",
        },
    }


def self_test():
    import unittest

    class AlignmentTests(unittest.TestCase):
        def report(self):
            samples = []
            for second in range(8):
                samples.append({
                    "epoch_time_ns": second * 1_000_000_000 + 100_000_000,
                    "capture_started_epoch_ns": second * 1_000_000_000,
                    "interval_seconds": 1,
                    "interval_deltas": {key: 1_000_000_000 for key in COUNTERS} if second else None,
                    "phys_footprint_sum_mib": 100 + second,
                    "resident_sum_mib": 200 + second,
                    "processes": [{"cpu_energy_nj": 1}],
                })
            return {"samples": samples, "requested_interval_seconds": 1}

        def test_whole_envelopes_only_and_unit_conversion(self):
            result = interval_summary(self.report(), 1_050_000_000, 6_050_000_000)
            self.assertEqual(result["included_interval_indices"], [3, 4, 5])
            self.assertEqual(result["covered_duration_seconds"], 3)
            self.assertEqual(result["observed_cpu_energy_joules"], 3)
            self.assertEqual(result["observed_cpu_seconds"], 6)
            self.assertEqual(result["mean_phys_footprint_sum_mib"], 103.5)
            self.assertEqual(result["peak_phys_footprint_sum_mib"], 105)

        def test_empty_bounds_and_process_gaps_are_visible(self):
            report = self.report()
            report["samples"][3]["read_errors"] = [{"pid": 7, "error": "gone"}]
            report["samples"][4]["processes_returning_after_sampling_gap"] = [7]
            result = interval_summary(report, 1_050_000_000, 6_050_000_000)
            self.assertEqual(len(result["diagnostics"]["read_errors"]), 1)
            self.assertEqual(len(result["diagnostics"]["processes_returning_after_sampling_gap"]), 1)
            self.assertGreaterEqual(len(result["warnings"]), 2)
            self.assertFalse(interval_summary(report, 10_000_000_000, 11_000_000_000)["valid"])

        def test_missing_callbacks_are_invalid(self):
            result = analyze_run(Path("workloads/chrome-idle-1/status.json"), {"run": "chrome-idle-1", "events": []}, [])
            self.assertFalse(result["valid"])
            self.assertIsNone(result["summary"])

        def test_speedometer_requires_standard_iterations_and_focus(self):
            metadata = {"run": "chrome-speed-1", "browser": "chrome", "workload": "speedometer-3.1", "tab": "1",
                        "visibility": "visible", "focused": True, "viewport": {"width": 1000, "height": 800}}
            completed = {**metadata, "event": "completed", "started_epoch_ms": 1050, "finished_epoch_ms": 6050,
                         "iterations": 10, "valid": True, "visibility_events": [],
                         "metrics": {"Score": {"mean": 50, "values": [50] * 10}, "Geomean": {"mean": 20}}}
            status = {"run": "chrome-speed-1", "events": [
                {**metadata, "event": "ready"}, {**metadata, "event": "started"}, completed,
            ]}
            sources = [(Path("chrome-speed-1-metrics.json"), self.report())]
            good = analyze_run(Path("workloads/chrome-speed-1/status.json"), status, sources)
            self.assertTrue(good["valid"], good["invalid_reasons"])
            self.assertEqual(good["score"]["geomean_ms"], 20)
            completed["visibility_events"].append({"event": "blur", "state": "visible", "focused": False})
            self.assertFalse(analyze_run(Path("status.json"), status, sources)["valid"])
            completed["visibility_events"] = []
            completed["iterations"] = 1
            self.assertFalse(analyze_run(Path("status.json"), status, sources)["valid"])

        def test_expected_controller_sigint_is_distinct_from_unplanned_interruption(self):
            metrics = Path("chrome-speed-1-metrics.json").resolve()
            callbacks = Path("workloads/chrome-speed-1/status.json").resolve()
            controller = {
                "run": "chrome-speed-1", "result": "completed", "sampler_returncode": 0,
                "sampler_forced_termination": False, "errors": [], "completed_tabs": ["1"],
                "metrics_path": str(metrics), "status_path": str(callbacks),
                "controller_finished_at_utc": "1970-01-01T00:00:07+00:00",
            }
            self.assertEqual(verify_controller_stop(controller, "chrome-speed-1", metrics, callbacks, ["1"], 6050), [])
            report = self.report()
            report["interrupted"] = True
            expected = interval_summary(report, 1_050_000_000, 6_050_000_000, expected_completed_stop=True)
            self.assertEqual(expected["sampler_stop"], "expected_after_completion")
            self.assertFalse(any("interrupted" in warning for warning in expected["warnings"]))
            unplanned = interval_summary(report, 1_050_000_000, 6_050_000_000)
            self.assertEqual(unplanned["sampler_stop"], "unverified_interruption")
            self.assertTrue(any("interrupted" in warning for warning in unplanned["warnings"]))
            controller["sampler_forced_termination"] = True
            self.assertTrue(verify_controller_stop(controller, "chrome-speed-1", metrics, callbacks, ["1"], 6050))

        def test_iframe_focus_exception_requires_matching_native_observations(self):
            metadata = {"run": "chrome-speed-1", "browser": "chrome", "workload": "speedometer-3.1", "tab": "1",
                        "visibility": "visible", "focused": True, "viewport": {"width": 1000, "height": 800}}
            completed = {**metadata, "event": "completed", "focused": False,
                         "started_epoch_ms": 1050, "finished_epoch_ms": 6050, "iterations": 10, "valid": True,
                         "visibility_events": [{"event": "blur", "epoch_ms": 1100, "state": "visible", "focused": True}],
                         "metrics": {"Score": {"mean": 50, "values": [50] * 10}}}
            status = {"run": "chrome-speed-1", "events": [
                {**metadata, "event": "ready"}, {**metadata, "event": "started"}, completed,
            ]}
            report = self.report()
            sources = [(Path("chrome-speed-1-metrics.json"), report)]
            path = Path("workloads/chrome-speed-1/status.json")
            self.assertFalse(analyze_run(path, status, sources)["valid"])
            for sample in report["samples"]:
                sample["frontmost_application"] = {
                    "source": "NSWorkspace.sharedWorkspace.frontmostApplication",
                    "captured_at_epoch_ns": sample["epoch_time_ns"],
                    "bundle_identifier": "com.google.Chrome", "error": None,
                }
            accepted = analyze_run(path, status, sources)
            self.assertTrue(accepted["valid"], accepted["invalid_reasons"])
            self.assertTrue(accepted["native_foreground"]["verified_expected_application"])
            report["samples"][3]["frontmost_application"]["bundle_identifier"] = "com.apple.finder"
            self.assertFalse(analyze_run(path, status, sources)["valid"])
            report["samples"][3]["frontmost_application"]["bundle_identifier"] = "com.google.Chrome"
            completed["visibility_events"].append({"event": "visibilitychange", "epoch_ms": 1200, "state": "hidden", "focused": False})
            self.assertFalse(analyze_run(path, status, sources)["valid"])

        def test_suspended_static_idle_requires_opt_in_and_keeps_foreground_bounds(self):
            metadata = {"run": "safari-idle2-1", "browser": "safari", "workload": "synthetic-idle", "mode": "idle", "requested_duration_seconds": 210}
            events = [
                {**metadata, "tab": "1", "event": "ready", "epoch_ms": 900, "server_epoch_ms": 900, "visibility": "hidden", "focused": False},
                {**metadata, "tab": "2", "event": "ready", "epoch_ms": 900, "server_epoch_ms": 900, "visibility": "visible", "focused": True},
                {**metadata, "tab": "2", "event": "started", "epoch_ms": 1004, "visibility": "visible", "focused": True},
                {**metadata, "tab": "2", "event": "completed", "epoch_ms": 211005, "started_epoch_ms": 1005, "finished_epoch_ms": 211005,
                 "start_visibility": "visible", "start_focused": True, "visibility": "visible", "focused": True, "visibility_events": []},
                {**metadata, "tab": "1", "event": "started", "epoch_ms": 330000, "visibility": "visible", "focused": True},
            ]
            status = {"run": "safari-idle2-1", "start_epoch_ms": 1000, "events": events}
            report = self.report()
            sample_template = report["samples"][1]
            report["samples"] = [{**sample_template, "epoch_time_ns": second * 1_000_000_000 + 100_000_000,
                                  "capture_started_epoch_ns": second * 1_000_000_000,
                                  "interval_deltas": sample_template["interval_deltas"] if second else None}
                                 for second in range(213)]
            sources = [(Path("safari-idle2-1-metrics.json"), report)]
            path = Path("workloads/safari-idle2-1/status.json")
            self.assertFalse(analyze_run(path, status, sources)["valid"])
            accepted = analyze_run(path, status, sources, allow_suspended_idle_tabs=True)
            self.assertTrue(accepted["valid"], accepted["invalid_reasons"])
            self.assertEqual(accepted["idle_foreground_tab"], "2")
            self.assertEqual(accepted["summary"]["requested_start_epoch_ns"], 16_005_000_000)
            self.assertEqual(accepted["summary"]["requested_end_epoch_ns"], 196_005_000_000)
            self.assertEqual(accepted["suspended_or_unobserved_idle_tabs"][0]["tab"], "1")
            self.assertTrue(accepted["suspended_or_unobserved_idle_tabs"][0]["late_start_after_window"])
            sample = report["samples"][50]
            sample["frontmost_application"] = {
                "captured_at_epoch_ns": sample["epoch_time_ns"],
                "bundle_identifier": "com.apple.finder", "error": None,
            }
            rejected = analyze_run(path, status, sources, allow_suspended_idle_tabs=True)
            self.assertFalse(rejected["valid"])
            self.assertTrue(any("Native foreground" in reason for reason in rejected["invalid_reasons"]))
            del sample["frontmost_application"]
            events[-1]["epoch_ms"] = 50000
            self.assertFalse(analyze_run(path, status, sources, allow_suspended_idle_tabs=True)["valid"])
            events[-1]["epoch_ms"] = 330000
            events[0]["server_epoch_ms"] = 2000
            self.assertFalse(analyze_run(path, status, sources, allow_suspended_idle_tabs=True)["valid"])
            events[0]["server_epoch_ms"] = 900
            for event in events:
                event["workload"] = "synthetic-mixed"
                event["mode"] = "mixed"
            self.assertFalse(analyze_run(path, status, sources, allow_suspended_idle_tabs=True)["valid"])

    suite = unittest.defaultTestLoader.loadTestsFromTestCase(AlignmentTests)
    return 0 if unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful() else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, help="Directory containing metrics files and workloads/<run>/status.json")
    parser.add_argument("--output", type=Path, help="JSON output, default INPUT/summary.json")
    parser.add_argument("--csv", type=Path, help="CSV output, default INPUT/summary.csv")
    parser.add_argument("--exclude", action="append", default=[], help="Exclude a run glob, for example '*pilot*'")
    parser.add_argument("--allow-suspended-idle-tabs", action="store_true", help="For static idle only, anchor to the stable foreground tab and allow missing/late background timer callbacks")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    if args.input is None:
        parser.error("--input is required unless --self-test is used")
    result = analyze(args.input, args.exclude, allow_suspended_idle_tabs=args.allow_suspended_idle_tabs)
    output = args.output or args.input / "summary.json"
    csv_path = args.csv or args.input / "summary.csv"
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(result, indent=2, allow_nan=False) + "\n")
    write_csv(csv_path, result["runs"])
    print(json.dumps({"output": str(output), "csv": str(csv_path), "run_count": result["run_count"],
                      "valid_run_count": result["valid_run_count"], "read_errors": len(result["read_errors"])}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
