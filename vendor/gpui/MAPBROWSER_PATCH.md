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
