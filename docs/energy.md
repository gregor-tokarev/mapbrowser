# Energy design

Battery life is Map Browser's main performance metric. The target is parity with [Helium](https://github.com/imputnet/helium), a Chromium browser known for low overhead. Pages are drawn by the same Chromium engine, so the differences come from four places: what the browser window does while you aren't touching it, which pages are loaded, what those pages download, and which background services Chromium runs.

None of the numbers below were measured on a Mac yet; the [measurement plan](#measuring-on-a-mac) describes how to do it.

## What the shell does

| Change | Why it matters |
|---|---|
| **Frames only on demand.** The vendored GPUI used to run a `CVDisplayLink` for every visible window, waking the main thread 60–120 times a second even when nothing changed, and to re-render the whole scene for a second after any mouse movement. The display link now runs only while something needs drawing and stops after eight idle refreshes. A watchdog replaces a link that stops delivering. Mouse movement alone never causes a frame. See `vendor/gpui/MAPBROWSER_PATCH.md`. | An idle window should cause no periodic wakeups at all. |
| **Lazy session restore.** At launch only the visible tab gets a page; other tabs load the first time you select them. Helium does the same. | The old prototype loaded every saved tab in every space at startup, and those pages then kept running. |
| **Redraw only for visible changes.** Title, address, loading and favicon events from tabs in other spaces update the saved state without redrawing the window, and so do address changes that keep the host (map pans, scroll spies), title changes of favorites (their tiles show no title), anything the hidden sidebar would show, and loading progress while a page is fullscreen. Chromium reports a load start and stop for every history update (pushState, replaceState, #hash) and every iframe load; only a main-frame load of a new document shows the loading bar, so the others redraw nothing unless Back or Forward changes. Scrolls that can't move the tab list don't redraw it. | Busy background pages that update their titles don't cost frames, nor does a chat favorite blinking its unread count, nor a map pan. |
| **Animated favicons paced.** A tab fetches a changed favicon at most every 2 s and encodes it off the main thread; a site's icon file is written at most every 10 s, and GPUI keeps only a site's last few icons decoded. | Pages that animate their icon (timers, unread badges) would cost a fetch, an encode, a disk write, two redraws and memory for every frame. |
| **Link status in a window of its own.** The hovered link's address shows in a small pop-up window that is made once, then shown and hidden; only it redraws as the pointer moves over links. | Hovering links never redraws the browser window, except for one frame when the pill appears or goes while a toast shows over the card (the toast moves above it). |
| **Shadows shaded only where seen.** The page card's shadow is drawn in strips along its edges; GPUI would otherwise shade it over the whole card, under the opaque page. | Millions of hidden shadow fragments on every frame of a large window. |
| **No animation at rest.** The loading bar moves only on Chromium's progress events; space switches and other transitions request frames only while running. | Spinners and pulsing indicators keep the GPU and display awake. |
| **Opaque window, one gradient.** No blur or vibrancy; the whole background is a single gradient quad. | Translucency makes the window server recomposite whenever anything behind or under the window changes. |
| **Corner caps instead of a rounded mask.** The page container is a plain clipped rectangle; four tiny caps in the background colour round the card. | A rounded mask over Chromium's layer tree can force an offscreen pass on every page frame and keep video from going direct to the display. |
| **Hidden pages are hidden for Chromium too.** Inactive tabs' views are hidden, and so is the view of a page the card covers with a sign-in, crash or error notice, which Chromium reports as `Visibility::HIDDEN`. | All standard background throttling applies: timers at most once a second, once a minute after five minutes, background CPU priority. |
| **Cheap saves.** State is serialised on the main thread and written on a background thread 500 ms after a change, or within 10 s when only pages changed titles or addresses. A pending save is never postponed, so constant changes can't starve it. Identical saves are skipped; routine saves use a barrier sync, and only quitting flushes the drive. | Saving on every click used to force full device flushes on the main thread, and a page retitling itself every second wrote the session every second. |
| **Resize work only where it shows.** Bounds changes resize only visible pages; others are sized when shown. | Resizing every hidden page forces layout in all background renderers. |
| **History saved like the session.** Visits go to `history.json`, apart from the session, on the same rules: snapshotted on the main thread, written on a background thread at most 10 s after a visit or a new title (500 ms after clearing), never postponed, and not at all when nothing changed. Changes of address within a page (fragments, the History API) and reloads aren't visits. A visit takes its page's titles only in its first 10 s, as Chrome does, so a page that keeps changing its title (a timer, an unread count) writes the file once or twice, not every 10 s as it does the session. | A single-page app rewriting its address, a map being panned or a ticking title writes nothing; browsing writes one file every few seconds at most. |
| **Sound indicator without timers.** A short script in each web page's frames listens for its audio and video elements' play, pause and volume events and reports only when the page starts or stops being audible. CEF's audio callback isn't used: it would take the sound from the speakers. | Showing which tab plays sound costs nothing while nothing starts or stops. |
| **One-off timers for passing UI.** The sidebar's peek from the window's edge waits on a single 150 ms timer and follows the pointer only while it lasts; toasts and the fullscreen note close on a single timer each. | Nothing polls the pointer or the clock while the window is idle. |

Pages you open keep running when you switch away. Map Browser never freezes or discards them automatically: chat and mail pages must keep receiving messages. "Unload Space", "Unload Tab" and the – on pinned tabs let you put pages to sleep yourself.

## What Chromium does

The app sets these switches in the browser process only. CEF writes its own `--disable-features` list first, and Chromium keeps only the last value of a repeated switch, so the lists are merged rather than appended.

| Switch or feature | Effect |
|---|---|
| `--disable-background-networking` | Stops Safe Browsing list updates, translate downloads, extension and upgrade checks |
| `disable-features=CPUMeasurementInFreezingPolicy,MemoryMeasurementInFreezingPolicy` | Chromium's Battery Saver would otherwise freeze busy background pages; this keeps pages running as promised |
| `disable-features=MediaRouter` | No Cast, DIAL or mDNS discovery |
| `disable-features=NetworkTimeServiceQuerying` | No periodic network time checks |
| `disable-features=AutofillServerCommunication` | No server query on every page with a form |
| `enable-features=BatterySaverModeAlignWakeUps` | While Battery Saver is active, timer wakeups are aligned to 32 ms |

The app also sets `--disable-chrome-login-prompt`, so sign-ins reach the page card; it has no effect on energy.

Chromium's Battery Saver stays on: below 20 % battery it caps page rendering at 30 frames a second and puts JavaScript into a battery-saving mode. The component updater stays on because it delivers certificate revocation lists and DRM; it checks every five hours.

Never add switches that disable background throttling, renderer backgrounding, occlusion detection or the back-forward cache.

## Ad and tracker blocking

Helium ships uBlock Origin. A 2026 study across 5,000 sites ([Pélissier et al., WWW 2026](https://doi.org/10.1145/3774904.3792414)) measured a median 9.6 % less client CPU energy with uBlock Origin, and 17.7 % less on news sites. Map Browser blocks with Brave's adblock-rust and the EasyList and EasyPrivacy network rules, built in and on by default:

- the main page is never blocked, only what it loads;
- matching happens on Chromium's IO thread in about 4 µs per request (release build, measured on Linux);
- the compiled rules are cached in the data directory, so later launches load them in tens of milliseconds;
- the engine adds about 8 MB of memory;
- a per-site switch turns blocking off.

Cosmetic rules (hiding empty ad boxes) and automatic list updates are not implemented yet.

## Where Helium still does better

Helium also freezes background tabs beyond a few recent ones and discards inactive tabs by default. Map Browser deliberately doesn't, so with many busy pages open it will use more energy than Helium.

## Measuring on a Mac

Use a release build, the same Mac, power mode, brightness, display refresh rate and window size for every run, and quit other apps.

1. **Shell wakeups.** With one quiet page (`make test-server`, `http://127.0.0.1:8765/quiet`), run `sudo powermetrics --samplers tasks --show-process-wakeups -i 1000 -n 30`. Check the main process at rest, while moving the pointer over the sidebar, and while moving it over the page. Expect no periodic wakeups at rest. Activity Monitor's Energy tab ("Idle Wake Ups") gives a rough view.
2. **Display link.** In Instruments (System Trace), the CVDisplayLink thread should be idle when nothing changes.
3. **Background traffic.** Launch with `--log-net-log=/tmp/mb-netlog.json`. Leave two spaces with quiet pages idle for 30 minutes, then list the hosts contacted. Expect no Safe Browsing, time or autofill requests.
4. **Background pages.** With the heartbeat fixture in a hidden space, expect about one request a second, dropping to one a minute after five minutes. With Battery Saver forced on (`performance_tuning.battery_saver_mode.state = 3`), a busy hidden page must keep running.
5. **Corner caps.** Compare GPU power with 1080p video and with scrolling against a build that uses a rounded mask.
6. **Blocking.** Load 20 ad-heavy news pages in sequence with a 60 s dwell, blocking on and off, five times each, and compare energy per page.
7. **Battery rundown.** Unplugged, fixed brightness, Wi-Fi on: run the same scripted workload in Map Browser, Helium and Chrome at least three times each and compare minutes to 20 %.
