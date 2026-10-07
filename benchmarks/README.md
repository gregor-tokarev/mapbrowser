# Browser benchmark workloads

The Python server serves the same local content to Map Browser, Arc, Chrome and Safari. It never opens a browser, changes profiles or uses CDP. Browser pages POST their own start and completion records to the collector.

Speedometer measures web application responsiveness. The synthetic fixtures provide controlled intervals for external CPU, memory and energy sampling. Neither a Speedometer score nor CPU time alone establishes battery life.

## Start the server

```sh
python3 scripts/benchmark_workload.py prepare
python3 scripts/benchmark_workload.py serve --port 8767 --output artifacts/benchmarks
```

`prepare` downloads the official [Speedometer release/3.1 source](https://github.com/WebKit/Speedometer/tree/1386415be8fef2f6b6bbdbe1828872471c5d802a), pinned at commit `1386415be8fef2f6b6bbdbe1828872471c5d802a`. It downloads 12,358,793 bytes and extracts 61,022,359 bytes. The archive SHA256 is `cfefa818d7789ed2f2b9f3f1bf608cc35e4e8241489eef47049296ce92791313`.

The source is stored under ignored `benchmarks/vendor/speedometer-3.1`. Its provenance manifest includes SHA256 hashes of every file. Running `prepare` again verifies all files. No npm installation or build is needed. The server injects the harness script into its response for `index.html`; every source file on disk remains unchanged, and all other files are served unchanged.

## Speedometer 3.1

Open a fresh URL for each run, changing the browser label and run ID:

```text
http://127.0.0.1:8767/speedometer/?startAutomatically&mb_run=chrome-speed-1&mb_browser=chrome&mb_barrier=1
```

The page reports `ready`, then checks the nonblocking start-time endpoint once per second until the controller schedules the run. Polling stops as soon as the future start time is known, before measured work begins. The default official workload uses ten iterations and an 800×600 benchmark iframe. Keep the outer page viewport at least 850×650. Do not pass suite filters, custom timing settings or iteration overrides when reporting a standard run.

For a run that starts automatically after five seconds, replace `mb_barrier=1` with `mb_delay=5000`. The harness removes its `mb_` parameters before the official parameter parser runs, so navigating to a fresh URL with a fresh run ID is required for the next run.

The harness wraps the official exposed `benchmarkClient.start`, `didFinishLastIteration` and `handleError` methods. It delegates to the original methods, performs no repeated polling during measured work, and POSTs full official metrics after completion. Standard score values are in `metrics.Score`, with the aggregate also copied to `score`.

The official [test instructions](https://browserbench.org/Speedometer3.1/instructions.html) call for a focused page, no other active tabs or programs, consistent browser settings, AC power and cooling time between runs. Match those conditions when comparing performance. Energy runs should record their power state separately and apply the same conditions to every browser.

## Fixed-duration workloads

An idle page:

```text
http://127.0.0.1:8767/workload.html?run=chrome-idle-1&browser=chrome&tab=1&mode=idle&duration=210
```

For ten tabs, use the same run ID and `tab=1` through `tab=10`. Every tab must report `ready` before the controller sends the start command. Keep the final intended foreground tab selected throughout the interval.

Available modes:

| Mode | Work during the interval | Completion fields |
| --- | --- | --- |
| `idle` | Static document with 160 table rows and a static canvas. Only the final completion timer remains scheduled. | Actual elapsed time and visibility changes |
| `mixed` | Every second, 20,000 integer operations and one table-cell update. The title changes every tenth update. | Delivered ticks, checksum and visibility changes |
| `canvas` | A foreground animation draws 96 circles on a 620×220 logical-pixel canvas each animation frame, at the device pixel ratio. | Frame count, frames per second and frame-interval median/p95 |

Fixtures keep normal browser background throttling enabled. Compare delivered tick/frame counts alongside resource use. A browser doing less work because it throttles more is not necessarily more efficient per completed operation. No workers, media, network polling or hidden activity overrides are installed.

All tabs use the same server start time. Their one-second pre-start polls return immediately and stop once that time is known; no request remains held open while other tabs load. A 45- or 60-second controller delay allows background tabs time to observe the scheduled start and settle. Hidden-tab timer policies can delay the actual start or finish; each page records both scheduled and actual timestamps. A 210-second fixture allows a central 180-second sample with 15 seconds at either end excluded. This keeps page loading and callback requests outside that measurement window.

For quick checks without a shared barrier, append `&barrier=0&delay_ms=1000`.

### Optional static-tab suspension protocol

Some browsers suspend background page timers after the document has rendered. For a comparison of loaded static tabs, explicitly pass `--allow-suspended-idle-tabs` to both `scripts/benchmark_phase.py` and `scripts/analyze_benchmarks.py`. The option applies only to 210-second `mode=idle` fixtures. It does not relax Speedometer, mixed or canvas workload requirements.

This mode requires every expected document to report ready before the scheduled start and one stable, visible, focused foreground tab to complete. The foreground tab's actual start anchors the central 180-second window. Background tabs may lack start or completion callbacks; the report labels them `suspended_or_unobserved_timer_delivery`. A background tab that resumes after measurement does not move the measurement bounds. Observed background focus or visibility changes during the steady window still invalidate the run.

The controller can stop after foreground completion and records `completed_with_suspended_idle_tabs`. If an older controller was manually stopped after the foreground completed, analysis can retain a warning when its report proves a normal sampler flush after that completion. No missing callback is fabricated. Post-measurement UI checks should verify that suspended tabs resume their original document without a reload, and retain that evidence beside the metrics.

This compares loaded static documents under each browser's background policy. It does not establish equal active background execution, notification reliability or battery life. Report Safari's suspension behavior alongside comparisons when it differs from other browsers.

## Controller API

Check readiness:

```sh
curl -s 'http://127.0.0.1:8767/api/status?run=chrome-idle-1'
```

Release all ready tabs five seconds from now:

```sh
curl -s -X POST http://127.0.0.1:8767/api/start \
  -H 'Content-Type: application/json' \
  -d '{"run":"chrome-idle-1","delay_ms":5000}'
```

The same command starts a Speedometer run that uses `mb_barrier=1`. A run can start only once. Use a fresh ID for every repetition. Avoid repeatedly fetching status during the measured interval; status is also available directly in files.

Results are written to `artifacts/benchmarks/<run>/events.jsonl` and `status.json`. Each event includes browser-provided wall/performance timestamps, UA, viewport, visibility and focus, plus server UTC, epoch and monotonic receipt timestamps. Completion events include visibility/focus changes during the run. `/api/source` returns the pinned source metadata.

`GET /api/start-time?run=<id>` always returns immediately, with `start_epoch_ms: null` before release and the scheduled timestamp afterward. Holding one HTTP request per waiting tab can exhaust a browser's per-origin connection pool, so the fixtures use short polls exclusively before the scheduled start is known.

## Checks without a browser

```sh
python3 -m unittest benchmarks/test_workload_server.py -v
node benchmarks/test_speedometer_harness.cjs
```

These verify HTTP injection without source mutation, shared starts, event persistence, path validation and callback delegation. Actual browser execution must still be checked through the application UI.
