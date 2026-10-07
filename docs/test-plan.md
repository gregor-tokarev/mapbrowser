# Browser verification

The local fixture exercises browser behavior without using personal accounts. Start it with `python3 scripts/test_server.py`, then open `http://127.0.0.1:8765/` in the browser. The server binds only to the loopback interface. Keep that exact origin in every workspace; `localhost` and `127.0.0.1` have different storage.

## Computer use checks

Run these checks through the native window. Record the build, macOS version, display refresh rate, and test time alongside screenshots or a screen recording. Screenshots show state but cannot establish animation smoothness.

1. Navigate to the fixture through the address field. Type, select text, copy and paste, and submit with Return. Verify that the document title appears on the tab.
2. Save `work-only` in the first workspace. Both the server cookie and local storage must show that value. Reload and confirm they persist.
3. Open the same origin in a second tab in this workspace. Both saved values must match. The page instance identifiers should differ.
4. Open the same origin in another workspace. Both values must initially be empty. Save `personal-only`, return to the first workspace, and confirm it still shows `work-only`.
5. Quit the browser normally, reopen it, and check both workspaces. Their tabs and separate values must survive. Use a normal quit so the engine can flush its profile data.
6. Follow the Second page link, go back, and go forward. Check the address and title at each step. Scroll to the bottom with the trackpad and keyboard, then select text.
7. Leave the first workspace's fixture open, switch to the second workspace, and wait at least 15 seconds. Inspect `http://127.0.0.1:8765/api/heartbeats` from a separate browser or `curl`. The first page instance must keep sending increasing ticks while its workspace is inactive. The history gives server-side evidence, so a counter that catches up only after switching back is insufficient. Browser timer throttling can increase the interval; exact one-second timing is not required.
8. Close a tab and check that the adjacent tab activates without losing its document state. Open a new tab and check its initial navigation. Repeat with a workspace's final tab.
9. Resize the window, maximize it, and restore it. The web page and sidebar must keep their bounds. Check text entry after each resize.

## Workspace sliding animation

Give each workspace distinct tab titles and fixture values. A recording should include the sidebar, page content, and pointer throughout the switch.

1. Switch to the next workspace, then back. Confirm the motion changes direction, the destination workspace is marked active, and the destination page accepts input after the animation ends.
2. Switch several times in rapid succession, including a reversal before the preceding animation finishes. The final selection, sidebar contents, address, and visible page must agree. Look for blank frames, stale content, duplicate pages, jumps, or a stuck input state.
3. Repeat after scrolling the page, and after resizing the window. Each workspace should retain its page scroll and text input state.
4. Stop switching and leave the window idle. The animation must stop scheduling redraws after completion. Compare the process sampler before, during, and after switching to catch a persistent CPU increase.
5. If the implementation supports Reduce Motion, enable macOS Reduce Motion and verify its alternate behavior. Record lack of support as an outstanding accessibility issue rather than a passed check.

For frame-by-frame timing, inspect an actual screen recording at the display's supported capture rate. UI snapshots taken before and after a switch only establish the endpoints. Record any capture-rate limitation. Do not infer a 120 Hz result from a 60 fps recording.

## CPU and memory observations

Find the main app PID in Activity Monitor or use `ps -axo pid,ppid,comm`. Pass the main PID to the sampler so it includes Chromium helpers.

```sh
python3 scripts/sample_processes.py --pid 12345 --duration 60 --output artifacts/idle.json --label 'Mapbrowser release build, one quiet page'
```

The sampler runs `ps` once per second by default and writes raw process samples and a summary. It does not change the browser. It tracks observed helpers after a parent exits. Short-lived processes between samples can be missed, and cumulative CPU deltas omit newly observed processes for their first interval. The RSS sum double-counts shared pages. These measurements can expose unexpected idle CPU use, but cannot prove battery parity.

## Controlled Chrome, Helium, and Arc comparison

Use a release build of Mapbrowser. Use dedicated test profiles in the comparison browsers, disable extensions, and leave personal browsing sessions out of the test. Do not clear or modify an existing personal profile. Chrome and Helium can be launched as separate test instances with `--user-data-dir` pointing to a new temporary directory. Confirm their process list uses that directory before collecting data. For Arc, use a dedicated macOS test account if an isolated test instance is unavailable.

Keep the Mac, OS version, browser versions, power mode, display brightness, display refresh rate, window size, and network conditions the same. Quit unrelated apps, allow startup activity to settle, and run one browser at a time. Record whether the Mac is plugged in. Use a plugged-in run for repeatable CPU observations, then a separate unplugged run for battery observations.

Use identical numbers of profiles and tabs when the browser supports them. A single-profile Chrome baseline does not isolate the cost of multiple workspace profiles. Run these workloads separately.

- One foreground `http://127.0.0.1:8765/quiet` tab for the idle baseline.
- The same count of quiet tabs spread across the same count of profiles.
- One interactive fixture in each profile, with the same number of active heartbeat pages in every browser.
- Repeated tab or workspace switches at a recorded cadence. Only compare operations each browser supports, and report differences in the tested behavior.

Warm each workload for two minutes, sample it for at least five minutes, and repeat each run three times in a different order. Use the same sampling tool for every browser. The heartbeat fixture deliberately creates ongoing network and timer activity, so report it separately from the quiet baseline.

For battery comparison, run a fixed workload for a longer period with the machine unplugged and the same starting charge range. Record elapsed time, battery percentage, and the operating system's energy observations. Longer repeated runs reduce the effect of coarse battery percentage readings. Where available, use Instruments energy profiling to investigate wakeups and CPU or GPU activity. Never describe short CPU/RSS samples as proof that Mapbrowser drains less battery than Arc or matches Chrome. A result remains provisional until repeated battery or energy measurements support it.
