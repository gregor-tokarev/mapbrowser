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

/// Browser shortcuts. On macOS the engine's key monitor reports those pressed
/// in the browser window, even while a web page has keyboard focus.
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
    /// A main-frame load failed. `url` is the address that failed: failed
    /// loads never report it as `Address`.
    LoadError {
        tab_id: u64,
        url: String,
        message: String,
    },
    /// A page asked for a new tab: a popup, a ⌘-, ⇧- or middle-click, or the
    /// context menu's "Open Link in New Tab". `url` is a web (or blob:, or
    /// from file: pages, file:) URL as Chromium resolved it: store it as it
    /// is, never as address-bar text. Background tabs must not be selected.
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
    /// A mouse button went down on the page. It takes keyboard focus, and
    /// the shell's popovers close as they would on any press outside them.
    /// Only macOS reports it; elsewhere the shell draws the whole window.
    PagePressed,
    /// The tab's browser is gone; the tab is unloaded. Closing takes a while,
    /// so this can arrive after `create_tab` made a new browser for the tab:
    /// ignore it while `Engine::has_tab` is true.
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
/// macOS virtual key codes (kVK_ANSI_*) of the letters a to z.
#[cfg(any(target_os = "macos", test))]
const LETTER_KEYS: [u16; 26] = [
    0, 11, 8, 2, 14, 3, 5, 4, 34, 38, 40, 37, 46, 45, 31, 35, 12, 15, 1, 17, 32, 9, 13, 7, 16, 6,
];

/// The character a key press means for shortcuts, lowercased, found the way
/// Chromium's KeyboardCodeFromNSEvent does: the typed character (with ⌘ held,
/// layouts such as "Dvorak – QWERTY ⌘" switch to their command keymap), else
/// the character without modifiers, else, for layouts that type no ASCII on
/// that key (Cyrillic, Greek), the letter on that key in the US layout.
#[cfg(any(target_os = "macos", test))]
pub fn shortcut_character(characters: &str, ignoring_modifiers: &str, key: u16) -> Option<char> {
    fn ascii(text: &str) -> Option<char> {
        let mut chars = text.chars();
        let character = chars.next()?;
        (chars.next().is_none() && character.is_ascii_graphic())
            .then(|| character.to_ascii_lowercase())
    }
    ascii(characters)
        .or_else(|| ascii(ignoring_modifiers))
        .or_else(|| {
            let index = LETTER_KEYS.iter().position(|&code| code == key)?;
            Some(char::from(b'a' + u8::try_from(index).ok()?))
        })
}

/// Maps a key press to a browser shortcut. Letter shortcuts follow the
/// character the layout types (`shortcut_character`), as the rest of macOS
/// and the menu bar do, so ⌘Z on AZERTY undoes instead of closing the tab.
/// Tab, the arrows, the brackets and the digits stay on their physical keys,
/// which every layout has. Every shortcut needs ⌘ or ⌃, so typing is never
/// intercepted.
#[cfg(any(target_os = "macos", test))]
pub fn shortcut_for_key(
    key: u16,
    character: Option<char>,
    modifiers: Modifiers,
) -> Option<Shortcut> {
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
    let letter = match (command, control, option, shift, character) {
        (true, false, false, false, Some('t')) => Some(Shortcut::CommandBar),
        (true, false, false, true, Some('t')) => Some(Shortcut::ReopenClosedTab),
        (true, false, false, false, Some('l')) => Some(Shortcut::EditAddress),
        (true, false, false, false, Some('w')) => Some(Shortcut::CloseTab),
        (true, false, false, false, Some('r')) => Some(Shortcut::Reload),
        (true, false, false, false, Some('s')) => Some(Shortcut::ToggleSidebar),
        (true, false, false, false, Some('d')) => Some(Shortcut::TogglePin),
        (true, false, false, true, Some('c')) => Some(Shortcut::CopyUrl),
        (true, false, false, true, Some('k')) => Some(Shortcut::ClearToday),
        _ => None,
    };
    if letter.is_some() {
        return letter;
    }
    Some(match (command, control, option, shift, key) {
        (false, true, false, false, 48) => Shortcut::NextTab,
        (false, true, false, true, 48) => Shortcut::PreviousTab,
        (true, false, true, false, 125) => Shortcut::NextTabInList,
        (true, false, true, false, 126) => Shortcut::PreviousTabInList,
        (true, false, true, false, 123) => Shortcut::PreviousSpace,
        (true, false, true, false, 124) => Shortcut::NextSpace,
        (true, false, false, false, 33) => Shortcut::Back,
        (true, false, false, false, 30) => Shortcut::Forward,
        (true, false, false, false, _) => Shortcut::SelectTab(digit?),
        (false, true, false, false, _) => Shortcut::SelectSpace(digit?),
        _ => return None,
    })
}

/// Standard editing commands for the focused frame of the active page.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditCommand {
    SelectAll,
    Copy,
    Cut,
    Paste,
    Undo,
    Redo,
}

/// ⌘A, ⌘C, ⌘X, ⌘V, ⌘Z and ⇧⌘Z, by `shortcut_character` like the shortcuts.
#[cfg(any(target_os = "macos", test))]
pub fn edit_command(character: Option<char>, modifiers: Modifiers) -> Option<EditCommand> {
    let Modifiers {
        command: true,
        control: false,
        option: false,
        shift,
    } = modifiers
    else {
        return None;
    };
    Some(match (character?, shift) {
        ('a', false) => EditCommand::SelectAll,
        ('c', false) => EditCommand::Copy,
        ('x', false) => EditCommand::Cut,
        ('v', false) => EditCommand::Paste,
        ('z', false) => EditCommand::Undo,
        ('z', true) => EditCommand::Redo,
        _ => return None,
    })
}

/// Whether a page may open `url` in a new tab (a popup, ⌘-click or "Open
/// Link in New Tab"). Web pages and the blob: documents they make open;
/// file: pages only from file: pages, as Chromium allows. Everything else is
/// dropped: data: (Chromium refuses top-level data: navigations from pages,
/// against spoofing; a new tab would load it as if typed), about:,
/// javascript:, and external protocols such as mailto: or tel:, which a tab
/// can't show and which must never be saved as a tab.
#[cfg(any(target_os = "macos", test))]
pub fn opens_in_tab(url: &str, opener_url: &str) -> bool {
    fn scheme(url: &str) -> Option<String> {
        url.split_once(':')
            .map(|(scheme, _)| scheme.trim().to_ascii_lowercase())
    }
    match scheme(url).as_deref() {
        Some("http" | "https" | "blob") => true,
        Some("file") => scheme(opener_url).as_deref() == Some("file"),
        _ => false,
    }
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

    const SHIFT_COMMAND: Modifiers = Modifiers {
        shift: true,
        ..COMMAND
    };
    const OPTION_COMMAND: Modifiers = Modifiers {
        option: true,
        ..COMMAND
    };
    const SHIFT_CONTROL: Modifiers = Modifiers {
        shift: true,
        ..CONTROL
    };

    fn with(modifiers: Modifiers, option: bool, shift: bool) -> Modifiers {
        Modifiers {
            option,
            shift,
            ..modifiers
        }
    }

    /// The US layout: each letter key types its own letter.
    fn us(letter: char) -> (u16, Option<char>) {
        let index = usize::from(letter as u8 - b'a');
        (LETTER_KEYS[index], Some(letter))
    }

    #[test]
    fn maps_every_browser_shortcut() {
        for ((key, character), modifiers, expected) in [
            (us('t'), COMMAND, Shortcut::CommandBar),
            (us('l'), COMMAND, Shortcut::EditAddress),
            (us('w'), COMMAND, Shortcut::CloseTab),
            (us('t'), SHIFT_COMMAND, Shortcut::ReopenClosedTab),
            ((48, None), CONTROL, Shortcut::NextTab),
            ((48, None), SHIFT_CONTROL, Shortcut::PreviousTab),
            ((125, None), OPTION_COMMAND, Shortcut::NextTabInList),
            ((126, None), OPTION_COMMAND, Shortcut::PreviousTabInList),
            ((123, None), OPTION_COMMAND, Shortcut::PreviousSpace),
            ((124, None), OPTION_COMMAND, Shortcut::NextSpace),
            (us('r'), COMMAND, Shortcut::Reload),
            ((33, Some('[')), COMMAND, Shortcut::Back),
            ((30, Some(']')), COMMAND, Shortcut::Forward),
            (us('s'), COMMAND, Shortcut::ToggleSidebar),
            (us('d'), COMMAND, Shortcut::TogglePin),
            (us('c'), SHIFT_COMMAND, Shortcut::CopyUrl),
            (us('k'), SHIFT_COMMAND, Shortcut::ClearToday),
        ] {
            assert_eq!(
                shortcut_for_key(key, character, modifiers),
                Some(expected),
                "{key} {character:?}"
            );
        }
    }

    #[test]
    fn maps_digits_in_number_row_order() {
        for (index, key) in DIGIT_KEYS.into_iter().enumerate() {
            let digit = index as u8 + 1;
            // AZERTY types "&é\"'(§è!ç" on these keys without ⇧.
            for character in [char::from(b'0' + digit), '&'] {
                assert_eq!(
                    shortcut_for_key(key, Some(character), COMMAND),
                    Some(Shortcut::SelectTab(digit))
                );
                assert_eq!(
                    shortcut_for_key(key, Some(character), CONTROL),
                    Some(Shortcut::SelectSpace(digit))
                );
            }
            assert_eq!(shortcut_for_key(key, None, SHIFT_COMMAND), None);
        }
        // kVK_ANSI_0 selects nothing.
        assert_eq!(shortcut_for_key(29, Some('0'), COMMAND), None);
    }

    #[test]
    fn never_intercepts_typing() {
        for key in 0..=127 {
            for character in [None, Some('t'), Some('w'), Some('1')] {
                for (option, shift) in [(false, false), (false, true), (true, false), (true, true)]
                {
                    let modifiers = with(Modifiers::default(), option, shift);
                    assert_eq!(shortcut_for_key(key, character, modifiers), None, "{key}");
                    assert_eq!(edit_command(character, modifiers), None);
                }
            }
        }
    }

    #[test]
    fn leaves_editing_and_unknown_commands_to_the_focused_view() {
        // ⌘A, ⌘Z, ⌘X, ⌘C, ⌘V, ⌘Q, ⌘F, ⌘⇧Z, ⌘⌃F (window fullscreen), and the
        // text field motions ⌃A, ⌃E and ⌃K.
        for (letter, modifiers) in [
            ('a', CONTROL),
            ('e', CONTROL),
            ('k', CONTROL),
            ('a', COMMAND),
            ('z', COMMAND),
            ('x', COMMAND),
            ('c', COMMAND),
            ('v', COMMAND),
            ('q', COMMAND),
            ('f', COMMAND),
            ('z', SHIFT_COMMAND),
            (
                'f',
                Modifiers {
                    control: true,
                    ..COMMAND
                },
            ),
        ] {
            let (key, character) = us(letter);
            assert_eq!(
                shortcut_for_key(key, character, modifiers),
                None,
                "{letter}"
            );
        }
    }

    #[test]
    fn shortcut_characters_follow_the_layout() {
        // US: ⌘T types "t".
        assert_eq!(shortcut_character("t", "t", 17), Some('t'));
        // ⇧ and Caps Lock type capitals.
        assert_eq!(shortcut_character("T", "T", 17), Some('t'));
        // ⌃ types a control character; the key's own character counts.
        assert_eq!(shortcut_character("\u{14}", "t", 17), Some('t'));
        // ⌥ types a symbol, likewise.
        assert_eq!(shortcut_character("†", "t", 17), Some('t'));
        // French AZERTY types "z" on the US W key and "q" on the US A key.
        assert_eq!(shortcut_character("z", "z", 13), Some('z'));
        assert_eq!(shortcut_character("q", "q", 0), Some('q'));
        // Dvorak types "," on the US W key: no letter, so no ⌘W.
        assert_eq!(shortcut_character(",", ",", 13), Some(','));
        // "Dvorak – QWERTY ⌘" switches to QWERTY while ⌘ is held.
        assert_eq!(shortcut_character("w", ",", 13), Some('w'));
        // Cyrillic types no ASCII: the US letter on that key stands in.
        assert_eq!(shortcut_character("ц", "ц", 13), Some('w'));
        assert_eq!(shortcut_character("", "", 13), Some('w'));
        // Non-letter keys in such layouts have no stand-in.
        assert_eq!(shortcut_character("х", "х", 33), None);
    }

    #[test]
    fn letter_shortcuts_follow_the_layout() {
        let press = |characters, key, modifiers| {
            let character = shortcut_character(characters, characters, key);
            (
                shortcut_for_key(key, character, modifiers),
                edit_command(character, modifiers),
            )
        };
        // AZERTY ⌘Z (the US W key) undoes instead of closing the tab, ⌘Q
        // (the US A key) quits instead of selecting all, ⌘A (the US Q key)
        // selects all, and ⌘W (the US Z key) closes the tab.
        assert_eq!(press("z", 13, COMMAND), (None, Some(EditCommand::Undo)));
        assert_eq!(press("q", 0, COMMAND), (None, None));
        assert_eq!(
            press("a", 12, COMMAND),
            (None, Some(EditCommand::SelectAll))
        );
        assert_eq!(press("w", 6, COMMAND), (Some(Shortcut::CloseTab), None));
        // QWERTZ ⌘Z (the US Y key) undoes and ⌘Y does nothing.
        assert_eq!(press("z", 16, COMMAND), (None, Some(EditCommand::Undo)));
        assert_eq!(press("y", 6, COMMAND), (None, None));
        // Dvorak ⌘, (the US W key) does nothing, ⌘Q (the US X key) doesn't
        // cut, and ⌘T (the US K key) opens the command bar.
        assert_eq!(press(",", 13, COMMAND), (None, None));
        assert_eq!(press("q", 7, COMMAND), (None, None));
        assert_eq!(press("'", 12, COMMAND), (None, None));
        assert_eq!(press("t", 40, COMMAND), (Some(Shortcut::CommandBar), None));
        // Cyrillic falls back to the US positions.
        assert_eq!(press("ц", 13, COMMAND), (Some(Shortcut::CloseTab), None));
        assert_eq!(press("я", 6, COMMAND), (None, Some(EditCommand::Undo)));
        assert_eq!(
            press("Я", 6, SHIFT_COMMAND),
            (None, Some(EditCommand::Redo))
        );
    }

    #[test]
    fn edit_commands_need_command_alone_or_with_shift_for_redo() {
        for (letter, expected) in [
            ('a', EditCommand::SelectAll),
            ('c', EditCommand::Copy),
            ('x', EditCommand::Cut),
            ('v', EditCommand::Paste),
            ('z', EditCommand::Undo),
        ] {
            assert_eq!(edit_command(Some(letter), COMMAND), Some(expected));
            assert_eq!(edit_command(Some(letter), OPTION_COMMAND), None);
            assert_eq!(edit_command(Some(letter), CONTROL), None);
        }
        assert_eq!(edit_command(Some('a'), SHIFT_COMMAND), None);
        assert_eq!(edit_command(None, COMMAND), None);
    }

    #[test]
    fn pages_open_tabs_only_for_urls_a_tab_can_show() {
        let page = "https://news.example/story";
        for url in [
            "https://example.com/",
            "HTTP://example.com/",
            "blob:https://news.example/2c4e-7f1a",
        ] {
            assert!(opens_in_tab(url, page), "{url}");
        }
        for url in [
            "mailto:alice@corp.com",
            "tel:+15550100",
            "sms:+15550100",
            "javascript:void(0)",
            "data:text/html,<h1>Sign in</h1>",
            "about:blank",
            "file:///etc/hosts",
            "chrome://settings",
            "facetime:alice@corp.com",
            "",
            "example.com",
        ] {
            assert!(!opens_in_tab(url, page), "{url}");
        }
        // Local pages may open other local files.
        assert!(opens_in_tab("file:///tmp/b.html", "file:///tmp/a.html"));
        assert!(opens_in_tab("https://example.com/", "file:///tmp/a.html"));
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
