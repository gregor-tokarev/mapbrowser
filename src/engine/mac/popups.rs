//! Windows pages open for sign-in and payment ("Continue with Google",
//! PayPal, Microsoft): real popup windows whose `window.opener` stays
//! connected, so the provider's page can report back and close itself.
//! With no parent view Alloy puts each in a plain NSWindow of its own,
//! titled, closable and resizable, and makes it key; closing it hands focus
//! back to the browser window. Popups get the opener's profile, so a sign-in
//! lands in the right space. Their handlers keep their titles, addresses and
//! loads away from the opener's tab, and quitting waits for them to close.
//! DevTools windows are tracked the same way.

use super::{
    BrowserLink, TabRequests, browser_view, context_menu::TabMenu, page_keys::PageKeys, request_tab,
};
use crate::engine::{NewWindow, ScreenRect, WindowRequest, new_window_for, popup_rect};
use cef::*;
use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::{NSScreen, NSView, NSWindow};
use objc2_foundation::NSString;
use std::sync::{Arc, atomic::Ordering};

/// Decides `on_before_popup` for the tab `link` stands for, or for one of
/// its popups: true cancels the popup.
#[allow(clippy::too_many_arguments)]
pub(super) fn before_popup(
    link: &BrowserLink,
    opener: Option<&Frame>,
    url: String,
    disposition: WindowOpenDisposition,
    user_gesture: bool,
    features: Option<&PopupFeatures>,
    window_info: Option<&mut WindowInfo>,
    client: Option<&mut Option<Client>>,
) -> bool {
    let request = if disposition == WindowOpenDisposition::NEW_PICTURE_IN_PICTURE {
        // Document picture-in-picture windows are Chrome-style only.
        return true;
    } else if disposition == WindowOpenDisposition::NEW_POPUP {
        WindowRequest::Popup
    } else if disposition == WindowOpenDisposition::NEW_FOREGROUND_TAB {
        WindowRequest::ForegroundTab
    } else if disposition == WindowOpenDisposition::NEW_BACKGROUND_TAB {
        WindowRequest::BackgroundTab
    } else {
        WindowRequest::Window
    };
    match new_window_for(request, &url, user_gesture) {
        NewWindow::Popup => {
            let (Some(window_info), Some(client)) = (window_info, client) else {
                return true;
            };
            if let Some(rect) =
                opener_rect(link).map(|opener| popup_rect(requested(features), opener))
            {
                window_info.bounds = Rect {
                    x: rect.x,
                    y: rect.y,
                    width: rect.width,
                    height: rect.height,
                };
            }
            *client = Some(popup_client(link.clone()));
            false
        }
        NewWindow::Tab { background } => {
            request_tab(link, opener, url, background);
            true
        }
        NewWindow::Blocked => true,
    }
}

/// The position and size `window.open` asked for, if any.
fn requested(features: Option<&PopupFeatures>) -> [Option<i32>; 4] {
    let Some(features) = features else {
        return [None; 4];
    };
    [
        (features.x, features.x_set),
        (features.y, features.y_set),
        (features.width, features.width_set),
        (features.height, features.height_set),
    ]
    .map(|(value, set)| (set != 0).then_some(value))
}

/// The browser window's frame in Chromium's screen coordinates, which
/// measure from the top-left of the main display; Cocoa's measure from its
/// bottom-left.
fn opener_rect(link: &BrowserLink) -> Option<ScreenRect> {
    let mtm = MainThreadMarker::new()?;
    let container = link.keys().container;
    // SAFETY: A non-zero `container` is the engine's web container, alive
    // until `shutdown`; popups are only created before it, on the main thread.
    let window = unsafe { (container as *const NSView).as_ref() }?.window()?;
    let main_display = NSScreen::screens(mtm).firstObject()?.frame();
    let frame = window.frame();
    Some(ScreenRect {
        x: frame.origin.x as i32,
        y: (main_display.size.height - frame.origin.y - frame.size.height) as i32,
        width: frame.size.width as i32,
        height: frame.size.height as i32,
    })
}

wrap_client! {
    struct PopupClient {
        life: LifeSpanHandler,
        keys: KeyboardHandler,
        display: Option<DisplayHandler>,
        requests: Option<RequestHandler>,
        menu: Option<ContextMenuHandler>,
    }
    impl Client {
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life.clone())
        }
        fn keyboard_handler(&self) -> Option<KeyboardHandler> {
            Some(self.keys.clone())
        }
        fn display_handler(&self) -> Option<DisplayHandler> {
            self.display.clone()
        }
        fn request_handler(&self) -> Option<RequestHandler> {
            self.requests.clone()
        }
        fn context_menu_handler(&self) -> Option<ContextMenuHandler> {
            self.menu.clone()
        }
    }
}

/// A popup opened from the tab `link` stands for. Its links open tabs, and
/// its own popups windows, as the tab's do.
fn popup_client(link: BrowserLink) -> Client {
    PopupClient::new(
        PopupLife::new(link.clone()),
        PageKeys::new(link.clone(), true),
        Some(PopupDisplay::new()),
        // Their loads show nowhere: the tab's page state isn't theirs.
        Some(TabRequests::new(link.clone(), Arc::default())),
        Some(TabMenu::new(link, true)),
    )
}

/// A tab's DevTools window, which Chromium titles and runs itself.
pub(super) fn tools_client(link: BrowserLink) -> Client {
    PopupClient::new(
        PopupLife::new(link.clone()),
        PageKeys::new(link, true),
        None,
        None,
        None,
    )
}

wrap_life_span_handler! {
    struct PopupLife { link: BrowserLink }
    impl LifeSpanHandler {
        fn on_after_created(&self, browser: Option<&mut Browser>) {
            let Some(browser) = browser else { return; };
            self.link.live_browsers.fetch_add(1, Ordering::SeqCst);
            let mut pending = self.link.pending();
            pending.popups.insert(browser.identifier(), browser.clone());
            if pending.shutting_down {
                drop(pending);
                if let Some(host) = browser.host() { host.close_browser(1); }
            }
        }
        fn on_before_popup(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, _popup_id: i32, target_url: Option<&CefString>, _target_frame_name: Option<&CefString>, target_disposition: WindowOpenDisposition, user_gesture: i32, popup_features: Option<&PopupFeatures>, window_info: Option<&mut WindowInfo>, client: Option<&mut Option<Client>>, _settings: Option<&mut BrowserSettings>, _extra_info: Option<&mut Option<DictionaryValue>>, _no_javascript_access: Option<&mut i32>) -> i32 {
            let url = target_url.map(ToString::to_string).unwrap_or_default();
            i32::from(before_popup(&self.link, frame.as_deref(), url, target_disposition, user_gesture != 0, popup_features, window_info, client))
        }
        // do_close is left to CEF, which closes the popup's window.
        fn on_before_close(&self, browser: Option<&mut Browser>) {
            self.link.live_browsers.fetch_sub(1, Ordering::SeqCst);
            let ready_to_quit = {
                let mut pending = self.link.pending();
                if let Some(browser) = browser {
                    pending.popups.remove(&browser.identifier());
                }
                pending.shutting_down && pending.all_closed()
            };
            if ready_to_quit { quit_message_loop(); }
        }
    }
}

wrap_display_handler! {
    struct PopupDisplay;
    impl DisplayHandler {
        fn on_title_change(&self, browser: Option<&mut Browser>, title: Option<&CefString>) {
            if let Some(window) = browser.and_then(|browser| popup_window(browser)) {
                window.setTitle(&NSString::from_str(&title.map(ToString::to_string).unwrap_or_default()));
            }
        }
        fn on_address_change(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, url: Option<&CefString>) {
            // The site under the title, where the page can't fake it.
            if frame.is_some_and(|frame| frame.is_main() != 0)
                && let Some(window) = browser.and_then(|browser| popup_window(browser))
            {
                let host = url.and_then(|url| crate::favicons::host_of(&url.to_string()));
                window.setSubtitle(&NSString::from_str(host.as_deref().unwrap_or_default()));
            }
        }
    }
}

fn popup_window(browser: &Browser) -> Option<Retained<NSWindow>> {
    browser_view(browser)?.window()
}
