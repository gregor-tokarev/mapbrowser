"""Local server checks. These do not launch a browser or claim benchmark results."""

import importlib.util
import json
from pathlib import Path
import tempfile
import threading
import unittest
from urllib.error import HTTPError
from urllib.request import Request, urlopen


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "benchmark_workload.py"
SPEC = importlib.util.spec_from_file_location("benchmark_workload", SCRIPT)
workload = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(workload)


class WorkloadServerTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        root = Path(self.temporary.name)
        self.source = root / "source"
        self.source.mkdir()
        (self.source / "resources").mkdir()
        self.html = b'<html><head><script src="resources/main.mjs" type="module"></script></head><body>Original</body></html>'
        (self.source / "index.html").write_bytes(self.html)
        (self.source / "resources" / "main.mjs").write_bytes(b"export const unchanged = true;\n")
        self.server = workload.BenchmarkServer(("127.0.0.1", 0), self.source, root / "output")
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.base = f"http://127.0.0.1:{self.server.server_port}"

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)
        self.temporary.cleanup()

    def get(self, path):
        with urlopen(self.base + path, timeout=5) as response:
            return response.read()

    def post(self, path, body):
        request = Request(self.base + path, data=json.dumps(body).encode(),
                          headers={"Content-Type": "application/json"})
        with urlopen(request, timeout=5) as response:
            return json.load(response)

    def test_injection_leaves_official_source_bytes_unchanged(self):
        html = self.get("/speedometer/")
        self.assertIn(b'<script src="/speedometer-harness.js"></script>', html)
        self.assertEqual((self.source / "index.html").read_bytes(), self.html)
        self.assertEqual(self.get("/speedometer/resources/main.mjs"), b"export const unchanged = true;\n")
        self.assertIn(b"Controlled browser workload", self.get("/workload.html"))

    def test_nested_directory_index_and_relative_query_paths(self):
        directory = self.source / "resources" / "perf.webkit.org" / "public" / "v3"
        directory.mkdir(parents=True)
        html = b'<html><script src="./app.js?version=3"></script><body>Charts</body></html>'
        (directory / "index.html").write_bytes(html)
        (directory / "app.js").write_bytes(b"window.chartReady = true;\n")
        base = "/speedometer/resources/perf.webkit.org/public/v3"
        self.assertEqual(self.get(base + "/?test=1"), html)
        self.assertEqual(self.get(base + "/app.js?version=3"), b"window.chartReady = true;\n")
        with urlopen(self.base + base + "?test=1", timeout=5) as response:
            self.assertEqual(response.url, self.base + base + "/?test=1")
            self.assertEqual(response.read(), html)
        request = Request(self.base + base + "/?test=1", method="HEAD")
        with urlopen(request, timeout=5) as response:
            self.assertEqual(response.getheader("Content-Length"), str(len(html)))
            self.assertEqual(response.read(), b"")
        self.assertEqual((directory / "index.html").read_bytes(), html)

    def test_shared_start_barrier_and_events_are_persisted(self):
        self.post("/api/event", {"run": "test-1", "event": "ready", "tab": "1"})
        # A pre-start check returns immediately and leaves browser connections
        # available for loading additional tabs.
        before = json.loads(self.get("/api/start-time?run=test-1"))
        self.assertIsNone(before["start_epoch_ms"])
        self.assertIn(b"Controlled browser workload", self.get("/workload.html"))
        started = self.post("/api/start", {"run": "test-1", "delay_ms": 20})
        after = json.loads(self.get("/api/start-time?run=test-1"))
        self.assertEqual(after["start_epoch_ms"], started["start_epoch_ms"])
        self.post("/api/event", {"run": "test-1", "event": "completed", "tab": "1", "score": 50})
        status = json.loads(self.get("/api/status?run=test-1"))
        self.assertEqual([event["event"] for event in status["events"]], ["ready", "completed"])
        self.assertIsInstance(status["events"][0]["server_monotonic_ns"], int)
        persisted = json.loads((self.server.collector.output / "test-1" / "status.json").read_text())
        self.assertEqual(persisted, status)
        with self.assertRaises(HTTPError) as error:
            self.post("/api/start", {"run": "test-1"})
        self.assertEqual(error.exception.code, 400)
        error.exception.close()

    def test_invalid_run_ids_and_path_traversal_are_rejected(self):
        with self.assertRaises(HTTPError) as error:
            self.post("/api/event", {"run": "../outside", "event": "ready"})
        self.assertEqual(error.exception.code, 400)
        error.exception.close()
        with self.assertRaises(HTTPError) as error:
            self.get("/speedometer/../private")
        self.assertEqual(error.exception.code, 404)
        error.exception.close()


if __name__ == "__main__":
    unittest.main()
