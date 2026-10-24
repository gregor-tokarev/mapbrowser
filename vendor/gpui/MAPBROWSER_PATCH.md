This is the crates.io GPUI 0.2.2 source, pinned through Cargo's local patch mechanism.

The functional changes are in `src/platform/mac/metal_renderer.rs`. Full-window
path resolve and multisample textures are allocated on the first frame that
contains paths, instead of whenever the window changes size. Resizing invalidates
the attachments. Path rendering still uses the original allocation, shaders, and
four-sample antialiasing. The shell drawable pool uses two buffers instead of
three, and the Metal layer now honors the window's opaque/transparent setting.
Quads, shadows, text, and image shaders are unchanged.

The purpose is to avoid unused path attachments and bound full-window drawable
storage in a browser shell drawn with quads and glyphs. This does not change
Chromium rendering or suspend background pages. Keep this patch separate when
updating GPUI, and remove each change if upstream provides equivalent behavior.

Run `cargo run --example renderer_memory` from the repository root to check the
first path after an empty scene, resizing with paths, and hiding/showing paths.
Unused texture capacity is not equivalent to committed physical memory; measure
the process footprint before claiming savings.

Frame scheduling is demand-driven on macOS. Upstream runs a CVDisplayLink for
every visible window and calls the frame callback on every refresh, even when
nothing changed. It also presents the unchanged scene for one second after any
input, including mouse moves, and each present re-renders the whole scene. Here,
a window asks the platform for a frame whenever it is invalidated, queues a
next-frame callback, or draws a scene it has not presented
(`PlatformWindow::frame_waker`). The display link starts on demand and stops
after eight refreshes without demand. The link's dispatch source is never
suspended, so suspend and resume calls cannot become unbalanced. Other platforms
still request frames continuously.

The post-input presenting is kept only for key presses, clicks, and scrolls in
the active window (one second, all platforms), so ProMotion displays do not lower
their refresh rate between keystrokes. Mouse moves no longer extend it.

A display link that is believed to run but delivers no refreshes would freeze
the window, and upstream only recovered from that by accident, because it
recreated the link on every occlusion change, activation, and `displayLayer:`.
Here the link is replaced when the window becomes visible, when it is bound to
another display than the window's, and when a start fails. While the link
should run, a watchdog timer checks every 250 ms that refreshes arrive and
replaces the link if none did. It also retries a failed start while a frame is
wanted; until then frame requests only record demand, so a link that cannot
start is not rebuilt on every invalidation. The period doubles with each
failure, and after three replacements or retries in a row without a refresh
(about 4 s) the watchdog gives up: it drops the link and stops. Only the window
becoming visible, the displays waking, a display configuration change, the
window moving to another display or becoming key, or a click or key press in
it then starts a new link. The timer also stops when the link stops, so an idle
window has no timers.

Windows count as hidden while the displays sleep (from
`NSWorkspaceScreensDidSleepNotification` to
`NSWorkspaceScreensDidWakeNotification`), as in Chromium and the JDK: a link on
a sleeping display delivers no refreshes, and AppKit can keep reporting the
window as visible, so the watchdog would otherwise keep replacing links that
cannot work. Waking shows the windows again with a new link and one forced
present.

A replaced link is stopped and its dispatch source cancelled, but neither is
released, as upstream: `CVDisplayLinkStop` returns without waiting for the
link's thread, which may still use both (zed#32116). Each replacement therefore
leaks one small CVDisplayLink and one cancelled source; the cases above bound
how often that happens. A failed `DisplayLink::new` leaks nothing: the
CVDisplayLink is set up first, and a source whose link could not take it is
activated, cancelled and released. Becoming visible presents the
current scene once, and a present that found no drawable is retried up to
three times. Effects queued by platform callbacks that run outside
`App::update` (launch, reopen, keyboard layout change) are flushed right away,
because no frame update would apply them.

The pacing rules are in `src/platform/frame_pacing.rs`, which has no
dependencies. Run its tests on any host with
`rustc --edition 2024 --test vendor/gpui/src/platform/frame_pacing.rs -o /tmp/frame_pacing && /tmp/frame_pacing`.
Measure idle and interaction wakeups and keystroke latency on a Mac before
claiming energy savings.

Input fixes for a window that embeds native views (`src/platform/mac/window.rs`,
`src/window.rs`, `src/elements/div.rs`), kept apart from the frame patch:

- Normal windows get a tracking area for mouse moves, entering and exiting
  while key, like pop-up windows already had. AppKit sends mouse moves only to
  the first responder, so while an embedded web view had keyboard focus GPUI
  saw none and every hover state, `on_hover` and tooltip went dead. While
  GPUI's view is first responder AppKit delivers each move twice; the second
  delivery (same timestamp and position) is dropped. Leaving the view sends a
  mouse move to the exit point before the exit event, so hover states and
  tooltips clear.
- `Window::is_window_hovered` (which gates every cursor change) is false on
  macOS while the pointer is over a native subview, so redraws and hit-test
  changes no longer replace the web page's cursor with GPUI's arrow. Outside
  the window it stays true, as upstream: the move to the exit point resets a
  hand or I-beam to the arrow (Chromium sets the arrow on exit for the same
  reason), and GPUI uses no cursor rects that would.
- The 16 ms synthetic drag loop stops when its window state is gone, when the
  window closes, and when no mouse button is pressed. Upstream looped forever
  once the window was gone (a mouse-up landing in another window or a window
  closed mid-drag), waking the main thread about 60 times a second.
- Scroll events clamp the offset before deciding to notify, so a scroll that
  can't move the content (it fits, or is at its end) no longer redraws the
  whole view for every wheel and momentum event.

Check on a Mac: with a web page focused, hovering sidebar rows shows their
close buttons and tooltips appear; the page's cursor (I-beam, hand) is not
reset to an arrow while the shell redraws; flicking the pointer out of the
window from the URL pill (hand) or a rename field (I-beam), or out of the
command bar from a row, leaves the arrow; idle wakeups stay at zero after
pressing a command-bar row, moving slightly and releasing.

The application delegate's callbacks (`src/platform/mac/platform.rs`) return
early instead of asserting once `MacPlatform::run` has returned and cleared the
platform pointer. Map Browser runs Chromium's shutdown after GPUI's loop ends,
and that shutdown can spin the AppKit loop again; a menu validation, Dock click,
opened URL or keyboard-layout change arriving then used to abort the process.

`applicationShouldHandleReopen:hasVisibleWindows:` returns YES. Upstream
declared it without a return value, so AppKit read whatever the register held
and might skip its own reopen work, such as bringing back a minimized window
when the Dock icon is clicked. A menu named "Help" becomes AppKit's Help menu
(`NSApp.helpMenu`), which gets the menu search field, as "Window" already
becomes its Windows menu.
