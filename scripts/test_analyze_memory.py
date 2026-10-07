import copy
import unittest

from analyze_memory import summarize


class MemorySummaryTests(unittest.TestCase):
    def setUp(self):
        self.report = {
            "label": "test", "root_pids": [1], "requested_interval_seconds": 1,
            "samples": [{
                "elapsed_seconds": i + 0.02,
                "frontmost_application": {"bundle_identifier": "test.browser"},
                "read_errors": [],
                "processes": [{"pid": 1, "command": "browser", "phys_footprint_bytes": 2**20},
                              {"pid": 2, "command": "Helper (Renderer)", "phys_footprint_bytes": 2**21}],
            } for i in range(61)],
        }

    def test_fixed_interval_includes_helpers_and_excludes_startup(self):
        self.report["samples"][0]["processes"][0]["phys_footprint_bytes"] = 2**30
        result = summarize(self.report, "test.browser")
        self.assertEqual(result["sample_count"], 30)
        self.assertEqual(result["footprint_mib"]["whole_browser"]["mean"], 3)
        self.assertEqual(result["footprint_mib"]["renderers"]["mean"], 2)

    def test_invalid_capture_is_rejected(self):
        for mutation in ("background", "missing_focus", "read_error", "truncated"):
            with self.subTest(mutation=mutation):
                report = copy.deepcopy(self.report)
                sample = report["samples"][40]
                if mutation == "background":
                    sample["frontmost_application"]["bundle_identifier"] = "other"
                elif mutation == "missing_focus":
                    del sample["frontmost_application"]
                elif mutation == "read_error":
                    sample["read_errors"] = ["unreadable helper"]
                else:
                    report["samples"] = report["samples"][:40]
                with self.assertRaises(ValueError):
                    summarize(report, "test.browser")


if __name__ == "__main__":
    unittest.main()
