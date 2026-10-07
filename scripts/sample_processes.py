#!/usr/bin/env python3
"""Sample a process tree with ps. CPU/RSS observations cannot prove battery parity."""

import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import statistics
import subprocess
import sys
import time


def cpu_seconds(value):
    days = 0
    if "-" in value:
        day_text, value = value.split("-", 1)
        days = int(day_text)
    parts = [float(part) for part in value.split(":")]
    seconds = 0.0
    for part in parts:
        seconds = seconds * 60 + part
    return days * 86400 + seconds


def process_table():
    completed = subprocess.run(
        ["ps", "-axo", "pid=,ppid=,pcpu=,rss=,time=,comm="],
        text=True, capture_output=True, check=True,
    )
    table = {}
    for line in completed.stdout.splitlines():
        fields = line.strip().split(None, 5)
        if len(fields) < 6:
            continue
        pid, ppid, cpu, rss, cumulative, command = fields
        table[int(pid)] = {"pid": int(pid), "ppid": int(ppid), "ps_cpu_percent": float(cpu), "rss_kib": int(rss), "cpu_seconds": cpu_seconds(cumulative), "command": command}
    return table


def select_tree(table, roots, known):
    selected = {pid for pid in roots if pid in table}
    # Keep a previously seen helper if its original parent exited.
    selected.update(pid for pid, command in known.items() if pid in table and table[pid]["command"] == command)
    while True:
        added = {pid for pid, process in table.items() if process["ppid"] in selected}
        if added <= selected:
            return [table[pid] for pid in sorted(selected)]
        selected.update(added)


def positive_float(value):
    number = float(value)
    if number <= 0 or number == float("inf") or number != number:
        raise argparse.ArgumentTypeError("Expected a finite number greater than zero")
    return number


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pid", type=int, action="append", required=True, help="Root PID to sample, repeat for extra root processes")
    parser.add_argument("--duration", type=positive_float, default=60, help="Sampling duration in seconds, default 60")
    parser.add_argument("--interval", type=positive_float, default=1, help="Seconds between samples, default 1")
    parser.add_argument("--output", type=Path, required=True, help="JSON result path")
    parser.add_argument("--label", default="", help="Description of browser, build, and workload")
    args = parser.parse_args()
    if any(pid <= 0 for pid in args.pid):
        parser.error("--pid must be greater than zero")

    started_at = datetime.now(timezone.utc).isoformat()
    started = time.monotonic()
    samples = []
    known = {}
    previous = {}
    previous_elapsed = None
    interrupted = False
    print("Sampling CPU/RSS with ps. These numbers do not measure battery energy.", file=sys.stderr)
    try:
        while True:
            processes = select_tree(process_table(), set(args.pid), known)
            elapsed = time.monotonic() - started
            if not samples and not processes:
                parser.error("No specified root process exists")
            if not processes:
                print("All observed browser processes exited; stopping.", file=sys.stderr)
                break
            known = {process["pid"]: process["command"] for process in processes}
            comparable = [process for process in processes if process["pid"] in previous and process["command"] == previous[process["pid"]]["command"]]
            delta = sum(max(0, process["cpu_seconds"] - previous[process["pid"]]["cpu_seconds"]) for process in comparable)
            sample = {
                "elapsed_seconds": round(elapsed, 3),
                "process_count": len(processes),
                "rss_sum_mib": round(sum(process["rss_kib"] for process in processes) / 1024, 3),
                "ps_cpu_percent_sum": round(sum(process["ps_cpu_percent"] for process in processes), 3),
                "observed_cpu_delta_seconds": round(delta, 4) if previous_elapsed is not None else None,
                "observed_interval_cpu_percent": round(delta / (elapsed - previous_elapsed) * 100, 3) if previous_elapsed is not None else None,
                "processes": processes,
            }
            samples.append(sample)
            previous = {process["pid"]: process for process in processes}
            previous_elapsed = elapsed
            if elapsed >= args.duration:
                break
            deadline = min(len(samples) * args.interval, args.duration)
            time.sleep(max(0, started + deadline - time.monotonic()))
    except KeyboardInterrupt:
        interrupted = True

    intervals = [sample["observed_interval_cpu_percent"] for sample in samples if sample["observed_interval_cpu_percent"] is not None]
    report = {
        "label": args.label,
        "started_at_utc": started_at,
        "root_pids": args.pid,
        "requested_duration_seconds": args.duration,
        "interval_seconds": args.interval,
        "interrupted": interrupted,
        "limitations": [
            "CPU time and RSS do not measure energy, battery drain, GPU work, or wakeups.",
            "ps CPU percentage uses the operating system's averaging window; it is not a measurement of this interval.",
            "Interval CPU uses cumulative time differences for processes present in both adjacent samples. Short-lived and newly observed processes are undercounted.",
            "The RSS sum counts shared pages once per process and is not total physical memory usage.",
            "Sampling adds overhead. Apply the same sampler and workload to every comparison.",
            "The process tree may omit services launched outside the browser's parent-child tree.",
        ],
        "summary": {
            "samples": len(samples),
            "observed_duration_seconds": samples[-1]["elapsed_seconds"] if samples else 0,
            "mean_ps_cpu_percent_sum": round(statistics.mean(sample["ps_cpu_percent_sum"] for sample in samples), 3) if samples else None,
            "mean_observed_interval_cpu_percent": round(statistics.mean(intervals), 3) if intervals else None,
            "observed_cpu_delta_seconds": round(sum(sample["observed_cpu_delta_seconds"] or 0 for sample in samples), 4),
            "peak_rss_sum_mib": max((sample["rss_sum_mib"] for sample in samples), default=None),
        },
        "samples": samples,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"output": str(args.output), **report["summary"]}), file=sys.stderr)


if __name__ == "__main__":
    main()
