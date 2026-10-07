#!/usr/bin/env python3
"""Unprivileged macOS process CPU, footprint, wakeup and CPU-energy observations."""
from __future__ import annotations

import argparse
import ctypes
from datetime import datetime, timezone
import json
import math
import os
from pathlib import Path
import statistics
import subprocess
import sys
import time


# Matches the installed macOS SDK's sys/resource.h, RUSAGE_INFO_V6.
RUSAGE_FIELDS = """
ri_user_time ri_system_time ri_pkg_idle_wkups ri_interrupt_wkups ri_pageins
ri_wired_size ri_resident_size ri_phys_footprint ri_proc_start_abstime
ri_proc_exit_abstime ri_child_user_time ri_child_system_time
ri_child_pkg_idle_wkups ri_child_interrupt_wkups ri_child_pageins
ri_child_elapsed_abstime ri_diskio_bytesread ri_diskio_byteswritten
ri_cpu_time_qos_default ri_cpu_time_qos_maintenance ri_cpu_time_qos_background
ri_cpu_time_qos_utility ri_cpu_time_qos_legacy ri_cpu_time_qos_user_initiated
ri_cpu_time_qos_user_interactive ri_billed_system_time ri_serviced_system_time
ri_logical_writes ri_lifetime_max_phys_footprint ri_instructions ri_cycles
ri_billed_energy ri_serviced_energy ri_interval_max_phys_footprint ri_runnable_time
ri_flags ri_user_ptime ri_system_ptime ri_pinstructions ri_pcycles ri_energy_nj
ri_penergy_nj ri_secure_time_in_system ri_secure_ptime_in_system
ri_neural_footprint ri_lifetime_max_neural_footprint ri_interval_max_neural_footprint
""".split()


class RUsageInfoV6(ctypes.Structure):
    _fields_ = [("ri_uuid", ctypes.c_uint8 * 16)] + [(name, ctypes.c_uint64) for name in RUSAGE_FIELDS] + [("ri_reserved", ctypes.c_uint64 * 9)]


class CoalitionInfo(ctypes.Structure):
    # Private XNU PROC_PIDCOALITIONINFO ABI: two IDs and three reserved words.
    _fields_ = [("coalition_id", ctypes.c_uint64 * 2), ("reserved", ctypes.c_uint64 * 3)]


class MachTimebaseInfo(ctypes.Structure):
    _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]


class FrontmostApplication:
    """Read the public NSWorkspace property without activating any application."""
    source = "NSWorkspace.sharedWorkspace.frontmostApplication"

    def __init__(self):
        self.appkit = ctypes.CDLL("/System/Library/Frameworks/AppKit.framework/AppKit")
        self.objc = ctypes.CDLL("/usr/lib/libobjc.A.dylib")
        self.objc.objc_getClass.argtypes = [ctypes.c_char_p]
        self.objc.objc_getClass.restype = ctypes.c_void_p
        self.objc.sel_registerName.argtypes = [ctypes.c_char_p]
        self.objc.sel_registerName.restype = ctypes.c_void_p
        self.objc.objc_autoreleasePoolPush.argtypes = []
        self.objc.objc_autoreleasePoolPush.restype = ctypes.c_void_p
        self.objc.objc_autoreleasePoolPop.argtypes = [ctypes.c_void_p]
        self.objc.objc_autoreleasePoolPop.restype = None
        self.pointer_message = ctypes.CFUNCTYPE(ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p)(("objc_msgSend", self.objc))
        self.int_message = ctypes.CFUNCTYPE(ctypes.c_int, ctypes.c_void_p, ctypes.c_void_p)(("objc_msgSend", self.objc))
        self.string_message = ctypes.CFUNCTYPE(ctypes.c_char_p, ctypes.c_void_p, ctypes.c_void_p)(("objc_msgSend", self.objc))
        self.selectors = {name: self.objc.sel_registerName(name.encode()) for name in ("sharedWorkspace", "frontmostApplication", "bundleIdentifier", "localizedName", "processIdentifier", "UTF8String")}
        self.workspace_class = self.objc.objc_getClass(b"NSWorkspace")
        if not self.workspace_class:
            raise RuntimeError("NSWorkspace class is unavailable")
        self.corefoundation = ctypes.CDLL("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
        self.corefoundation.CFRunLoopRunInMode.argtypes = [ctypes.c_void_p, ctypes.c_double, ctypes.c_bool]
        self.corefoundation.CFRunLoopRunInMode.restype = ctypes.c_int32
        self.default_mode = ctypes.c_void_p.in_dll(self.corefoundation, "kCFRunLoopDefaultMode").value

    def read(self):
        began = time.monotonic_ns()
        pool = self.objc.objc_autoreleasePoolPush()
        record = {"source": self.source, "captured_at_epoch_ns": time.time_ns(), "bundle_identifier": None, "pid": None, "localized_name": None, "error": None}
        try:
            # NSRunningApplication's changing properties update on a main-loop
            # turn. Drain ready notifications without waiting or activating UI.
            for _ in range(8):
                status = self.corefoundation.CFRunLoopRunInMode(self.default_mode, 0.0, True)
                if status != 4:  # kCFRunLoopRunHandledSource
                    break
            workspace = self.pointer_message(self.workspace_class, self.selectors["sharedWorkspace"])
            application = self.pointer_message(workspace, self.selectors["frontmostApplication"])
            record["captured_at_epoch_ns"] = time.time_ns()
            if not application:
                record["error"] = "NSWorkspace returned no frontmost application"
                return record
            record["pid"] = self.int_message(application, self.selectors["processIdentifier"])
            for output, selector in (("bundle_identifier", "bundleIdentifier"), ("localized_name", "localizedName")):
                string = self.pointer_message(application, self.selectors[selector])
                raw = self.string_message(string, self.selectors["UTF8String"]) if string else None
                record[output] = raw.decode("utf-8", errors="replace") if raw else None
            return record
        finally:
            self.objc.objc_autoreleasePoolPop(pool)
            record["query_duration_ns"] = time.monotonic_ns() - began


COUNTERS = {
    "user_cpu_ns": "ri_user_time",
    "system_cpu_ns": "ri_system_time",
    "package_idle_wakeups": "ri_pkg_idle_wkups",
    "interrupt_wakeups": "ri_interrupt_wkups",
    "cpu_energy_nj": "ri_energy_nj",
    "performance_core_cpu_energy_nj": "ri_penergy_nj",
    "disk_read_bytes": "ri_diskio_bytesread",
    "disk_written_bytes": "ri_diskio_byteswritten",
}


class NativeMetrics:
    def __init__(self):
        if sys.platform != "darwin":
            raise RuntimeError("This sampler requires macOS with proc_pid_rusage version 6.")
        self.lib = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
        self.lib.proc_pid_rusage.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
        self.lib.proc_pid_rusage.restype = ctypes.c_int
        self.lib.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64, ctypes.c_void_p, ctypes.c_int]
        self.lib.proc_pidinfo.restype = ctypes.c_int
        self.system = ctypes.CDLL("/usr/lib/libSystem.B.dylib")
        self.system.mach_absolute_time.argtypes = []
        self.system.mach_absolute_time.restype = ctypes.c_uint64
        self.system.mach_timebase_info.argtypes = [ctypes.POINTER(MachTimebaseInfo)]
        self.system.mach_timebase_info.restype = ctypes.c_int
        timebase = MachTimebaseInfo()
        if self.system.mach_timebase_info(ctypes.byref(timebase)) != 0 or timebase.denom == 0:
            raise RuntimeError("Cannot obtain the Mach absolute-time conversion factor.")
        self.timebase_numer = timebase.numer
        self.timebase_denom = timebase.denom

    def absolute_time(self):
        return self.system.mach_absolute_time()

    def rusage(self, pid):
        value = RUsageInfoV6()
        ctypes.set_errno(0)
        if self.lib.proc_pid_rusage(pid, 6, ctypes.byref(value)) != 0:
            error = ctypes.get_errno()
            raise OSError(error, f"proc_pid_rusage({pid}, V6): {os.strerror(error)}")
        result = {name: int(getattr(value, field)) for name, field in COUNTERS.items()}
        # XNU's rusage CPU times are Mach absolute ticks on Apple Silicon.
        # Energy already has explicit nanojoule units and is not time-scaled.
        for name in ("user_cpu_ns", "system_cpu_ns"):
            result[name] = result[name] * self.timebase_numer // self.timebase_denom
        result.update({
            "user_cpu_abstime": int(value.ri_user_time),
            "system_cpu_abstime": int(value.ri_system_time),
            "counter_read_epoch_ns": time.time_ns(),
            "start_abstime": int(value.ri_proc_start_abstime),
            "exit_abstime": int(value.ri_proc_exit_abstime),
            "executable_uuid": bytes(value.ri_uuid).hex(),
            "phys_footprint_bytes": int(value.ri_phys_footprint),
            "resident_bytes": int(value.ri_resident_size),
            "lifetime_max_phys_footprint_bytes": int(value.ri_lifetime_max_phys_footprint),
        })
        return result

    def coalition(self, pid):
        value = CoalitionInfo()
        ctypes.set_errno(0)
        count = self.lib.proc_pidinfo(pid, 20, 0, ctypes.byref(value), ctypes.sizeof(value))
        if count != ctypes.sizeof(value):
            error = ctypes.get_errno()
            raise OSError(error, f"coalition lookup({pid}) returned {count}/40 bytes: {os.strerror(error)}")
        return {"resource": int(value.coalition_id[0]), "jetsam": int(value.coalition_id[1])}


def process_table():
    output = subprocess.check_output(["ps", "-axo", "pid=,ppid=,uid=,comm="], text=True)
    result = {}
    for line in output.splitlines():
        fields = line.strip().split(None, 3)
        if len(fields) == 4:
            pid, ppid, uid, command = fields
            result[int(pid)] = {"pid": int(pid), "ppid": int(ppid), "uid": int(uid), "command": command}
    return result


def descendants(table, roots):
    selected = set(roots)
    while True:
        added = {pid for pid, item in table.items() if item["ppid"] in selected}
        if added <= selected:
            return selected
        selected.update(added)


class ProcessCollector:
    def __init__(self, roots, use_coalitions=True):
        self.native = NativeMetrics()
        self.roots = set(roots)
        self.sampler_pid = os.getpid()
        if self.sampler_pid in self.roots:
            raise RuntimeError("The sampler cannot be one of its own target roots.")
        self.root_starts = {}
        self.root_coalitions = {}
        self.notes = []
        self.known = {}
        try:
            self.frontmost = FrontmostApplication()
            self.frontmost_error = None
            # Pay initial AppKit/LaunchServices setup cost before timed sampling.
            self.frontmost.read()
        except (OSError, RuntimeError, AttributeError) as error:
            self.frontmost = None
            self.frontmost_error = str(error)
            self.notes.append(f"Frontmost application tracking unavailable: {error}")
        try:
            own_coalition = self.native.coalition(self.sampler_pid)["resource"]
        except OSError:
            own_coalition = None
        for pid in sorted(self.roots):
            self.root_starts[pid] = self.native.rusage(pid)["start_abstime"]
            try:
                coalition = self.native.coalition(pid)["resource"]
            except OSError as error:
                self.notes.append(str(error))
                continue
            if not use_coalitions:
                continue
            if not coalition or own_coalition is None or coalition == own_coalition:
                self.notes.append(f"Root {pid}: resource coalition attribution disabled because it is unavailable or shared with the sampler. Descendants remain included; launch an app through LaunchServices for its own coalition.")
                continue
            self.root_coalitions[pid] = coalition

    def capture(self):
        full_table = process_table()
        excluded = descendants(full_table, {self.sampler_pid})
        table = {pid: item for pid, item in full_table.items() if item["uid"] == os.getuid() and pid not in excluded}
        cache = {}
        errors = []

        def usage(pid):
            if pid not in cache:
                try:
                    cache[pid] = self.native.rusage(pid)
                except OSError as error:
                    cache[pid] = None
                    errors.append({"pid": pid, "operation": "rusage", "errno": error.errno, "error": str(error)})
                    if error.errno == 3:  # ESRCH confirms this PID no longer exists.
                        self.known.pop(pid, None)
            return cache[pid]

        active_roots = {pid for pid, start in self.root_starts.items() if pid in table and usage(pid) and usage(pid)["start_abstime"] == start}
        family = descendants(table, active_roots)
        coalitions = {}
        coalition_errors = 0
        for pid in table if self.root_coalitions else ():
            try:
                coalitions[pid] = self.native.coalition(pid)
            except OSError:
                coalition_errors += 1
        accepted_coalitions = set(self.root_coalitions.values())
        cohort = {pid for pid, value in coalitions.items() if value["resource"] in accepted_coalitions}
        # Retain already attributed helpers after reparenting, but never retain
        # a reused PID. An unlisted zombie can still expose its final rusage.
        candidates = family | cohort | set(self.known)
        records = []
        for pid in sorted(candidates - excluded):
            value = usage(pid)
            if value is None:
                continue
            prior = self.known.get(pid)
            same_process = prior is not None and prior["start_abstime"] == value["start_abstime"]
            if pid in self.roots and value["start_abstime"] != self.root_starts[pid]:
                self.known.pop(pid, None)
                continue
            if pid not in family and pid not in cohort and not same_process:
                self.known.pop(pid, None)
                continue
            metadata = table.get(pid) or prior
            if metadata is None:
                continue
            reasons = []
            if pid in active_roots:
                reasons.append("explicit_root")
            elif pid in family:
                reasons.append("descendant")
            if pid in cohort:
                reasons.append("matching_root_resource_coalition")
            if not reasons:
                reasons.append("retained_same_process")
            records.append({
                **{key: metadata[key] for key in ("pid", "ppid", "uid", "command")},
                **value,
                "coalitions": coalitions.get(pid, prior.get("coalitions") if prior else None),
                "attribution": reasons,
                "listed_by_ps": pid in table,
            })
        for item in records:
            if item["exit_abstime"]:
                self.known.pop(item["pid"], None)
            else:
                self.known[item["pid"]] = item
        frontmost = self.frontmost.read() if self.frontmost else {
            "source": FrontmostApplication.source,
            "captured_at_epoch_ns": time.time_ns(),
            "bundle_identifier": None,
            "pid": None,
            "localized_name": None,
            "error": self.frontmost_error,
        }
        return {"processes": records, "read_errors": errors, "coalition_lookup_failures": coalition_errors, "frontmost_application": frontmost}


def interval_deltas(current, previous, baseline_abstime):
    totals = {key: 0 for key in COUNTERS}
    new_processes = []
    late_existing = []
    regressions = []
    for item in current:
        key = (item["pid"], item["start_abstime"])
        prior = previous.get(key)
        if prior is None:
            if item["start_abstime"] >= baseline_abstime:
                prior = {counter: 0 for counter in COUNTERS}
                new_processes.append(item["pid"])
            else:
                late_existing.append(item["pid"])
                continue
        for counter in COUNTERS:
            delta = item[counter] - prior[counter]
            if delta < 0:
                regressions.append({"pid": item["pid"], "counter": counter})
                continue
            totals[counter] += delta
    return totals, new_processes, late_existing, regressions


def positive_float(value):
    result = float(value)
    if not math.isfinite(result) or result <= 0:
        raise argparse.ArgumentTypeError("Expected a finite number greater than zero")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pid", type=int, action="append", required=True, help="Browser root PID; repeat for multiple roots")
    parser.add_argument("--duration", type=positive_float, default=60)
    parser.add_argument("--interval", type=positive_float, default=1)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--label", default="")
    parser.add_argument("--no-coalitions", action="store_true", help="Use only roots and descendants; incomplete for Safari XPC services")
    args = parser.parse_args()
    if any(pid <= 0 for pid in args.pid):
        parser.error("--pid must be positive")
    try:
        collector = ProcessCollector(args.pid, not args.no_coalitions)
    except (RuntimeError, OSError) as error:
        parser.error(str(error))
    print("Sampling unprivileged rusage: CPU energy only, not battery or whole-device energy.", file=sys.stderr)
    started_at = datetime.now(timezone.utc).isoformat()
    started_at_epoch_ns = time.time_ns()
    started = time.monotonic()
    baseline_abstime = collector.native.absolute_time()
    samples = []
    previous = {}
    lifetime_seen = {}
    interrupted = False
    try:
        while True:
            capture_started = time.monotonic()
            capture_started_epoch_ns = time.time_ns()
            sample = collector.capture()
            elapsed = time.monotonic() - started
            processes = sample["processes"]
            if not processes:
                print("No attributed processes remain; stopping.", file=sys.stderr)
                break
            sample.update({
                "elapsed_seconds": elapsed,
                "epoch_time_ns": time.time_ns(),
                "capture_started_epoch_ns": capture_started_epoch_ns,
                "capture_duration_seconds": time.monotonic() - capture_started,
                "process_count": len(processes),
                "phys_footprint_sum_mib": sum(item["phys_footprint_bytes"] for item in processes) / 1048576,
                "resident_sum_mib": sum(item["resident_bytes"] for item in processes) / 1048576,
            })
            if samples:
                seconds = elapsed - samples[-1]["elapsed_seconds"]
                delta, new, late, regressions = interval_deltas(processes, lifetime_seen, baseline_abstime)
                sample.update({
                    "interval_seconds": seconds,
                    "interval_deltas": delta,
                    "interval_cpu_core_percent": (delta["user_cpu_ns"] + delta["system_cpu_ns"]) / 1e9 / seconds * 100,
                    "interval_cpu_energy_joules": delta["cpu_energy_nj"] / 1e9,
                    "interval_cpu_power_watts": delta["cpu_energy_nj"] / 1e9 / seconds,
                    "new_processes_counted_from_birth": new,
                    "existing_processes_first_attributed": late,
                    "counter_regressions": regressions,
                    "processes_returning_after_sampling_gap": [item["pid"] for item in processes if (item["pid"], item["start_abstime"]) in lifetime_seen and (item["pid"], item["start_abstime"]) not in previous],
                    "previously_attributed_processes_missing": [pid for pid, start in previous if not any(item["pid"] == pid and item["start_abstime"] == start for item in processes)],
                })
            else:
                sample["interval_deltas"] = None
            samples.append(sample)
            previous = {(item["pid"], item["start_abstime"]): item for item in processes}
            for key, item in previous.items():
                earlier = lifetime_seen.get(key)
                lifetime_seen[key] = dict(item)
                if earlier:
                    for counter in COUNTERS:
                        lifetime_seen[key][counter] = max(earlier[counter], item[counter])
            if elapsed >= args.duration:
                break
            time.sleep(max(0, started + min(len(samples) * args.interval, args.duration) - time.monotonic()))
    except KeyboardInterrupt:
        interrupted = True

    measured_seconds = sum(sample.get("interval_seconds", 0) for sample in samples)
    totals = {key: sum((sample["interval_deltas"] or {}).get(key, 0) for sample in samples) for key in COUNTERS}
    cpu_seconds = (totals["user_cpu_ns"] + totals["system_cpu_ns"]) / 1e9
    energy_available = any(item["cpu_energy_nj"] > 0 for sample in samples for item in sample["processes"])
    summary = {
        "samples": len(samples),
        "measured_interval_seconds": measured_seconds,
        "observed_cpu_seconds": cpu_seconds,
        "mean_cpu_core_percent": cpu_seconds / measured_seconds * 100 if measured_seconds else None,
        "cpu_energy_counter_observed_nonzero": energy_available,
        "observed_cpu_energy_joules": totals["cpu_energy_nj"] / 1e9 if energy_available else None,
        "mean_observed_cpu_power_watts": totals["cpu_energy_nj"] / 1e9 / measured_seconds if energy_available and measured_seconds else None,
        "mean_phys_footprint_sum_mib": statistics.mean(sample["phys_footprint_sum_mib"] for sample in samples) if samples else None,
        "peak_phys_footprint_sum_mib": max((sample["phys_footprint_sum_mib"] for sample in samples), default=None),
        "peak_resident_sum_mib": max((sample["resident_sum_mib"] for sample in samples), default=None),
        "observed_package_idle_wakeups": totals["package_idle_wakeups"],
        "observed_interrupt_wakeups": totals["interrupt_wakeups"],
        "read_error_count": sum(len(sample["read_errors"]) for sample in samples),
        "counter_regression_count": sum(len(sample.get("counter_regressions", [])) for sample in samples),
        "total_interval_deltas": totals,
    }
    report = {
        "label": args.label,
        "started_at_utc": started_at,
        "started_at_epoch_ns": started_at_epoch_ns,
        "root_pids": args.pid,
        "root_start_abstimes": collector.root_starts,
        "accepted_root_resource_coalitions": collector.root_coalitions,
        "attribution_notes": collector.notes,
        "requested_duration_seconds": args.duration,
        "requested_interval_seconds": args.interval,
        "interrupted": interrupted,
        "measurement": "proc_pid_rusage RUSAGE_INFO_V6, with same-user root/descendant/resource-coalition attribution",
        "mach_timebase": {"numer": collector.native.timebase_numer, "denom": collector.native.timebase_denom},
        "frontmost_application_source": FrontmostApplication.source,
        "summary": summary,
        "samples": samples,
        "limitations": [
            "ri_energy_nj is reported CPU energy in nanojoules when available. It excludes whole-device GPU, display, networking, storage and battery energy. ri_penergy_nj is the performance-core subset and is not added to the total.",
            "CPU energy attribution is asynchronous and platform-dependent. All-zero energy counters mean no reported energy with support unconfirmed; summary energy values are null in that case.",
            "Physical footprint is the kernel's per-process charged-memory ledger. The sum is not a simultaneous deduplicated footprint scan; resident sums include shared mappings.",
            "Only same-user processes attributed to roots, descendants, matching nonzero root resource coalitions, or retained stable process identities are included. Shared services outside those groups are excluded.",
            "Coalition lookup uses a private XNU ABI and can fail. Launching a browser from a terminal may share a coalition with the sampler; that coalition is deliberately excluded to prevent attribution of unrelated terminal work.",
            "New processes born after measurement began contribute their counters since birth. Existing processes first attributed later establish a new baseline. Processes born and reaped between samples remain unobserved; final work before disappearance can be missed.",
            "Lifetime counter baselines survive sampling gaps. Work observed after a gap is assigned to the interval when it reappears and flagged; very short event-aligned intervals may therefore include work from an earlier missed interval.",
            "A PID is paired with ri_proc_start_abstime to avoid combining different process lifetimes. The process table and counter reads are sequential, not an atomic system snapshot.",
            "Sampling adds CPU and system activity. The sampler and its descendants are excluded from browser totals, but their work still affects whole-device conditions.",
            "CPU percentage is expressed as percent of one CPU core and uses total observed CPU time divided by total measured interval time. This is not a battery-life score.",
            "Raw rusage user/system CPU times are converted from Mach absolute ticks with mach_timebase_info; raw ticks and the conversion factor are retained. Epoch timestamps support alignment with page/server events.",
            "Each sample reads NSWorkspace.frontmostApplication, which identifies the application receiving key events, independently of page or iframe focus. Samples cannot exclude brief app changes between reads or prove which browser tab or frame held focus. Existing reports without this field have no native foreground evidence.",
        ],
        "sources": [
            "https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/resource.h",
            "https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/proc_info_private.h",
            "https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/proc_info.c",
            "https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/kern/recount.h",
            "https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/kern/bsd_kern.c",
            "https://developer.apple.com/documentation/appkit/nsworkspace/frontmostapplication",
        ],
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, allow_nan=False) + "\n")
    print(json.dumps({"output": str(args.output), **summary}), file=sys.stderr)
    return 0 if samples else 2


if __name__ == "__main__":
    raise SystemExit(main())
