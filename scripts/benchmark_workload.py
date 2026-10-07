#!/usr/bin/env python3
"""Serve pinned Speedometer 3.1 and controlled browser workloads without browser automation."""

import argparse
from datetime import datetime, timezone
import hashlib
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import json
import mimetypes
from pathlib import Path, PurePosixPath
import re
import shutil
import tarfile
import tempfile
import threading
import time
from urllib.parse import parse_qs, unquote, urlsplit
from urllib.request import urlopen


ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "benchmarks"
COMMIT = "1386415be8fef2f6b6bbdbe1828872471c5d802a"
ARCHIVE_URL = f"https://codeload.github.com/WebKit/Speedometer/tar.gz/{COMMIT}"
DEFAULT_SOURCE = FIXTURES / "vendor" / "speedometer-3.1"
MAX_ARCHIVE_BYTES = 100 * 1024 * 1024
MAX_EVENT_BYTES = 16 * 1024 * 1024
RUN_PATTERN = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,95}\Z")


def utc_now():
    return datetime.now(timezone.utc).isoformat()


def prepare(destination):
    destination = destination.resolve()
    manifest_path = destination / ".map-benchmark-source.json"
    if manifest_path.exists():
        manifest = json.loads(manifest_path.read_text())
        if manifest.get("commit") != COMMIT:
            raise ValueError(f"Existing source is not the pinned release: {destination}")
        for relative, expected in manifest["file_sha256"].items():
            actual = hashlib.sha256((destination / relative).read_bytes()).hexdigest()
            if actual != expected:
                raise ValueError(f"Official source was modified: {relative}")
        return manifest
    if destination.exists():
        raise ValueError(f"Source directory exists without a provenance manifest: {destination}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="speedometer-download-", dir=destination.parent) as temporary:
        temporary = Path(temporary)
        archive = temporary / "source.tar.gz"
        digest = hashlib.sha256()
        download_bytes = 0
        with urlopen(ARCHIVE_URL, timeout=60) as response, archive.open("wb") as output:
            while chunk := response.read(1024 * 1024):
                download_bytes += len(chunk)
                if download_bytes > MAX_ARCHIVE_BYTES:
                    raise ValueError("Archive exceeds the 100 MiB download limit")
                digest.update(chunk)
                output.write(chunk)
        extracted = temporary / "source"
        extracted.mkdir()
        hashes = {}
        expanded_bytes = 0
        with tarfile.open(archive, "r:gz") as source:
            for member in source:
                parts = PurePosixPath(member.name).parts
                if not parts or parts[0] != f"Speedometer-{COMMIT}" or ".." in parts:
                    raise ValueError("Unexpected archive member path")
                relative = Path(*parts[1:])
                if relative == Path("."):
                    continue
                target = extracted / relative
                if member.isdir():
                    target.mkdir(parents=True, exist_ok=True)
                elif member.isfile():
                    expanded_bytes += member.size
                    if expanded_bytes > MAX_ARCHIVE_BYTES:
                        raise ValueError("Extracted source exceeds 100 MiB")
                    target.parent.mkdir(parents=True, exist_ok=True)
                    with source.extractfile(member) as content, target.open("wb") as output:
                        shutil.copyfileobj(content, output)
                    hashes[relative.as_posix()] = hashlib.sha256(target.read_bytes()).hexdigest()
                else:
                    raise ValueError(f"Unsupported archive member: {member.name}")
        for required in ["index.html", "resources/main.mjs", "resources/tests.mjs", "LICENSE"]:
            if required not in hashes:
                raise ValueError(f"Missing required benchmark file: {required}")
        manifest = {
            "benchmark": "Speedometer 3.1",
            "repository": "https://github.com/WebKit/Speedometer",
            "branch": "release/3.1",
            "commit": COMMIT,
            "archive_url": ARCHIVE_URL,
            "archive_sha256": digest.hexdigest(),
            "download_bytes": download_bytes,
            "expanded_bytes": expanded_bytes,
            "prepared_at_utc": utc_now(),
            "source_files_unchanged": True,
            "file_sha256": hashes,
        }
        (extracted / manifest_path.name).write_text(json.dumps(manifest, indent=2) + "\n")
        extracted.rename(destination)
    return manifest


def validate_run(value):
    if not isinstance(value, str) or not RUN_PATTERN.fullmatch(value):
        raise ValueError("run must be 1-96 letters, numbers, periods, underscores or hyphens")
    return value


class Collector:
    def __init__(self, output):
        self.output = output.resolve()
        self.output.mkdir(parents=True, exist_ok=True)
        self.lock = threading.RLock()
        self.condition = threading.Condition(self.lock)
        self.runs = {}

    def run(self, name):
        return self.runs.setdefault(name, {"run": name, "start_epoch_ms": None, "events": []})

    def record(self, payload):
        name = validate_run(payload.get("run"))
        kind = payload.get("event")
        if kind not in {"ready", "started", "completed", "error"}:
            raise ValueError("Unsupported event")
        record = {
            **payload,
            "server_received_at_utc": utc_now(),
            "server_epoch_ms": time.time_ns() / 1_000_000,
            "server_monotonic_ns": time.monotonic_ns(),
        }
        with self.condition:
            run = self.run(name)
            run["events"].append(record)
            directory = self.output / name
            directory.mkdir(exist_ok=True)
            with (directory / "events.jsonl").open("a") as stream:
                stream.write(json.dumps(record, separators=(",", ":")) + "\n")
            self.write_status(directory, run)
            self.condition.notify_all()
        return {"ok": True, "server_epoch_ms": record["server_epoch_ms"]}

    @staticmethod
    def write_status(directory, run):
        temporary = directory / "status.tmp"
        temporary.write_text(json.dumps(run, indent=2) + "\n")
        temporary.replace(directory / "status.json")

    def start(self, payload):
        name = validate_run(payload.get("run"))
        delay = float(payload.get("delay_ms", 5000))
        if not 0 <= delay <= 300_000:
            raise ValueError("delay_ms must be between 0 and 300000")
        with self.condition:
            run = self.run(name)
            if run["start_epoch_ms"] is not None:
                raise ValueError("This run has already started; use a fresh run ID")
            run["start_epoch_ms"] = time.time_ns() / 1_000_000 + delay
            directory = self.output / name
            directory.mkdir(exist_ok=True)
            self.write_status(directory, run)
            self.condition.notify_all()
            return {"run": name, "start_epoch_ms": run["start_epoch_ms"]}

    def start_time(self, name):
        name = validate_run(name)
        with self.lock:
            value = self.run(name)["start_epoch_ms"]
        return {"run": name, "start_epoch_ms": value}

    def status(self, name=None):
        with self.lock:
            if name:
                name = validate_run(name)
                return self.runs.get(name, {"run": name, "start_epoch_ms": None, "events": []})
            return {"runs": [{"run": name, "start_epoch_ms": run["start_epoch_ms"],
                              "events": len(run["events"])} for name, run in self.runs.items()]}


class BenchmarkServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, address, source, output):
        self.source = source.resolve()
        self.collector = Collector(output)
        super().__init__(address, Handler)


class Handler(SimpleHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, message, *args):
        # Asset request logging adds avoidable terminal work during the benchmark.
        if len(args) > 1 and str(args[1]) in {"200", "304"}:
            return
        super().log_message(message, *args)

    def send_bytes(self, status, data, content_type):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(data)

    def send_json(self, status, value):
        self.send_bytes(status, json.dumps(value).encode(), "application/json; charset=utf-8")

    def do_HEAD(self):
        self.do_GET()

    def do_GET(self):
        parsed = urlsplit(self.path)
        path = unquote(parsed.path)
        query = parse_qs(parsed.query)
        try:
            if path == "/api/status":
                return self.send_json(200, self.server.collector.status(query.get("run", [None])[0]))
            if path == "/api/start-time":
                return self.send_json(200, self.server.collector.start_time(query.get("run", [None])[0]))
            if path == "/api/source":
                manifest = json.loads((self.server.source / ".map-benchmark-source.json").read_text())
                manifest.pop("file_sha256", None)
                return self.send_json(200, manifest)
            if path == "/":
                return self.send_json(200, {
                    "speedometer": "/speedometer/?startAutomatically&mb_run=example&mb_browser=browser&mb_delay=5000",
                    "controlled_speedometer": "/speedometer/?startAutomatically&mb_run=example&mb_browser=browser&mb_barrier=1",
                    "workload": "/workload.html?run=example&browser=browser&tab=1&mode=idle&duration=60",
                    "start": "POST /api/start {run, delay_ms}",
                    "status": "/api/status?run=example",
                })
            if path.startswith("/speedometer/"):
                relative = path.removeprefix("/speedometer/") or "index.html"
                root = self.server.source
            elif path in {"/speedometer-harness.js", "/workload.html", "/workload.js"}:
                relative = path[1:]
                root = FIXTURES
            else:
                return self.send_error(404)
            target = (root / relative).resolve()
            if not target.is_relative_to(root):
                return self.send_error(404)
            if target.is_dir():
                # Preserve the directory URL so its relative scripts, data and
                # stylesheet paths resolve exactly as on the official server.
                if not parsed.path.endswith("/"):
                    location = parsed.path + "/" + ("?" + parsed.query if parsed.query else "")
                    self.send_response(301)
                    self.send_header("Location", location)
                    self.send_header("Content-Length", "0")
                    self.send_header("Cache-Control", "no-store")
                    self.end_headers()
                    return
                target = next((target / name for name in ("index.html", "index.htm") if (target / name).is_file()), target)
            if not target.is_file():
                return self.send_error(404)
            content = target.read_bytes()
            if path in {"/speedometer/", "/speedometer/index.html"}:
                content = content.replace(b"<head>", b'<head>\n<script src="/speedometer-harness.js"></script>', 1)
            mime = mimetypes.guess_type(target.name)[0] or "application/octet-stream"
            if target.suffix in {".js", ".mjs"}:
                mime = "text/javascript"
            self.send_bytes(200, content, mime)
        except (ValueError, json.JSONDecodeError) as error:
            self.send_json(400, {"error": str(error)})
        except TimeoutError as error:
            self.send_json(408, {"error": str(error)})
        except FileNotFoundError:
            self.send_json(404, {"error": "Benchmark source is missing. Run the prepare command first."})

    def do_POST(self):
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if not 0 < length <= MAX_EVENT_BYTES:
                raise ValueError("Invalid request size")
            payload = json.loads(self.rfile.read(length))
            if not isinstance(payload, dict):
                raise ValueError("Expected a JSON object")
            path = urlsplit(self.path).path
            if path == "/api/event":
                self.send_json(200, self.server.collector.record(payload))
            elif path == "/api/start":
                self.send_json(200, self.server.collector.start(payload))
            else:
                self.send_error(404)
        except (ValueError, json.JSONDecodeError) as error:
            self.send_json(400, {"error": str(error)})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    prepare_parser = commands.add_parser("prepare", help="Download and verify the pinned official source")
    prepare_parser.add_argument("--source", type=Path, default=DEFAULT_SOURCE)
    serve_parser = commands.add_parser("serve", help="Serve workloads and collect result callbacks")
    serve_parser.add_argument("--source", type=Path, default=DEFAULT_SOURCE)
    serve_parser.add_argument("--port", type=int, default=8767)
    serve_parser.add_argument("--output", type=Path, default=ROOT / "artifacts" / "benchmarks")
    args = parser.parse_args()
    if args.command == "prepare":
        manifest = prepare(args.source)
        print(json.dumps({key: value for key, value in manifest.items() if key != "file_sha256"}, indent=2))
        return
    server = BenchmarkServer(("127.0.0.1", args.port), args.source, args.output)
    print(json.dumps({"url": f"http://127.0.0.1:{server.server_port}",
                      "output": str(server.collector.output), "source": str(server.source)}), flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
