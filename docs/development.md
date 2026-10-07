# Development

Map Browser is a Rust application. [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) draws the window and sidebar, and [CEF](https://github.com/chromiumembedded/cef) (Chromium 152, through [the Rust CEF bindings](https://github.com/tauri-apps/cef-rs)) renders pages in native child views. The browser runs on macOS only. Linux builds the same interface without Chromium, for previews and tests.

## Build and run on macOS

Requires the Rust toolchain, Xcode with its command-line tools and Metal toolchain, CMake, and Ninja. The first build downloads the matching CEF runtime into `target/cef`; allow several minutes and a few GB of disk space.

```sh
brew install cmake ninja
make run
```

| Target | What it does |
|---|---|
| `make run` | Build the optimized app bundle and open it |
| `make build` | Build and sign the bundle without opening it |
| `make dev` | Build a debug bundle and run it in the foreground with terminal logs |
| `make test` | Rust unit tests, bundle and signing tests |
| `make check` | Formatting and `cargo check --all-targets` |
| `make fmt` | Format the Rust sources |
| `make test-server` | Serve the local test pages on `http://127.0.0.1:8765/` |

Quit the running app before rebuilding its bundle. Run the `.app`, not `cargo run`: Chromium needs the bundle structure, its sandboxed helper processes, and the local ad hoc signature. The bundle also carries the filter lists from `resources/filters/` and the licences from `licenses/`.

## Environment variables

| Variable | Effect |
|---|---|
| `MAPBROWSER_DATA_DIR` | State and profiles directory. Default: `~/Library/Application Support/MapBrowser` |
| `MAPBROWSER_TRACE_ANIMATION=1` | Print render timestamps for each space switch |
| `MAPBROWSER_TRACE_CEF=1` | Trace native geometry, the first-paint workaround and content-blocker loading |
| `MAPBROWSER_DEBUG_PORT=9223` | Enable Chromium's remote debugging endpoint. Use a separate data directory |
| `MAPBROWSER_REDUCE_MOTION=1` | Linux only: stands in for the macOS Reduce Motion setting |

```sh
MAPBROWSER_DATA_DIR="$PWD/.test-data/manual" MAPBROWSER_TRACE_ANIMATION=1 make dev
```

## Linux preview

`cargo build` and `cargo test --bin mapbrowser` also work on Linux. There the engine is a stand-in that creates no web views: it reports titles, addresses and loading states for whatever you open, so the sidebar, command bar and menus can be exercised, but the page card stays empty. GPUI's Linux backend draws with Vulkan; under Xvfb, Mesa's lavapipe driver works. Use the preview for layout and behaviour, never for frame-rate or energy numbers.

## Code layout

| Path | Contents |
|---|---|
| `src/main.rs` | Entry point, actions, key bindings, menu bar, window creation |
| `src/app.rs` | Browser behaviour: tabs, spaces, lazy loading, command bar actions, saving |
| `src/ui/` | Sidebar, page card, command bar, menus, themes, icons, drag and drop, swipe |
| `src/model.rs` | Saved state (schema 2): spaces, tabs, sections, themes, sidebar, blocking exceptions, window frame; removing deleted spaces' profiles |
| `src/recent.rs` | Recently used tabs per space, for ⌃Tab |
| `src/favicons.rs` | Favicon cache keyed by host |
| `src/blocker.rs` | Ad and tracker blocking with adblock-rust |
| `src/engine/` | Chromium embedding (`mac.rs`), the Linux stand-in (`stub.rs`) and shared events |
| `src/native.rs` | AppKit bridge: CEF's message loop inside GPUI's application, window helpers, corner caps |
| `vendor/gpui/` | GPUI 0.2.2 with Metal memory, frame-scheduling and native-view input changes; see `MAPBROWSER_PATCH.md` |
| `assets/icons/` | Sidebar icons (Lucide, ISC licence) |

## Implementation notes

- **Native views.** Each tab's page is a native view inside a container that GPUI positions over the page card. GPUI cannot draw above those views, so menus and popovers stay inside the sidebar and the command bar is a separate child window.
- **Rounded card.** The page container is clipped to a rectangle; four small corner caps in the window's colour round it. A rounded layer mask over Chromium's layers would cost an offscreen pass on every page frame.
- **First paint.** CEF 152 needs a compositor workaround in this host: after the first real main-frame load, the app changes the native view width by one logical pixel and restores it, once per browser instance, without a reload or ongoing work.
- **Message loop.** Chromium runs its native macOS message loop through the GPUI application; there is no polling timer for browser work.
- **Frame scheduling.** The vendored GPUI runs its display link only while a frame is needed. See `vendor/gpui/MAPBROWSER_PATCH.md` and [energy design](energy.md).
- **Deleting a space.** Each space is a Chromium profile in `<data dir>/profiles/space-<id>`, and Chromium can keep a profile's files open until it quits. Deleting a space therefore only closes its pages and drops it from the session; its directory is removed at the next launch, before Chromium starts (`BrowserState::remove_orphaned_profiles`). That sweep removes only real directories directly in `profiles/` named exactly `space-<id>` with an id no space has; it never follows symlinks and never touches `profiles/default`. It runs only when the session was read from `state.json` itself: after recovering from the backup, or starting from defaults, a space whose data is still wanted may be missing from the session. It also waits while Chromium's `profiles/SingletonLock` exists, since another running instance (or one that crashed) may hold the profiles. Space ids are never reused, so a removed directory can't belong to a later space.
- **Recent tabs.** ⌃Tab walks the active space's five most recently shown tabs (kept in memory, filled up in sidebar order after a launch) and goes to the outlined one when ⌃ is released. The page usually has keyboard focus, so on macOS an AppKit flags-changed monitor reports ⌃'s release; GPUI's modifiers event covers the shell and Linux.
- **Window frame.** The window's frame, display UUID and user-chosen fullscreen are saved with the session (at most every 10 s while it moves, and on quit) and restored at launch, moved and shrunk onto that display. If the display is gone, the window opens centred on the main display.

## Verify

```sh
make test
make check
make test-server
```

Open `http://127.0.0.1:8765/` in two spaces and save different values in each: the page shows server cookies, local storage and background activity. See [the test plan](test-plan.md) for native checks, the redesign checklist and the energy comparison procedure.

## Platform limitations

- [macOS Keychain](keychain.md): the bundled CEF uses the shared "Chromium Safe Storage" Keychain item.
- [Apple Passwords and Touch ID](passkeys.md): passkeys stored in Apple Passwords need Apple's browser entitlement and a custom CEF build.

[Earlier prototype test results](test-results.md) and the [first thread report](thread-report.md) describe the September 2026 prototype before the redesign.
