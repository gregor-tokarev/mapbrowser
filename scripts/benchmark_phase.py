#!/usr/bin/env python3
"""Measure an already-open benchmark phase, release its barrier, and collect results."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
import math
from pathlib import Path
import re
import signal
import subprocess
import sys
import time
from urllib.parse import quote
from urllib.request import Request, urlopen


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_OUTPUT = ROOT / "artifacts" / "benchmarks" / "2026-09-12"


def positive(value):
    number = float(value)
    if not math.isfinite(number) or number <= 0:
        raise argparse.ArgumentTypeError("Expected a finite number greater than zero")
    return number


def nonnegative(value):
    number = float(value)
    if not math.isfinite(number) or number < 0:
        raise argparse.ArgumentTypeError("Expected a finite nonnegative number")
    return number


def utc_now():
    return datetime.now(timezone.utc).isoformat()


def request_json(url, body=None):
    request = Request(url)
    if body is not None:
        request = Request(url, data=json.dumps(body).encode(), headers={"Content-Type": "application/json"})
    with urlopen(request, timeout=10) as response:
        return json.load(response)


def validate_ready(status, args):
    if status.get("run") != args.run:
        raise ValueError("Status returned a different run ID")
    if status.get("start_epoch_ms") is not None:
        raise ValueError("The run has already started; use a fresh run ID")
    events = status.get("events", [])
    if any(event.get("event") in {"started", "completed", "error"} for event in events):
        raise ValueError("The run already contains start/completion/error events")
    ready = [event for event in events if event.get("event") == "ready"]
    tabs = {str(event.get("tab", "1")) for event in ready}
    if len(ready) != args.expected_tabs or len(tabs) != args.expected_tabs:
        raise ValueError(f"Expected {args.expected_tabs} unique ready tabs, observed {len(ready)} events / {len(tabs)} tabs")
    if any(event.get("browser") != args.browser for event in ready):
        raise ValueError("Ready events do not all match --browser")
    if any(event.get("barrier") is not True for event in ready):
        raise ValueError("Every tab must be waiting on the shared start barrier")
    if args.allow_suspended_idle_tabs and any(event.get("workload") != "synthetic-idle" or event.get("mode") != "idle" or event.get("requested_duration_seconds") != 210 for event in ready):
        raise ValueError("--allow-suspended-idle-tabs requires 210-second static idle fixtures in every tab")
    return tabs


def completed_idle_foreground(status, tabs):
    scheduled = status.get("start_epoch_ms")
    if not isinstance(scheduled, (int, float)):
        return None
    events = status.get("events", [])
    ready = [event for event in events if event.get("event") == "ready"]
    if {str(event.get("tab", "1")) for event in ready} != tabs:
        return None
    if any(event.get("server_epoch_ms", event.get("epoch_ms", math.inf)) > scheduled for event in ready):
        return None
    candidates = []
    for event in events:
        if event.get("event") != "completed" or event.get("mode") != "idle":
            continue
        started = event.get("started_epoch_ms")
        finished = event.get("finished_epoch_ms")
        if not isinstance(started, (int, float)) or not isinstance(finished, (int, float)):
            continue
        if not scheduled <= started <= scheduled + 15000 or finished < started + 195000:
            continue
        if event.get("start_visibility") != "visible" or event.get("start_focused") is not True:
            continue
        if any(started + 15000 <= change.get("epoch_ms", -math.inf) <= started + 195000 for change in event.get("visibility_events", [])):
            continue
        candidates.append(event)
    return candidates[0] if len(candidates) == 1 else None


def stop_sampler(process):
    forced = False
    if process.poll() is None:
        process.send_signal(signal.SIGINT)
    try:
        return process.wait(timeout=30), forced
    except subprocess.TimeoutExpired:
        forced = True
        process.terminate()
        try:
            return process.wait(timeout=5), forced
        except subprocess.TimeoutExpired:
            process.kill()
            return process.wait(timeout=5), forced


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pid", type=int, action="append", required=True, help="Existing browser root PID; repeat for additional roots")
    parser.add_argument("--run", required=True)
    parser.add_argument("--browser", required=True)
    parser.add_argument("--expected-tabs", type=int, default=1)
    parser.add_argument("--delay", type=nonnegative, default=15, help="Seconds from barrier release to workload start, default 15")
    parser.add_argument("--timeout", type=positive, default=600, help="Seconds allowed after releasing the start barrier")
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--port", type=int, default=8767)
    parser.add_argument("--allow-suspended-idle-tabs", action="store_true", help="For 210-second static idle only, stop after the stable foreground tab completes even if background timers were suspended")
    args = parser.parse_args()
    if any(pid <= 0 for pid in args.pid) or args.expected_tabs <= 0:
        parser.error("--pid and --expected-tabs must be positive")
    if not 1 <= args.port <= 65535:
        parser.error("--port must be between 1 and 65535")
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,95}", args.run):
        parser.error("Invalid run ID")
    if args.delay > 300:
        parser.error("--delay must be at most 300 seconds")
    if args.timeout <= args.delay:
        parser.error("--timeout must exceed --delay")
    output = args.output_dir.resolve()
    status_path = output / "workloads" / args.run / "status.json"
    metrics_path = output / f"{args.run}-metrics.json"
    log_path = output / f"{args.run}-sampler.log"
    report_path = output / f"{args.run}-controller.json"
    if metrics_path.exists() or log_path.exists() or report_path.exists():
        parser.error("Output for this run already exists; use a fresh run ID")
    base = f"http://127.0.0.1:{args.port}"
    try:
        status = request_json(base + "/api/status?run=" + quote(args.run, safe=""))
        tabs = validate_ready(status, args)
        local = json.loads(status_path.read_text())
        if local.get("run") != args.run:
            raise ValueError("Local status file has a different run ID")
    except (OSError, ValueError) as error:
        parser.error(f"Run is not ready: {error}")

    output.mkdir(parents=True, exist_ok=True)
    command = [
        sys.executable, str(ROOT / "scripts" / "benchmark_metrics.py"),
        *[part for pid in args.pid for part in ("--pid", str(pid))],
        "--duration", str(args.timeout + args.delay + 30), "--interval", "1",
        "--output", str(metrics_path), "--label", f"{args.browser} / {args.run}",
    ]
    report = {
        "run": args.run, "browser": args.browser, "expected_tabs": args.expected_tabs,
        "root_pids": args.pid, "controller_started_at_utc": utc_now(),
        "requested_delay_seconds": args.delay, "timeout_seconds": args.timeout,
        "allow_suspended_idle_tabs": args.allow_suspended_idle_tabs,
        "status_path": str(status_path), "metrics_path": str(metrics_path), "sampler_log": str(log_path),
        "sampler_command": command, "result": "starting", "errors": [],
    }
    process = None
    final_status = status
    try:
        with log_path.open("w") as log:
            process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            report["sampler_pid"] = process.pid
            # Establish the sampler baseline before posting the shared start.
            readiness_deadline = time.monotonic() + 10
            while True:
                if process.poll() is not None:
                    raise RuntimeError(f"Sampler exited during startup with status {process.returncode}; see {log_path}")
                if "Sampling unprivileged rusage" in log_path.read_text():
                    break
                if time.monotonic() >= readiness_deadline:
                    raise RuntimeError("Sampler did not finish initialization within ten seconds")
                time.sleep(0.1)
            time.sleep(1)
            if process.poll() is not None:
                raise RuntimeError("Sampler exited before the shared start was posted")
            start = request_json(base + "/api/start", {"run": args.run, "delay_ms": args.delay * 1000})
            report["scheduled_start_epoch_ms"] = start["start_epoch_ms"]
            report["result"] = "running"
            print(json.dumps({"run": args.run, "result": "running", "sampler_pid": process.pid,
                              "scheduled_start_epoch_ms": start["start_epoch_ms"]}), flush=True)
            deadline = time.monotonic() + args.timeout
            while True:
                try:
                    final_status = json.loads(status_path.read_text())
                except (OSError, ValueError) as error:
                    report["errors"].append({"at_utc": utc_now(), "operation": "read_status", "message": str(error)})
                else:
                    events = final_status.get("events", [])
                    failed = [event for event in events if event.get("event") == "error"]
                    completed = {str(event.get("tab", "1")) for event in events if event.get("event") == "completed"}
                    if failed:
                        report["result"] = "workload_error"
                        report["errors"].extend({"operation": "workload", "tab": event.get("tab"),
                                                 "message": event.get("message", "unspecified error")} for event in failed)
                        break
                    if completed >= tabs:
                        report["result"] = "completed"
                        break
                    if args.allow_suspended_idle_tabs:
                        foreground = completed_idle_foreground(final_status, tabs)
                        if foreground is not None:
                            report["result"] = "completed_with_suspended_idle_tabs"
                            report["completion_basis"] = "Stable foreground static idle tab completed; every expected document was ready before start"
                            report["idle_foreground_tab"] = str(foreground.get("tab", "1"))
                            report["background_tabs_without_completion"] = sorted(tabs - completed)
                            report["comparison_scope"] = "Loaded static documents under native background suspension policy; not active-background execution or battery life"
                            break
                if process.poll() is not None:
                    report["result"] = "sampler_exited"
                    report["errors"].append({"operation": "sampler", "message": f"Sampler exited with status {process.returncode} before phase completion"})
                    break
                if time.monotonic() >= deadline:
                    report["result"] = "timeout"
                    break
                time.sleep(min(2, max(0, deadline - time.monotonic())))
    except KeyboardInterrupt:
        report["result"] = "interrupted"
    except (OSError, ValueError, RuntimeError) as error:
        report["result"] = "controller_error"
        report["errors"].append({"operation": "controller", "message": str(error)})
    finally:
        if process is not None:
            try:
                time.sleep(2)
            except KeyboardInterrupt:
                pass
            returncode, forced = stop_sampler(process)
            report["sampler_returncode"] = returncode
            report["sampler_forced_termination"] = forced
            if returncode != 0 or forced:
                report["errors"].append({"operation": "sampler", "message": f"Sampler exited with status {returncode}; forced termination: {forced}"})
        report["controller_finished_at_utc"] = utc_now()
        report["completed_tabs"] = sorted({str(event.get("tab", "1")) for event in final_status.get("events", []) if event.get("event") == "completed"})
        report["metrics_file_exists"] = metrics_path.is_file()
        if report["result"] in {"completed", "completed_with_suspended_idle_tabs"} and (report["errors"] or not metrics_path.is_file()):
            report["result"] = "completed_with_errors"
        temporary = report_path.with_suffix(".tmp")
        temporary.write_text(json.dumps(report, indent=2) + "\n")
        temporary.replace(report_path)
    print(json.dumps({"run": args.run, "result": report["result"], "completed_tabs": report["completed_tabs"],
                      "metrics_path": str(metrics_path), "controller_report": str(report_path)}), flush=True)
    return 0 if report["result"] in {"completed", "completed_with_suspended_idle_tabs"} else 2


if __name__ == "__main__":
    raise SystemExit(main())
