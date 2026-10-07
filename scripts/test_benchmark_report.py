import unittest

from benchmark_report import match_energy


class EnergyScopeTests(unittest.TestCase):
    def test_app_row_is_not_a_whole_browser_measurement(self):
        run = {
            "run": "arc-idle-1", "browser": "arc",
            "summary": {"requested_start_epoch_ns": 0,
                        "requested_end_epoch_ns": 10_000_000_000},
        }
        reading = {"timestamp": "1970-01-01T00:00:05Z",
                   "browser": "arc", "energyImpact": "0,3"}
        for scope in (None, "top_level_app_row_only"):
            reading["process_scope"] = scope
            matched, diagnostics = match_energy([reading], [run])
            self.assertEqual(matched[run["run"]], [])
            self.assertEqual(diagnostics["unverified_process_scope"], 1)
        reading["process_scope"] = "whole_browser_verified"
        matched, diagnostics = match_energy([reading], [run])
        self.assertEqual(matched[run["run"]], [0.3])
        self.assertEqual(diagnostics["included_readings"], 1)

    def test_verified_scope_still_requires_matching_time(self):
        run = {"run": "arc-idle-1", "browser": "arc",
               "summary": {"requested_start_epoch_ns": 0,
                           "requested_end_epoch_ns": 10_000_000_000}}
        reading = {"timestamp": "1970-01-01T00:00:11Z", "browser": "arc",
                   "energyImpact": "0,3", "process_scope": "whole_browser_verified"}
        matched, diagnostics = match_energy([reading], [run])
        self.assertEqual(matched[run["run"]], [])
        self.assertEqual(diagnostics["outside_valid_windows"], 1)


if __name__ == "__main__":
    unittest.main()
