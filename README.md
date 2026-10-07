# Map Browser

A macOS browser prototype with a Rust/GPUI interface and embedded Chromium. The sidebar holds vertical tabs and spaces. Each space has an independent, persistent Chromium profile, so the same site can use different accounts in different spaces.

## Build and run

Requires macOS, the Rust toolchain, Xcode with its command-line tools and Metal toolchain, CMake, and Ninja. The first Cargo build downloads the matching CEF runtime into `target/cef`; allow several minutes and a few GB of disk space.

```sh
brew install cmake ninja
make run
```

`make run` builds and opens the normal optimized release app. Use `make build` to create the bundle without opening it. `make dev` builds a debug bundle and runs its executable in the foreground, keeping logs and environment flags in the terminal. Run `make help` to list the targets.

Quit the running app before rebuilding its bundle.

The bundle contains Chromium, its sandboxed helper processes, and a local ad hoc signature. Run the `.app`, rather than `cargo run`, because Chromium needs that bundle structure. This is a local prototype, not a notarized distribution or an automatically updated browser.

## Use

- Enter an address or search in the address field. Bare domains use HTTPS; loopback addresses use HTTP.
- Use **New tab** or `⌘T`. Close a tab with its × or `⌘W`.
- Select a space at the bottom of the sidebar. Use + to create a space and ··· beside its name to rename it.
- Use `⌘⌥←` and `⌘⌥→` to switch spaces with a 240 ms slide. `Ctrl+Tab` switches tabs within the current space.
- Use `⌘L` to edit the address, `⌘R` to reload, and `⌘[` / `⌘]` for back / forward.

Switching spaces keeps opened pages alive. Chromium can apply its normal background scheduling, but the app does not discard or suspend pages when a space becomes inactive. Blank new tabs use GPUI and do not create a page renderer until you navigate.

State lives in `~/Library/Application Support/MapBrowser` unless `MAPBROWSER_DATA_DIR` is set. Spaces, tab URLs, and selections restore on launch. Cookies and site storage persist in separate profile directories. In-page navigation history is not restored across application restarts.

## macOS Keychain

The bundled CEF 152 runtime may ask to access the existing **Chromium Safe Storage** Keychain item. Chromium uses its secret to encrypt stored browser data. Each space has a separate profile, but the runtime uses the same encryption key name across profiles and other applications using that default.

Choose **Deny** if you do not want this prototype to access that shared item. Do not rely on encrypted login/session persistence when access is denied. This build has no supported setting for a Mapbrowser-specific Keychain item, and it does not disable encryption to suppress the prompt. See [the limitation and upstream fix](docs/keychain.md).

## Apple Passwords and Touch ID

The default build cannot use passkeys saved in Apple Passwords or iCloud Keychain. That integration requires Apple's approved browser passkey entitlement, a matching signing identity and provisioning profile, and a Chromium native-window handoff for the GPUI host. Touch ID confirms use of the passkey; Apple Passwords stores it. See [the implementation status and setup](docs/passkeys.md).

## Verify

```sh
make test
make check
make test-server
```

`make check` verifies formatting and checks all Cargo targets. Use `make fmt` to apply formatting.

Open `http://127.0.0.1:8765/` in two spaces. Save different test values in each. The page shows server cookies, local storage, and background activity. See [the test plan](docs/test-plan.md) for native UI checks and a controlled energy comparison.

Set `MAPBROWSER_TRACE_ANIMATION=1` when launching the bundled executable to print per-transition render timestamps. The animation requests frames only until the transition ends and respects macOS Reduce Motion. Chromium uses its native macOS message loop through the GPUI application. There is no fixed polling timer for browser work.

CEF 152 needs a native compositor initialization workaround in this host: after the first real main-frame load, the app changes the native view width by one logical pixel and immediately restores it. This runs once per browser instance, without reloading the page or scheduling continuing work. `MAPBROWSER_TRACE_CEF=1` records the initial native geometry and this adjustment.

```sh
MAPBROWSER_DATA_DIR="$PWD/.test-data/manual" \
MAPBROWSER_TRACE_ANIMATION=1 \
make dev
```

Battery life is an acceptance criterion, not a demonstrated result. Short CPU/RSS samples can reveal excess idle activity, but cannot establish parity with Chrome or Helium, or lower battery drain than Arc. That needs repeated energy measurements with matching workloads.

`MAPBROWSER_DEBUG_PORT=9223` optionally enables the Chromium debugging endpoint for local page tests. It is off by default. Use a separate test data directory when enabling it.

## Scope

This version includes browsing, spaces, tabs, profile isolation, session restoration, and workspace animation. It does not include extensions, split view, sync, bookmarks, a download manager, password management, or an automatic Chromium updater. Website dialogs, media permissions, DRM, and browser integrations need further testing before daily use.

The implementation uses [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui), [CEF](https://github.com/chromiumembedded/cef), and [the Rust CEF bindings](https://github.com/tauri-apps/cef-rs). The adapted text field retains its [GPUI license](licenses/GPUI-Apache-2.0.txt).
