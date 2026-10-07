//! Web page engine. macOS embeds Chromium; other platforms build a no-op
//! engine so the GPUI shell can be previewed and tested without web views.

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
pub use mac::Engine;

#[cfg(not(target_os = "macos"))]
mod stub;
#[cfg(not(target_os = "macos"))]
pub use stub::Engine;

#[derive(Clone, Debug)]
pub enum Shortcut {
    Address,
    NewTab,
    CloseTab,
    NextTab,
    PreviousSpace,
    NextSpace,
    Reload,
    Back,
    Forward,
}

#[derive(Clone, Debug)]
// Only the Chromium engine reports page events.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub enum EngineEvent {
    Created {
        tab_id: u64,
    },
    Title {
        tab_id: u64,
        title: String,
    },
    Address {
        tab_id: u64,
        url: String,
    },
    Loading {
        tab_id: u64,
        loading: bool,
        can_go_back: bool,
        can_go_forward: bool,
    },
    LoadError {
        tab_id: u64,
        message: String,
    },
    OpenTab {
        opener_tab_id: u64,
        url: String,
    },
    Closed {
        tab_id: u64,
    },
    Shortcut(Shortcut),
}
