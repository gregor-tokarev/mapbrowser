//! Keys for a focused page. As in Chrome, the page gets browser shortcuts
//! first, all but the reserved ones (`Shortcut::reserved`), and the editing
//! keys (⌘A, ⌘C, ⌘X, ⌘V, ⌘Z, ⇧⌘Z): the key monitor hands them to the page's
//! view, and `PageKeys` acts only if the page leaves the key unhandled (no
//! `preventDefault`), so web apps with their own undo or clipboard keep them.

use super::{BrowserLink, edit_frame};
use crate::engine::{
    EngineEvent, Modifiers, Shortcut, edit_command, shortcut_character, shortcut_for_key,
};
use cef::*;
use objc2::{msg_send, rc::Retained, runtime::Bool};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType, NSWindow};
use objc2_foundation::NSString;

/// kVK_Escape, kVK_LeftArrow and kVK_RightArrow.
const KEY_ESCAPE: u16 = 53;
const KEY_LEFT: u16 = 123;
const KEY_RIGHT: u16 = 124;

/// A key press, read the way shortcuts are matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct KeyPress {
    pub key: u16,
    pub character: Option<char>,
    pub modifiers: Modifiers,
}

impl KeyPress {
    /// The press a key down stands for; any other event is none.
    ///
    /// AppKit raises NSInternalInconsistencyException when `characters` or
    /// `charactersIgnoringModifiers` is read from an event other than a key
    /// down or up, such as the FlagsChanged every modifier key (⇧, ⌘, ⌥, ⌃,
    /// Caps Lock) sends, and `keyCode` from one that isn't a key event. An
    /// exception unwinding out of a CEF callback aborts the app, so the type
    /// is checked before anything else is read.
    pub fn from_ns_event(event: &NSEvent) -> Option<Self> {
        if event.r#type() != NSEventType::KeyDown {
            return None;
        }
        let flags = event.modifierFlags();
        let key = event.keyCode();
        Some(Self {
            key,
            character: shortcut_character(
                &text(event.characters()),
                &text(event.charactersIgnoringModifiers()),
                key,
            ),
            modifiers: Modifiers {
                command: flags.contains(NSEventModifierFlags::Command),
                control: flags.contains(NSEventModifierFlags::Control),
                option: flags.contains(NSEventModifierFlags::Option),
                shift: flags.contains(NSEventModifierFlags::Shift),
            },
        })
    }

    /// A key down as CEF reports it, read from the NSEvent it carries.
    /// Modifier keys arrive as raw key downs too, carrying a FlagsChanged
    /// event; `from_ns_event` turns those away.
    fn from_cef(event: Option<&KeyEvent>, os_event: *mut u8) -> Option<Self> {
        let kind = event?.type_;
        if kind != KeyEventType::RAWKEYDOWN && kind != KeyEventType::KEYDOWN {
            return None;
        }
        // SAFETY: On macOS CEF passes the NSEvent it is handling, or null,
        // borrowed for the duration of the callback.
        let event = unsafe { os_event.cast::<NSEvent>().as_ref() }?;
        Self::from_ns_event(event)
    }

    pub fn shortcut(self) -> Option<Shortcut> {
        shortcut_for_key(self.key, self.character, self.modifiers)
    }

    pub fn is_escape(self) -> bool {
        self.key == KEY_ESCAPE && self.modifiers == Modifiers::default()
    }

    /// ⌘← and ⌘→ go back and forward, but in text fields they move the
    /// caret to the start or end of the line.
    pub fn moves_caret(self) -> bool {
        matches!(self.key, KEY_LEFT | KEY_RIGHT)
            && matches!(self.shortcut(), Some(Shortcut::Back | Shortcut::Forward))
    }
}

/// Decoded from UTF-16 directly: a dead key or an input method can leave a
/// lone surrogate, for which `-UTF8String` (behind `to_string`) gives NULL.
/// Key events carry a character or two, so reading them one by one is cheap.
fn text(string: Option<Retained<NSString>>) -> String {
    string
        .map(|string| {
            let units = (0..string.length()).map(|index| string.characterAtIndex(index));
            char::decode_utf16(units)
                .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect()
        })
        .unwrap_or_default()
}

/// Hands a ⌘ key to the focused view of `window`, a page's, as AppKit would
/// if GPUI's view didn't take every key equivalent first. Chromium sends it
/// to the page and `PageKeys` gets it back if the page leaves it alone.
pub(super) fn send_to_page(window: &NSWindow, event: &NSEvent) -> bool {
    window.firstResponder().is_some_and(|responder| {
        // SAFETY: performKeyEquivalent: takes an NSEvent and returns BOOL;
        // the responder, a view, is retained for the call.
        let handled: Bool = unsafe { msg_send![&responder, performKeyEquivalent: event] };
        handled.as_bool()
    })
}

wrap_keyboard_handler! {
    pub(super) struct PageKeys { link: BrowserLink, popup: bool }
    impl KeyboardHandler {
        fn on_pre_key_event(&self, _browser: Option<&mut Browser>, event: Option<&KeyEvent>, os_event: *mut u8, is_keyboard_shortcut: Option<&mut i32>) -> i32 {
            // Marked, a shortcut the page leaves alone types no character
            // and comes back in on_key_event.
            if let Some(flag) = is_keyboard_shortcut
                && KeyPress::from_cef(event, os_event).and_then(KeyPress::shortcut).is_some()
            {
                *flag = 1;
            }
            0
        }
        fn on_key_event(&self, browser: Option<&mut Browser>, event: Option<&KeyEvent>, os_event: *mut u8) -> i32 {
            let (Some(browser), Some(press)) = (browser, KeyPress::from_cef(event, os_event)) else {
                return 0;
            };
            i32::from(self.unhandled(browser, press))
        }
    }
}

impl PageKeys {
    /// Runs what a key the page left alone means. False lets CEF offer it
    /// to the menu bar.
    fn unhandled(&self, browser: &Browser, press: KeyPress) -> bool {
        // Editing keys act on the focused frame, as the Edit menu's would.
        // Handled here, CEF doesn't also offer them to the menu bar, whose
        // items would run them a second time.
        if let Some(command) = edit_command(press.character, press.modifiers) {
            return edit_frame(Some(browser.clone()), command);
        }
        if self.popup {
            // Popup and DevTools windows have no tabs. No shortcut may reach
            // the browser window behind them through the menu bar.
            match press.shortcut() {
                Some(Shortcut::CloseTab) => {
                    if let Some(host) = browser.host() {
                        host.close_browser(0);
                    }
                    return true;
                }
                Some(_) => return true,
                None => {}
            }
        } else if let Some(shortcut) = press.shortcut() {
            self.link.send(EngineEvent::Shortcut(shortcut));
            return true;
        }
        // Esc the page has no use for stops a load, as in Chrome.
        if press.is_escape() && browser.is_loading() != 0 {
            browser.stop_load();
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_foundation::NSPoint;

    fn event(
        kind: NSEventType,
        flags: NSEventModifierFlags,
        text: &str,
        key: u16,
    ) -> Retained<NSEvent> {
        let text = NSString::from_str(text);
        NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            kind,
            NSPoint::new(0., 0.),
            flags,
            0.,
            0,
            None,
            &text,
            &text,
            false,
            key,
        )
        .expect("key event")
    }

    #[test]
    fn modifier_keys_are_no_presses() {
        // ⇧ (56), ⌘ (55), ⌥ (58), ⌃ (59) and Caps Lock (57) going down.
        for (key, flags) in [
            (56, NSEventModifierFlags::Shift),
            (55, NSEventModifierFlags::Command),
            (58, NSEventModifierFlags::Option),
            (59, NSEventModifierFlags::Control),
            (57, NSEventModifierFlags::CapsLock),
        ] {
            let modifier = event(NSEventType::FlagsChanged, flags, "", key);
            assert_eq!(KeyPress::from_ns_event(&modifier), None, "{key}");
        }
        let release = event(NSEventType::KeyUp, NSEventModifierFlags::Command, "t", 17);
        assert_eq!(KeyPress::from_ns_event(&release), None);
    }

    #[test]
    fn key_downs_are_read_for_shortcuts() {
        let press = event(NSEventType::KeyDown, NSEventModifierFlags::Command, "t", 17);
        let press = KeyPress::from_ns_event(&press).expect("a press");
        assert_eq!(press.shortcut(), Some(Shortcut::CommandBar));
    }
}
