//! Keys for a focused page. As in Chrome, the page gets browser shortcuts
//! first, all but the reserved ones (`Shortcut::reserved`): the key monitor
//! hands them to the page's view, and `PageKeys` runs the shortcut only if
//! the page leaves the key unhandled (no `preventDefault`).

use super::{BrowserLink, edit_frame};
use crate::engine::{
    EngineEvent, Modifiers, Shortcut, edit_command, shortcut_character, shortcut_for_key,
};
use cef::*;
use objc2::{msg_send, rc::Retained, runtime::Bool};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSWindow};
use objc2_foundation::NSString;

/// kVK_Escape.
pub(super) const KEY_ESCAPE: u16 = 53;

/// A key press, read the way shortcuts are matched.
#[derive(Clone, Copy)]
pub(super) struct KeyPress {
    pub key: u16,
    pub character: Option<char>,
    pub modifiers: Modifiers,
}

impl KeyPress {
    pub fn from_ns_event(event: &NSEvent) -> Self {
        let flags = event.modifierFlags();
        let key = event.keyCode();
        Self {
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
        }
    }

    /// A key down as CEF reports it, read from the NSEvent it carries.
    fn from_cef(event: Option<&KeyEvent>, os_event: *mut u8) -> Option<Self> {
        let kind = event?.type_;
        if kind != KeyEventType::RAWKEYDOWN && kind != KeyEventType::KEYDOWN {
            return None;
        }
        // SAFETY: On macOS CEF passes the NSEvent it is handling, or null,
        // borrowed for the duration of the callback.
        let event = unsafe { os_event.cast::<NSEvent>().as_ref() }?;
        Some(Self::from_ns_event(event))
    }

    pub fn shortcut(self) -> Option<Shortcut> {
        shortcut_for_key(self.key, self.character, self.modifiers)
    }

    fn is_escape(self) -> bool {
        self.key == KEY_ESCAPE && self.modifiers == Modifiers::default()
    }
}

fn text(string: Option<Retained<NSString>>) -> String {
    string.map(|string| string.to_string()).unwrap_or_default()
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
        if self.popup {
            // Popup and DevTools windows have no tabs. No shortcut may reach
            // the browser window behind them through the menu bar.
            if let Some(command) = edit_command(press.character, press.modifiers) {
                return edit_frame(Some(browser.clone()), command);
            }
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
