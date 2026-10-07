//! Web page engine. macOS embeds Chromium; other platforms build an engine
//! that emulates page events so the GPUI shell can be previewed and tested
//! without web views.

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
pub use mac::Engine;

#[cfg(not(target_os = "macos"))]
mod stub;
#[cfg(not(target_os = "macos"))]
pub use stub::Engine;

/// Browser shortcuts. On macOS the engine's key monitor reports them even
/// while a web page has keyboard focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    /// ⌘T: open the command bar to start a new tab.
    CommandBar,
    /// ⌘L: edit the current tab's address in the command bar.
    EditAddress,
    /// ⌘W
    CloseTab,
    /// ⇧⌘T
    ReopenClosedTab,
    /// ⌃Tab
    NextTab,
    /// ⌃⇧Tab
    PreviousTab,
    /// ⌥⌘↓: the next tab in sidebar order.
    NextTabInList,
    /// ⌥⌘↑: the previous tab in sidebar order.
    PreviousTabInList,
    /// ⌘1–⌘9, 1-based.
    SelectTab(u8),
    /// ⌃1–⌃9, 1-based.
    SelectSpace(u8),
    /// ⌥⌘←
    PreviousSpace,
    /// ⌥⌘→
    NextSpace,
    /// ⌘R
    Reload,
    /// ⌘[
    Back,
    /// ⌘]
    Forward,
    /// ⌘S
    ToggleSidebar,
    /// ⌘D
    TogglePin,
    /// ⇧⌘C
    CopyUrl,
    /// ⇧⌘K
    ClearToday,
}

#[derive(Clone, Debug)]
// Only the Chromium engine reports every event, and a shell may leave some
// payloads unread (previews show no favicons).
#[allow(dead_code)]
pub enum EngineEvent {
    /// A browser exists for the tab; call `Engine::register_pending`.
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
    /// Load progress in 0.0..=1.0, coalesced to steps of at least 0.1 plus
    /// the final 1.0.
    Progress {
        tab_id: u64,
        progress: f64,
    },
    /// The page's favicon as PNG bytes, rendered for 2x displays.
    Favicon {
        tab_id: u64,
        png: Vec<u8>,
    },
    LoadError {
        tab_id: u64,
        message: String,
    },
    /// A page asked for a new tab: a popup, ⌘-click, or the context menu's
    /// "Open Link in New Tab". Background tabs must not be selected.
    OpenTab {
        opener_tab_id: u64,
        url: String,
        background: bool,
    },
    /// A page entered or left HTML fullscreen (video players, slides). The
    /// shell owns the transition: toggle window fullscreen, hide the sidebar
    /// and card insets, and call `Engine::set_fullscreen_layout`.
    Fullscreen {
        tab_id: u64,
        fullscreen: bool,
    },
    /// Text to put on the clipboard ("Copy Link Address").
    CopyText(String),
    /// The tab's browser is gone; the tab is unloaded.
    Closed {
        tab_id: u64,
    },
    Shortcut(Shortcut),
}

/// Modifier keys held with a key press.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub command: bool,
    pub control: bool,
    pub option: bool,
    pub shift: bool,
}

/// macOS virtual key codes (kVK_ANSI_*) of the digit keys 1 to 9.
#[cfg(any(target_os = "macos", test))]
const DIGIT_KEYS: [u16; 9] = [18, 19, 20, 21, 23, 22, 26, 28, 25];

/// Maps a macOS virtual key code to a browser shortcut. Key codes name
/// physical keys in the US layout, so bindings stay on the same keys in every
/// layout. Every shortcut needs ⌘ or ⌃, so typing is never intercepted.
#[cfg(any(target_os = "macos", test))]
pub fn shortcut_for_key(key: u16, modifiers: Modifiers) -> Option<Shortcut> {
    let Modifiers {
        command,
        control,
        option,
        shift,
    } = modifiers;
    let digit = DIGIT_KEYS
        .iter()
        .position(|&code| code == key)
        .and_then(|index| u8::try_from(index + 1).ok());
    Some(match (command, control, option, shift, key) {
        (true, false, false, false, 17) => Shortcut::CommandBar,
        (true, false, false, true, 17) => Shortcut::ReopenClosedTab,
        (true, false, false, false, 37) => Shortcut::EditAddress,
        (true, false, false, false, 13) => Shortcut::CloseTab,
        (false, true, false, false, 48) => Shortcut::NextTab,
        (false, true, false, true, 48) => Shortcut::PreviousTab,
        (true, false, true, false, 125) => Shortcut::NextTabInList,
        (true, false, true, false, 126) => Shortcut::PreviousTabInList,
        (true, false, true, false, 123) => Shortcut::PreviousSpace,
        (true, false, true, false, 124) => Shortcut::NextSpace,
        (true, false, false, false, 15) => Shortcut::Reload,
        (true, false, false, false, 33) => Shortcut::Back,
        (true, false, false, false, 30) => Shortcut::Forward,
        (true, false, false, false, 1) => Shortcut::ToggleSidebar,
        (true, false, false, false, 2) => Shortcut::TogglePin,
        (true, false, false, true, 8) => Shortcut::CopyUrl,
        (true, false, false, true, 40) => Shortcut::ClearToday,
        (true, false, false, false, _) => Shortcut::SelectTab(digit?),
        (false, true, false, false, _) => Shortcut::SelectSpace(digit?),
        _ => return None,
    })
}

/// Progress callbacks arrive many times per load. Report only steps of at
/// least 0.1 and the moment a load completes, so the shell redraws rarely.
#[cfg(any(target_os = "macos", test))]
pub fn progress_worth_reporting(last: Option<f64>, progress: f64) -> bool {
    last.is_none_or(|last| (progress >= 1.0 && last < 1.0) || (progress - last).abs() >= 0.1)
}

/// Adds features to a Chromium `--enable-features`/`--disable-features`
/// value. Chromium keeps only the last value of a repeated switch, so new
/// entries must be merged into the existing list. Entries may carry a field
/// trial (`Name<Trial`) or parameters (`Name:key/value`); a feature already
/// listed in any form is left as it is.
#[cfg(any(target_os = "macos", test))]
pub fn merge_features(existing: &str, add: &[&str]) -> String {
    fn name(entry: &str) -> &str {
        entry.split(['<', ':']).next().unwrap_or(entry)
    }
    let mut features: Vec<&str> = existing
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect();
    for feature in add {
        if !features.iter().any(|entry| name(entry) == name(feature)) {
            features.push(feature);
        }
    }
    features.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMAND: Modifiers = Modifiers {
        command: true,
        control: false,
        option: false,
        shift: false,
    };
    const CONTROL: Modifiers = Modifiers {
        command: false,
        control: true,
        option: false,
        shift: false,
    };

    fn with(modifiers: Modifiers, option: bool, shift: bool) -> Modifiers {
        Modifiers {
            option,
            shift,
            ..modifiers
        }
    }

    #[test]
    fn maps_every_browser_shortcut() {
        for (key, modifiers, expected) in [
            (17, COMMAND, Shortcut::CommandBar),
            (37, COMMAND, Shortcut::EditAddress),
            (13, COMMAND, Shortcut::CloseTab),
            (17, with(COMMAND, false, true), Shortcut::ReopenClosedTab),
            (48, CONTROL, Shortcut::NextTab),
            (48, with(CONTROL, false, true), Shortcut::PreviousTab),
            (125, with(COMMAND, true, false), Shortcut::NextTabInList),
            (126, with(COMMAND, true, false), Shortcut::PreviousTabInList),
            (123, with(COMMAND, true, false), Shortcut::PreviousSpace),
            (124, with(COMMAND, true, false), Shortcut::NextSpace),
            (15, COMMAND, Shortcut::Reload),
            (33, COMMAND, Shortcut::Back),
            (30, COMMAND, Shortcut::Forward),
            (1, COMMAND, Shortcut::ToggleSidebar),
            (2, COMMAND, Shortcut::TogglePin),
            (8, with(COMMAND, false, true), Shortcut::CopyUrl),
            (40, with(COMMAND, false, true), Shortcut::ClearToday),
        ] {
            assert_eq!(shortcut_for_key(key, modifiers), Some(expected), "{key}");
        }
    }

    #[test]
    fn maps_digits_in_number_row_order() {
        for (index, key) in DIGIT_KEYS.into_iter().enumerate() {
            let digit = index as u8 + 1;
            assert_eq!(
                shortcut_for_key(key, COMMAND),
                Some(Shortcut::SelectTab(digit))
            );
            assert_eq!(
                shortcut_for_key(key, CONTROL),
                Some(Shortcut::SelectSpace(digit))
            );
            assert_eq!(shortcut_for_key(key, with(COMMAND, false, true)), None);
        }
        // kVK_ANSI_0 selects nothing.
        assert_eq!(shortcut_for_key(29, COMMAND), None);
    }

    #[test]
    fn never_intercepts_typing() {
        for key in 0..=127 {
            for (option, shift) in [(false, false), (false, true), (true, false), (true, true)] {
                let modifiers = with(Modifiers::default(), option, shift);
                assert_eq!(shortcut_for_key(key, modifiers), None, "{key}");
            }
        }
    }

    #[test]
    fn leaves_editing_and_unknown_commands_to_the_focused_view() {
        // ⌘A, ⌘Z, ⌘X, ⌘C, ⌘V, ⌘Q, ⌘F, ⌘⇧Z, ⌘⌃F (window fullscreen), and the
        // text field motions ⌃A, ⌃E and ⌃K.
        for (key, modifiers) in [
            (0, CONTROL),
            (14, CONTROL),
            (40, CONTROL),
            (0, COMMAND),
            (6, COMMAND),
            (7, COMMAND),
            (8, COMMAND),
            (9, COMMAND),
            (12, COMMAND),
            (3, COMMAND),
            (6, with(COMMAND, false, true)),
            (
                3,
                Modifiers {
                    control: true,
                    ..COMMAND
                },
            ),
        ] {
            assert_eq!(shortcut_for_key(key, modifiers), None, "{key}");
        }
    }

    #[test]
    fn coalesces_progress() {
        assert!(progress_worth_reporting(None, 0.1));
        assert!(!progress_worth_reporting(Some(0.1), 0.15));
        assert!(progress_worth_reporting(Some(0.1), 0.25));
        assert!(progress_worth_reporting(Some(0.95), 1.0));
        assert!(!progress_worth_reporting(Some(1.0), 1.0));
        // A new load starts over.
        assert!(progress_worth_reporting(Some(1.0), 0.1));
    }

    #[test]
    fn merges_features_without_dropping_existing_ones() {
        let cef = "GlicActorUi,AutofillActorMode,LensOverlay,KillOnInvalidNavigationHeaders";
        assert_eq!(
            merge_features(cef, &["MediaRouter", "LensOverlay"]),
            format!("{cef},MediaRouter")
        );
        assert_eq!(merge_features("", &["A", "B", "A"]), "A,B");
        assert_eq!(merge_features(" A , ,B<Trial", &["B", "C"]), "A,B<Trial,C");
        assert_eq!(merge_features("A:x/1", &["A"]), "A:x/1");
    }
}
