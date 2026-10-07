//! Links to other apps. Chromium can't show mailto: or tel: pages: left to
//! it, they would fail with an error page that the tab keeps as its
//! address. Navigations to them are stopped before they start instead (a
//! cancelled load leaves the page as it was): mail, phone, messages and
//! FaceTime links open in their macOS app after a click, and other apps'
//! protocols are dropped without a prompt.

use crate::engine::{Navigation, navigation_for};
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSString, NSURL};

/// Whether a frame's navigation to `url` must not load (OnBeforeBrowse).
pub(super) fn handled_outside(url: &str, user_gesture: bool) -> bool {
    match navigation_for(url) {
        Navigation::Load => false,
        Navigation::OpenInApp => {
            if user_gesture {
                open_in_app(url);
            }
            true
        }
        Navigation::Block => true,
    }
}

/// Opens a mail, phone, messages or FaceTime link in its macOS app; does
/// nothing for other URLs.
pub(super) fn open_in_app(url: &str) {
    if navigation_for(url) == Navigation::OpenInApp
        && let Some(url) = NSURL::URLWithString(&NSString::from_str(url))
    {
        NSWorkspace::sharedWorkspace().openURL(&url);
    }
}
