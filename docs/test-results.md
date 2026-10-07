# Prototype verification results

Native application testing finished on September 12, 2026. The final tested build identifier is `37cb9835a39523bf991b396b1bc85aeac33a974061c6243edf1fb8b3ce07f682`. The broader interaction tests, animation timing, and idle CPU observations described below used preceding build `131205c5`; the final build repeated first paint, navigation, tab creation, workspace switching, and quit checks after the initialization fix.

This is a development prototype built with Rust, GPUI 0.2.2, and CEF 152.0.6 through the Rust `cef` 152.2.0 bindings on macOS. Cookie persistence is blocked in the tested environment after denied Keychain access, and battery performance remains unverified. The recorded checks do not establish readiness for daily browsing.

## Completed checks

| Check | Result | Evidence |
| --- | --- | --- |
| Application model tests | 13 passed, none failed | [Final unit test output](../artifacts/unit-tests-final.log). Covers workspace and tab selection, profile paths, session restoration and repair, and address handling. The log includes four dead-code warnings and an upstream future compatibility notice. |
| Fixture storage endpoint | Passed | Two independent HTTP clients retained separate fixture cookie values. Save, reload through a fresh request, and clear succeeded. |
| Fixture cookie investigation | Passed in the HTTP control and native UI checks | A separate `curl` client used a new temporary cookie jar against the running fixture. The response set `mapbrowser_qa=work-only` with `HttpOnly; Max-Age=31536000; Path=/; SameSite=Lax`. The next request returned `work-only`; an independent request returned an empty value. Native screenshots later showed distinct saved values in the workspace pages. No fixture code change was needed. |
| Fixture heartbeat evidence | Passed at the server endpoint | The endpoint recorded increasing ticks and rejected duplicate tick numbers. This verifies the fixture's ability to capture activity; it does not by itself establish background workspace behavior. |
| Fixture navigation and quiet baseline | Passed at the HTTP endpoints | First, second, and quiet documents returned their expected titles. The quiet page contains no script or timers. |
| Process sampler | Passed | A real two-second run produced three timed process-tree samples and a JSON report. The run tested the sampler, not browser performance. |
| Animation analyzer | Passed | Parsed existing application logs, preserved a duration outlier, and reported malformed JSON and reversed timestamps. A controlled input with 5 ms and 35 ms gaps produced a 20 ms median and a 33.5 ms interpolated p95. |

The fixture is available through `python3 scripts/test_server.py`. Reproduction steps for application behavior are in the [test plan](test-plan.md).

## Native computer use observations

On build `131205c5`, address navigation, Command-T, back, forward, and reload passed through the native browser controls. A new tab in Work inherited that workspace's cookie and local storage. After the input fix, Command-A replaced `replace-this` with `personal-final` inside the actual web page input. Work independently saved `work-final`. These are synthetic fixture values. The earlier appended Personal value in the background evidence predates this input fix.

Scrolling Work, switching to Personal, and returning preserved Work's scroll position while Personal stayed at the top. Switching between native web content and blank pages, closing the final blank tab, window zoom and restore, switching after resize, and four rapid workspace switches all reached the correct endpoint. Completed animation records supplement those native observations; interrupted transitions do not produce a completion record.

Local storage survived a prior restart. Encrypted cookies did not survive that restart because Keychain access was denied; cookie and login persistence remain blocked in this test environment.

On the final build, first paint passed after denying the Keychain prompt and raising or activating the native window. The tester did not resize or reload the page. Two further fresh quiet-page tabs rendered directly. Creating a blank `SlideQA` workspace and repeatedly switching between its blank content and native web content also reached the correct endpoints. Command-Q exited with code 0 and no Mapbrowser parent or helper processes remained. The [final native trace](../artifacts/qa-initial-surface.log) records 21 ms between the quit request and return from the native event loop.

## Initial page rendering

The final build applies an experimental initialization workaround after a browser instance's first real main-frame load. It changes the native view width by one logical pixel and restores it once. The [native trace](../artifacts/qa-initial-surface.log) contains exactly three initialization markers for three browser instances. Subsequent navigation and Back did not add a marker.

The workaround does not poll, start a recurring timer, reload the page, or inject JavaScript. The cold-launch checks show that it restored initial rendering in the tested setup. They do not prove that geometry alone caused the original failure or establish that every native view lifecycle is covered.

## Background requests

The [background check](../artifacts/background-check.json) groups actual requests from the local fixture server by document instance and the page's reported `document.visibilityState`. These observations were collected on September 12, 2026. Each workspace label was mapped to its document instance during native UI testing.

| Workspace and state | Server evidence | Cookie value |
| --- | --- | --- |
| Work hidden while Personal reported visible | The same Work instance sent 20 requests, ticks 316 through 335, over 19.000 seconds. Median request gap was 1.000 second; maximum was 1.004 seconds. | `work-only` |
| Personal hidden while Work reported visible | The same Personal instance sent 12 requests, ticks 436 through 447, during the 11.493-second visibility overlap. Its full hidden segment contains 41 requests over 40.001 seconds, with a 1.000-second median gap and 1.006-second maximum. | `work-onlypersonal-only` |

These requests establish that the inactive workspace pages continued executing and making requests during the observed intervals. The evidence comes from requests the server received while the pages reported hidden, rather than counters read after reactivation. The history retains only 500 events. An additional hidden document, which was not independently mapped to a workspace, had a 26-second request gap; it remains in the report. Background timing is not guaranteed to stay at one second.

## Animation timing

The [animation report for build 131205c5](../artifacts/animation-131205c5.json) contains six completed transitions from the [archived preview log](../artifacts/preview-131205c5.log). Durations range from 241.5018 to 247.3768 ms. Across 173 consecutive GPUI render callback gaps, p50 is 8.3305 ms, p95 is 9.1722 ms, and maximum is 9.3347 ms. No gap exceeds 16.7 ms or 33.3 ms, and no transition exceeds the two-second outlier threshold.

The earlier [application log](../artifacts/app.log), captured before the integration fixes, contains a 123632.7340 ms transition. The analyzer preserves durations above two seconds as outliers and excludes them only from the primary summary. An occluded window can delay callbacks, but timing logs alone do not establish the cause. The earlier capture remains available and is separate from the six transitions recorded on build `131205c5`.

Render callback timestamps do not establish frame presentation or visible animation smoothness. A recording, if supplied, must state its capture-rate limitation. The logs do not contain interrupted or unfinished transitions.

## Open verification items

- Encrypted cookie persistence is blocked after denied Keychain access. The successful local storage restart check does not establish cookie or login persistence.
- The one-time initial rendering workaround needs broader native lifecycle testing. First paint passed in the recorded cold launch and two additional tabs.
- The animation timing and Mapbrowser idle diagnostic cover preceding build `131205c5`; the final build has native behavior checks but no new controlled energy measurement.
- Battery parity with Chrome or Helium and an improvement over Arc require repeated energy or battery tests under matched workloads.

## Keychain limitation

The bundled CEF runtime uses the shared `Chromium Safe Storage` Keychain item and can ask macOS for access. This prototype does not have a supported Mapbrowser-specific Keychain item setting in its current runtime. During this session access was denied, the engine logged `userCanceledErr` and unavailable encryption, and cookies were empty after restart while local storage remained intact. Cookie and login persistence cannot be marked passed. Separate profile directories do not provide separate encryption key names. See the [Keychain limitation](keychain.md) for the verified upstream details and the planned runtime update.

## Energy status

Battery parity with Chrome or Helium and improvement over Arc remain unverified. CPU time and RSS do not measure battery drain, GPU energy, or wakeups. The application samples are diagnostic observations only. A controlled comparison needs repeated runs of the same workload and a separate energy or battery measurement, as described in the [test plan](test-plan.md).

Both idle diagnostics used fresh profiles, one visible quiet fixture page, and Chromium 152.0.7977.83. The sampler collected each main browser process and its observed helper processes once per second.

| Browser | Observed duration | Mean sampled CPU, percent of one core | Observed CPU seconds | Peak sum of process RSS |
| --- | ---: | ---: | ---: | ---: |
| [Mapbrowser build 131205c5](../artifacts/map-idle.json) | 60.051 seconds | 1.003% | 0.60 | 703.391 MiB |
| [Chrome 152.0.7977.83](../artifacts/chrome-idle.json) | 60.035 seconds | 2.429% | 1.46 | 1169.969 MiB |

The Mapbrowser run had no debug port or animation tracing, Keychain access was denied, and one reload corrected the initial-paint issue before timing began. Its numbers apply to preceding build `131205c5`, which predates the final initial-rendering workaround.

The Mac was charging, other user applications remained open, the browser window sizes were not standardized, and no repeated trials were run. These conditions make this an idle sanity check, not an energy acceptance test. Summed RSS double-counts shared pages, and sampled CPU deltas miss some short-lived process work. The results do not establish battery life or performance under active browsing workloads.
