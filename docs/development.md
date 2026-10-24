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

`make build`, `make run` and `make dev` first quit a running copy of the app, which would otherwise keep running the old build (`open` only brings it to the front). Run the `.app` (`target/bundle/Map Browser.app`), not `cargo run`: Chromium needs the bundle structure, its sandboxed helper processes, and the local ad hoc signature. The bundle also carries the filter lists from `resources/filters/`, the licences from `licenses/` and the app icon. Its version comes from `Cargo.toml`.

The bundle used to be `Mapbrowser.app`. After switching, delete the old one: Launch Services may keep it registered for links and documents. The data folder is still `MapBrowser`, so spaces and logins carry over, but the ad hoc–signed app at its new path may ask for the "Chromium Safe Storage" Keychain item again.

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

A word in the address makes the stand-in act out what a real page would (see `src/engine/stub.rs`):

| Address contains | What happens |
|---|---|
| `download-demo` | Downloads a file; with `fail` it fails halfway, with `blob` it comes from a `blob:` address (no ↻). Files go to `<data dir>/Downloads`, never `~/Downloads` |
| `permission-demo` | Asks for camera and microphone; `location`, `notifications`, `fonts`, `windows`, `network`, `midi`, `sensors`, `idle`, `keyboard`, `pointer`, `storage` or `install` ask for those |
| `status-demo` | The pointer rests on a long link |
| `auth-demo` | Asks for a username and password; `proxy` as a proxy, `http:` unencrypted |
| `crash-demo`, `hang-demo` | Crashes once loaded (`oom`: out of memory), or stops responding |
| `cert-demo` | A certificate that doesn't verify; `expired`, `revoked` or `name` for those errors |
| `error-demo` | Fails to load: `offline`, `refused`, `timeout`, `reset`, `ssl`, `redirects` or `empty`, else not found; with `once` only the first try fails |
| `audio-demo`, `camera-demo` | Plays sound; uses the camera and microphone (`mic`: only the microphone) |
| `popup-demo`, `fullscreen-demo` | Opens a pop-up without a click; goes fullscreen like a video player |
| `unsaved-demo` | Asks before it is closed, reloaded or left |

Previews have no Launch Services and no menu bar: addresses and file paths on the command line stand in for links and files opened from other apps, and `⌘/` opens Keyboard Shortcuts. Prompt answers other than camera, microphone and screen are kept per space and site, as a profile would, so the site menu has something to list.

## Code layout

| Path | Contents |
|---|---|
| `src/main.rs` | Entry point, actions, key bindings, menu bar, window creation |
| `src/app.rs` | Browser behaviour: tabs, spaces, lazy loading, command bar actions, saving, opening links and files from other apps |
| `src/ui/` | Sidebar, page card, command bar, menus, themes, icons, drag and drop, swipe, toasts, downloads, permission cards |
| `src/ui/page_notice.rs` | What the card shows instead of a page: sign-in, Leave site?, crash, unresponsive, certificate and load-error cards; Mute; pop-up blocked |
| `src/ui/shortcuts.rs` | Help > Keyboard Shortcuts, a pop-up panel over the card |
| `src/ui/fullscreen_hint.rs` | "<host> · Press Esc to exit full screen" when a page goes fullscreen |
| `src/ui/elide.rs` | Text cut at its start (hosts, from a label's start, keeping the domain that owns them) or middle (file names, toasts), addresses with their host whole (the link pill), measured when laid out; break points for wrapped addresses |
| `src/input.rs` | The shell's text field (from GPUI's example): undo and redo, password masking, one-line paste |
| `src/model.rs` | Saved state (schema 2): spaces, tabs, sections, themes, sidebar, blocking exceptions, window frame, search engine, recently closed tabs, recent downloads; address parsing; removing deleted spaces' profiles |
| `src/history.rs` | Browsing history per space, in `history.json`, for the command bar and ⌘Y |
| `src/recent.rs` | Recently used tabs per space, for ⌃Tab |
| `src/favicons.rs` | Favicon cache keyed by host |
| `src/blocker.rs` | Ad and tracker blocking with adblock-rust |
| `src/engine/` | Chromium embedding (`mac.rs` and `mac/`), the Linux stand-in (`stub.rs`) and shared events |
| `src/engine/mac/page_requests.rs` | Sign-in, certificate and "Leave site?" callbacks waiting for the shell's answer; unresponsive pages |
| `src/engine/mac/audio.rs` | The script that reports which pages play sound |
| `src/engine/mac/site_data.rs` | Clear Site Data: one origin's cookies and storage in one space |
| `src/native.rs` | AppKit bridge: CEF's message loop inside GPUI's application, window helpers, corner caps, default browser, About panel |
| `vendor/gpui/` | GPUI 0.2.2 with Metal memory, frame-scheduling, native-view input, Dock reopen and Help menu changes; see `MAPBROWSER_PATCH.md` |
| `scripts/bundle.py` | Builds and signs the bundle: Info.plist, document types, helpers, icon |
| `assets/AppIcon.svg` | The app icon's source; `scripts/make_icon.py` renders `assets/AppIcon.icns` |
| `assets/icons/` | Sidebar icons (Lucide, ISC licence) |

## Implementation notes

- **Native views.** Each tab's page is a native view inside a container that GPUI positions over the page card. GPUI cannot draw above those views, so menus and popovers stay inside the sidebar, and the command bar, find bar, link status, toast over the card, Keyboard Shortcuts panel and fullscreen hint are separate child windows. While the card shows a notice (sign-in, crash, error), the engine hides the tab's view (`Engine::set_page_covered`) and GPUI draws the notice on the card.
- **Rounded card.** The page container is clipped to a rectangle; four small corner caps in the window's colour round it. A rounded layer mask over Chromium's layers would cost an offscreen pass on every page frame.
- **First paint.** CEF 152 needs a compositor workaround in this host: after the first real main-frame load, the app changes the native view width by one logical pixel and restores it, once per browser instance, without a reload or ongoing work.
- **Message loop.** Chromium runs its native macOS message loop through the GPUI application; there is no polling timer for browser work.
- **Frame scheduling.** The vendored GPUI runs its display link only while a frame is needed. See `vendor/gpui/MAPBROWSER_PATCH.md` and [energy design](energy.md).
- **Deleting a space.** Each space is a Chromium profile in `<data dir>/profiles/space-<id>`, and Chromium can keep a profile's files open until it quits. Deleting a space therefore only closes its pages and drops it from the session and the history; its directory is removed at the next launch, before Chromium starts (`BrowserState::remove_orphaned_profiles`). That sweep removes only real directories directly in `profiles/` named exactly `space-<id>` with an id no space has; it never follows symlinks and never touches `profiles/default`. It runs only when the session was read from `state.json` itself: after recovering from the backup, or starting from defaults, a space whose data is still wanted may be missing from the session. It also waits while Chromium's `profiles/SingletonLock` exists, since another running instance (or one that crashed) may hold the profiles. Space ids are never reused, so a removed directory can't belong to a later space.
- **Recent tabs.** ⌃Tab walks the active space's five most recently shown tabs (kept in memory, filled up in sidebar order after a launch) and goes to the outlined one when ⌃ is released; ⌃Esc cancels. The page usually has keyboard focus, so on macOS an AppKit flags-changed monitor reports ⌃'s release; GPUI's modifiers event covers the shell and Linux. ⌃Esc isn't reserved, so a page gets it first.
- **Window frame.** The window's frame, display UUID and user-chosen fullscreen are saved with the session (at most every 10 s while it moves, and on quit) and restored at launch, moved and shrunk onto that display. If the display is gone, the window opens centred on the main display.
- **Quitting with downloads.** ⌘Q, Map Browser > Quit Map Browser and the close button go through `request_quit`, which asks with GPUI's prompt (an NSAlert sheet) while downloads run, naming the file when there is one. Quits that reach `-[NSApp terminate:]` directly (the Dock's Quit, logout, AppleScript) go straight to the native quit handler and don't ask. Either way unfinished downloads are cancelled and the session is saved.
- **Launch errors.** If the session can't be read (a `state.json` from a newer schema, an unreadable data directory) or Chromium doesn't start, an NSAlert says why before the app exits, since an app opened from Finder has no visible stderr. Nothing on disk is changed.
- **Damaged sessions and failed saves.** When `state.json` is damaged or missing but a backup or damaged file exists, `BrowserState::recovery` records whether the session came from `state.backup.json` or the defaults and where the damaged file was kept; after launch a one-time sheet says so, with Show in Finder. A background save's result comes back to the main thread (the write stays detached, so a newer save never cancels it). A failure sets `save_failure`: a card in the sidebar, or the toast over the card with the sidebar hidden, says Map Browser can't save your tabs, in plain words, until a save succeeds or Try Again does.
- **Find and same-document navigations.** The engine sends `NewDocument` from CEF's `OnLoadStart`, which CEF calls only for navigations to another document. The find bar closes on that (and on load errors), so fragment and History API changes (`pushState`, `replaceState`) keep it, as in Chrome.
- **Toasts.** `show_toast_kind` takes a kind: Done (✓), Failed (✕, the theme's error red) or Info (a dot, for news such as the zoom level or "Pop-up blocked"); `show_toast` means Done. A toast lasts 1.5 s plus 50 ms per character over 24, 1.5 s more for failures, at most 5 s. `show_toast_about` names a subject (a host or a file name) that alone may be cut, by `Cut::Start` or `Cut::Middle` (`toast_window::Subject`): too long for the sidebar, the words go on one line and the subject on a second; over the card the subject is cut in place. Other long texts lose their middle. Undo toasts ("Closed 24 tabs · ⇧⌘T reopens them") last 4 s and wrap to two lines in the sidebar.
- **Toasts without the sidebar.** With the sidebar hidden, a toast shows in a small non-focusing pop-up window attached to the browser window at the card's bottom left, ignoring the mouse, and closes with the toast. It rises 22 pt while the link status pill shows. With no toast up it shows the save-failure notice, else running downloads ("Downloading <name> — 35%").
- **Moving tabs between spaces.** Spaces are separate profiles, so a moved tab gets a new id and opens again in the target space; a browser kept alive for a download stays with its old id and profile. The tab menu, the command bar and dropping a tab on a space's button all go through `leave_tab`, so a page with unsaved changes asks first.
- **Recently Closed.** Closed tabs are saved in `state.json` (`closed`) with their place in their section (`index`) and a `batch` id shared by tabs closed together (Clear, Close Other Tabs, Close Tabs Below). Each space keeps its last 20 closes (`REOPEN_LIMIT`), a batch counting once, so one space can't push out another's, and stops keeping older closes once it holds 100 closed tabs (`CLOSED_TAB_LIMIT`); the newest close always stays whole. ⇧⌘T takes the newest close, a whole batch at once, and puts each tab back in its section at its old index under a new id. The library lists the 20 newest (`LIBRARY_CLOSED_ROWS`); ⇧⌘T and the command bar reach the rest. Picking one of a batch reopens only that tab. Closed tabs go with their space, and Clear History empties the list.
- **History.** `src/history.rs` keeps each space's visits (address, title, count, last visit) in `<data dir>/history.json`, apart from the session. It holds at most 5000 pages, trimmed in batches of 100, and only http(s) addresses up to 2048 characters; visits older than 90 days are dropped on load. A visit is a `NewDocument` followed by `Address`; same-document changes, reloads of the tab's page, failed loads and a restored tab's first load don't count. Titles arrive later through `Title` and go to the tab's visit. Saves follow the session's rules (10 s after a visit or title, 500 ms after clearing, never postponed, written on a background thread, a sequence number keeping an older snapshot from replacing a newer one) and happen once more on quit. A damaged file starts the history empty. The command bar lists every space's visits together. Nothing logs addresses or titles.
- **Command bar.** One pop-up window with four modes: new tab (⌘T), edit address (⌘L), History (⌘Y) and Settings (⌘,). Its results are a pure function of `CommandContext` (open tabs, recent order, closed tabs, history, search engine), covered by unit tests. When the card changes (⌘S, a resize, a permission card), `sync_command_bar` replaces the window over the new card and passes it the same `TextInput`, so text, selection and undo survive; GPUI can't move windows on X11. Shortcuts that act on the page close the bar first.
- **Search engine.** A session preference (`search_engine` in `state.json`, read leniently: an unknown value means Google). It is mirrored in a process-wide atomic (`model::current_search_engine`) so the page's context menu, built in Chromium callbacks, uses it too.
- **Text fields.** `TextInput` keeps 100 undo states. Typed runs and runs of Backspace coalesce until the caret moves; paste, cut and word deletes are steps of their own. Its own handlers for the global Undo and Redo run before the fallback that sends them to the page, so ⌘Z and Edit > Undo reach a focused shell field first. Password fields show bullets, copy nothing and move by the whole password. Pasted line breaks and tabs become spaces, or nothing when joining a wrapped link.
- **Page notices.** Per tab, in this order: Leave site?, crashed, unresponsive, certificate, sign-in, failed load. Return and Esc answer through the `PageNotice` key context. The tab's next document ends the failure and crash states; closing or unloading the tab answers whatever is still open. Load errors carry Chromium's net error code (CEF's text is "ERR_…" without "net::"); the window-wide error is now only for tabs whose browser couldn't be created.
- **Sign-ins and certificates.** The browser-process switch `disable-chrome-login-prompt` sends HTTP authentication to `GetAuthCredentials`; without it CEF 152 hands it to Chrome's tab-modal login dialog, which has no host beside Alloy views. Sign-in requests arrive on the IO thread; their callbacks, like certificate and beforeunload callbacks, live in `page_requests.rs`, are never answered with the lock held, and are dropped on a new main-frame navigation, on commit, on a crash and on close. Requests for the same host, realm and proxy are answered together. A certificate warning offers Continue for every error Chromium lets you override (CEF doesn't say whether HSTS makes it fatal), never for revoked, invalid, malformed or known-interception certificates; continuing marks the tab Not Secure until it leaves that host. Popups keep Chromium's own interstitial.
- **Crashes and hangs.** `OnRenderProcessTerminated` is reported only for the tab's current browser, not popups. An unresponsive page keeps CEF's callback: Wait calls it, Reload (and ⌘R) terminates the renderer and reloads when the crash arrives.
- **Leaving pages.** Closing, unloading or moving one tab (`leave_tab`) calls `close_browser(0)`. For a page without unload handlers CEF closes it inside that call, so the tab goes at once; otherwise the row stays until `Closed`, and the beforeunload dialog becomes a "Leave site?" card. A second ⌘W forces the close. Bulk actions (Clear, Close Other Tabs, Unload Space, Delete Space) and quitting never ask: browsers no longer current, or closing during shutdown, get "leave" at once, and the quit handler answers any open question first.
- **Background focus.** A focus handler cancels navigation focus for browsers whose view is hidden (background tabs, other spaces, covered pages), so a link opened in the background doesn't take the keyboard from the page being typed in.
- **Sound and capture.** CEF's audio handler would take a page's sound away from the speakers and never says when it goes quiet, so `audio.rs` injects a short script into every http(s) frame at load end. It listens for media element events (play, pause, ended, volume change) and reports through a marked `console.debug` that the display handler swallows; there is no timer. It doesn't see Web Audio or media elements outside the document, and a page's DevTools shows its lines at Verbose level. Camera and microphone use comes from `OnMediaAccessChange`.
- **Popups.** Popup browsers get instance numbers from 1<<40, and their own download and permission handlers; their permission and sign-in requests show on the opener tab. CEF reports a download only while its browser lives, so a popup closed mid-download is hidden and kept until its downloads end, and a popup opened only for a file hides at once.
- **Downloads.** `state.json` keeps every running download plus the 20 newest others (`downloads`). Only http(s) source addresses up to 8 KB are kept, without credentials (`model::download_source`, also applied at load); `data:` and `blob:` downloads keep none and offer no ↻. `Downloads::finish` returns the name and tab before trimming the list, so a download the trim drops still closes the browser kept for it and shows its toast. At launch a running one becomes "Failed — Interrupted"; quitting through the question saves them as Cancelled. Restored rows get ids from 1<<48, away from the engine's. Missing files are checked at launch and when the library opens, never at render. Cancel uses CEF's stored download callback; ↻ calls `StartDownload` on a loaded browser of the download's space, or opens a background tab there that closes itself once the download starts. Failure reasons come from CEF's interrupt reason; a user cancel shows no toast. A finished row opens its file with `NSWorkspace openURL` (the bundle's `LSFileQuarantineEnabled` makes Gatekeeper check downloaded apps); its magnifier reveals it. ⌥⌘L (`ShowDownloads`, History > Show Downloads, the command bar's Show Downloads) opens the library with `show_downloads`, peeking a hidden sidebar and highlighting the first choice. ⌫ in the library (`RemoveFromList` in the `Menu` key context) and Clear Downloads take ended downloads off the list; Clear History does too. Rows of another space start their status with its name.
- **Site menu.** The shield opens `MenuKind::Site`: this session's permission answers (keyed by origin) merged with Chromium's stored content settings for 10 ask-by-default types, read with `CefRequestContext::GetContentSetting`. Reset sets every content setting that backs the permission to default (`reset_settings`, wider than the ten listed: camera pan-tilt-zoom, keyboard and pointer lock, local network, storage access and others); each is the setting Chromium decides that prompt by, since setting a type Chromium doesn't register would crash it. Zoom is read with `GetZoomLevel` when drawn (there is no zoom event). Clear Site Data deletes the origin's host cookies through the space's cookie manager (CEF's deletion by URL matches only host cookies), then visits all cookies and deletes the Domain= cookies sent to the host (`.example.com`), then calls DevTools' `Storage.clearDataForOrigin` on the tab's browser and reloads the page when it answers. Plain HTTP (`engine::is_insecure`: not localhost or loopback; `.local` devices are on the network) shows Not Secure, in words only when the host keeps room for its owning domain beside the chips and the hover buttons (`fit_url_pill`, measured from glyph advances), else as a lock; Copy URL is left out on hover before the host would be cut further, and the zoom chip is the pill's last part, so nothing moves under the pointer on its way to it; on macOS `shown_url` keeps the scheme for such pages, so a switch to https redraws.
- **Menus from the keyboard.** While a menu is open the root's key context is `Browser Menu` and `ui::menu::init` binds the keys; the handlers sit on the root, since `open_menu` focuses it. One highlight (`OpenMenu.highlight`, an entry and a part) follows both the pointer and the keys, so two rows are never lit. `Entry::parts`, `action` and `label` make an entry reachable; `parts` is an exhaustive match, so a new entry kind must be added there. A question's default is Cancel; ⌘⌫ chooses the destructive button. Menus are at most the window's height less 12 pt and scroll, and bottom-bar menus follow the window's bottom edge.
- **Menu bar.** `menus(&MenuBarState)` is rebuilt only when Show/Hide Sidebar, the active tab's section (Pin Tab, Unpin Tab or Remove from Favorites; Close Tab or Unload Tab) or the first nine space names change. A menu named "Help" becomes AppKit's Help menu, with its search field. Keyboard Shortcuts has no key on macOS, because web editors use ⌘/. ⇧⌘] and ⇧⌘[ are GPUI bindings only; with a page focused they rely on the page leaving them.
- **Default browser.** Set as Default Browser checks Launch Services first and shows a toast if Map Browser already is; otherwise it calls `-[NSWorkspace setDefaultApplicationAtURL:toOpenURLsWithScheme:]` for http, which macOS confirms with the user and applies to https too.
- **Opening files.** `scripts/bundle.py` declares document types (Viewer, Alternate rank) for HTML, PDF, SVG, PNG, JPEG, GIF, WebP, AVIF, BMP, plain text and `.webloc`; helpers don't carry them. `app::opened_address` and `opened_file` are the only paths that accept file URLs (Finder, the Dock, `open -a`, sidebar drops); a `.webloc` opens its address, read with NSDictionary. At launch, links and files become tabs before the selected tab loads, so only the last one gets a page. Files refused on the open-URL path are dropped silently; sidebar drops say "Can't open". Links dragged from other apps aren't accepted: GPUI's window registers only for file drags.
- **App identity.** The bundle is "Map Browser.app" and the menus say Map Browser. The executable and helpers keep the name `mapbrowser`, which CEF finds them by, and the helpers keep their bundle identifiers (`dev.mapbrowser.browser.helpergpu` and so on). The Alerts helper's display name is "Map Browser" and it carries the icon, so web notifications show both. To change the icon, edit `assets/AppIcon.svg` and run `python3 scripts/make_icon.py` (needs rsvg-convert or ImageMagick); the committed `AppIcon.icns` means builds need neither.
- **Sidebar peek.** With the sidebar hidden, a 10 pt strip at the window's left edge starts a one-off 150 ms timer on hover, then shows the sidebar without changing the saved hidden state. The page narrows rather than being covered, since GPUI can't draw over it. The peek ends once the pointer is past the sidebar, unless a menu, rename, drag or the command bar needs it. A permission card, naming a space or tab, and the site menu opened from the command bar also show a hidden sidebar while they last. The traffic lights follow whatever is drawn (`sync_traffic_lights`).

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
