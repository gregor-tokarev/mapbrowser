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

## Arc redesign checks on a Mac

The redesign was built on a machine without macOS: it type-checks for macOS and its interface was exercised in the Linux preview, but none of the native behaviour below has run yet. Check each item on a release build with a fresh data directory, then with a copy of an existing one (schema 1 sessions migrate to schema 2).

### Window and sidebar

1. Traffic lights sit at the top left, centred on the navigation row, fully visible and clickable. ⌘S hides the sidebar and the traffic lights together, and brings both back.
2. The sidebar toggle, back, forward and reload buttons respond to clicks even though they overlap the native titlebar strip. Back and forward dim when unavailable.
3. Dragging empty sidebar space moves the window. Double-clicking empty sidebar space opens the command bar.
4. Hover highlights, close buttons, Clear and tooltips appear in the sidebar while a web page has keyboard focus, and clear when the pointer moves onto the page or out of the window. Over the page, its own cursor (I-beam, hand) stays while the sidebar redraws during a load.
5. Clicking × repeatedly without moving the pointer closes one tab after another. Space switcher and toolbar tooltips stay inside the sidebar, also for long space names.
6. The page card's corners are rounded with no light or dark notches, in light and dark appearance, mid-way through a space switch, and after a theme change. Clicks and scrolls in the card's corners reach the page.
7. During a space switch the sliding pages stay inside the card and never cover the sidebar.
8. Dragging the sidebar edge resizes it smoothly and the page follows; double-clicking the edge resets it; the width survives a restart.
9. A two-finger sideways swipe on the sidebar switches space once per gesture, in the expected direction with natural scrolling on. Vertical scrolling of a long tab list still works.
10. Drag and drop moves tabs within and between favorites, pinned and Today. Dropping over the page cancels.
11. A tab, space, library or + menu closes when the page is clicked, and Esc then reaches the page.
12. Rename Space (offered only for the active space): typing goes to the field; clicking the page, pressing ⌘1 or selecting a tab ends the rename and keeps the name; clicking back into the field resumes typing there.
13. Menu bar: ⌘H hides the app, ⌥⌘H hides others, ⌘M minimizes (also with the sidebar hidden), Window > Zoom and Bring All to Front work, and Edit > Undo undoes typing in a web form.
14. The space menu (… beside the name, or a right-click on a space dot) ends with Delete Space…, greyed out when only one space is left. It turns into "Delete <name>? Its tabs and site data will be removed." in the sidebar; Cancel and Esc keep the space. Delete closes its pages, moves to the next space if it was the active one, and leaves the other spaces' tabs and logins alone. ⇧⌘T no longer offers its closed tabs.
15. After deleting a space, quit and reopen: `profiles/space-<id>` of the deleted space is gone and every other space keeps its logins. Delete one, then start a second copy with `open -n` before quitting the first: the second copy removes nothing.
16. Resize the window, move it to a second display and quit: it reopens there at that size. Fullscreen from the green button reopens in fullscreen; a video's fullscreen doesn't. With that display disconnected, it opens centred on the built-in display.
17. While renaming a space, ⌘Q or the red close button keeps the name typed so far.
18. Right-clicking a tab opens its menu just below the row or favorite tile, never over its title, and the row stays highlighted while the menu is open. Pointing at a space dot or favorite and right-clicking at once shows no tooltip over the menu.

### Command bar

1. ⌘T opens the command bar over the page, focused and accepting typing. ⌘T or Esc closes it; so does a click on the page or sidebar. It moves with the window and appears on the right display.
2. ⌘L opens it with the full address selected. Return on the unchanged address reloads; an edited address navigates.
3. Results: typing a domain opens it, a phrase searches, an open tab in another space switches to that space and tab, and actions run. `node.js`, `e.g.` and `me@example.com` search; `foo.dev`, `localhost:8000` and `raspberrypi.local` open.
4. With the bar open, clicking the URL pill (after ⌘L) or + New Tab (after ⌘T) closes it, and it stays closed.

### Pages and tabs

1. Only the visible tab loads at launch; selecting another tab loads it with its saved title and favicon shown meanwhile.
2. ⌘-click, middle-click and ⇧-click on links open background or foreground tabs and leave the current page alone. `target=_blank` links open tabs. A link's context menu offers Open Link in New Tab and Copy Link Address.
3. Favicons appear after a page loads and on unloaded tabs after a restart.
4. The loading bar above the card moves during loads and disappears afterwards. After a failed load (⌘L `exmaple.com`), Back removes the red notice. ⌘L with a download link starts the download and the tab keeps its page and address.
5. ⌘W on a pinned tab or favorite unloads it and keeps it in the sidebar. ⌘W on a Today tab removes it; ⇧⌘T restores it in the same space.
6. HTML fullscreen video fills the screen without the sidebar or card corners, and Esc returns to the normal layout. Leaving window fullscreen by other means also restores it.
7. Keyboard shortcuts work both with the page focused and with the sidebar focused, including on AZERTY, Dvorak and Russian layouts: ⌘Z undoes in a web form, ⌘W closes the tab, ⌘Q quits, ⌘A selects all.
8. Closing the window with the red button saves the latest tab changes, like ⌘Q.
9. ⌃Tab, with the page focused and with the sidebar focused, goes back to the tab you were on, and pressed again returns. Holding ⌃ and pressing Tab again outlines older tabs of the space in the sidebar without loading them; releasing ⌃ goes to the outlined one. ⌃⇧Tab walks the other way. Releasing ⌃ in another app cancels the walk. ⌥⌘↓ and ⌥⌘↑ still follow the sidebar order.

### Blocking and Chromium switches

1. With `MAPBROWSER_TRACE_CEF=1 make dev`, the log reports the content blocker as ready.
2. With `MAPBROWSER_DEBUG_PORT` set, a news site's ad and tracker requests fail in DevTools, ad slots stay empty rather than showing error pages, and typing an ad domain in the address still opens it.
3. The shield beside the address turns blocking off for the site and its subdomains after a reload, and back on.
4. `chrome://version` shows `--disable-background-networking`, `disable-features` with CEF's own entries (GlicActorUi, AutofillActorMode, LensOverlay, KillOnInvalidNavigationHeaders) plus CPUMeasurementInFreezingPolicy, MemoryMeasurementInFreezingPolicy, MediaRouter, NetworkTimeServiceQuerying and AutofillServerCommunication, and `enable-features` with BatterySaverModeAlignWakeUps.

### Energy

Follow the measuring section of [energy design](energy.md). The first check is the idle one: with one quiet page visible, the main process should show no periodic wakeups, also after pressing a command-bar result, moving slightly and releasing.

Display sleep: on power, with audio playing in one tab and another tab whose title keeps changing, run `pmset displaysleepnow` and leave the display off for ten minutes (once with "Require password" set to a delay or off, so the window is not hidden by the lock screen). While it is off, the main process shows no periodic wakeups and the log no "display link" warnings. On waking, the sidebar and an open command bar redraw at once, without a freeze of several seconds, and show the latest titles.

With `sudo powermetrics --samplers tasks --show-process-wakeups`, a page that changes `document.title` every second causes no periodic main-process wakeups in two setups: as the visible page with the sidebar hidden, and as a favorite that isn't selected with the sidebar shown. The favorite's tooltip still shows its current title.
