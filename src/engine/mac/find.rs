//! Find in page for the shell's find bar.

use super::{BrowserLink, Engine};
use crate::engine::EngineEvent;
use cef::*;

wrap_find_handler! {
    pub(super) struct TabFind { link: BrowserLink }
    impl FindHandler {
        fn on_find_result(&self, _browser: Option<&mut Browser>, _identifier: i32, count: i32, _selection_rect: Option<&Rect>, active_match_ordinal: i32, final_update: i32) {
            self.link.send(EngineEvent::FindResult {
                tab_id: self.link.tab_id,
                count: u32::try_from(count).unwrap_or(0),
                active: u32::try_from(active_match_ordinal).unwrap_or(0),
                final_update: final_update != 0,
            });
        }
    }
}

impl Engine {
    /// Highlights every match of `text` in the tab's page and selects one:
    /// the first for a new search, else (`find_next`) the next one, or the
    /// previous one unless `forward`. Counts arrive as `FindResult`.
    pub fn find(&self, tab_id: u64, text: &str, forward: bool, find_next: bool) {
        if let Some(host) = self.live_browser(tab_id).and_then(|browser| browser.host()) {
            host.find(
                Some(&CefString::from(text)),
                forward.into(),
                0,
                find_next.into(),
            );
        }
    }

    /// Ends a search: the highlights go and, as in Chrome, the selected
    /// match stays selected.
    pub fn stop_finding(&self, tab_id: u64) {
        if let Some(host) = self.live_browser(tab_id).and_then(|browser| browser.host()) {
            host.stop_finding(0);
        }
    }
}
