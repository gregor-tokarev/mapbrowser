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
wanted. The timer backs off to 8 s while failures repeat and stops when the
link stops, so an idle window has no timers. Becoming visible presents the
current scene once, and a present that found no drawable is retried up to
three times. Effects queued by platform callbacks that run outside
`App::update` (launch, reopen, keyboard layout change) are flushed right away,
because no frame update would apply them.

The pacing rules are in `src/platform/frame_pacing.rs`, which has no
dependencies. Run its tests on any host with
`rustc --edition 2024 --test vendor/gpui/src/platform/frame_pacing.rs -o /tmp/frame_pacing && /tmp/frame_pacing`.
Measure idle and interaction wakeups and keystroke latency on a Mac before
claiming energy savings.
