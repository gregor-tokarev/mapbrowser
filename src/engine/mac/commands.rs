//! Menu commands for a tab's page: zoom, printing and developer tools.

use super::{BrowserLink, Engine, popups};
use crate::engine::{ZoomStep, zoom_percent};
use cef::*;

impl Engine {
    /// Zooms the tab's page a step and returns its zoom in percent. As in
    /// Chrome, zoom is kept per site, in each space's profile.
    pub fn zoom(&self, tab_id: u64, step: ZoomStep) -> Option<u32> {
        let host = self.live_browser(tab_id)?.host()?;
        host.zoom(match step {
            ZoomStep::In => ZoomCommand::IN,
            ZoomStep::Out => ZoomCommand::OUT,
            ZoomStep::Reset => ZoomCommand::RESET,
        });
        Some(zoom_percent(host.zoom_level()))
    }

    /// Opens the system print panel for the page, as File > Print… or a
    /// page's `window.print()` does.
    pub fn print(&self, tab_id: u64) {
        if let Some(host) = self.live_browser(tab_id).and_then(|browser| browser.host()) {
            host.print();
        }
    }

    /// Opens the page's developer tools in a window of their own, or closes
    /// them. Their own client keeps that window from standing in for the
    /// tab's browser, and quitting closes it with the popups.
    pub fn toggle_dev_tools(&self, tab_id: u64) {
        let Some(host) = self.live_browser(tab_id).and_then(|browser| browser.host()) else {
            return;
        };
        if host.has_dev_tools() != 0 {
            host.close_dev_tools();
            return;
        }
        let mut client = popups::tools_client(BrowserLink {
            tab_id,
            instance: 0,
            events: self.events.clone(),
            pending: self.pending.clone(),
            keys: self.keys.clone(),
            live_browsers: self.live_browsers.clone(),
            downloads: self.downloads.clone(),
            permissions: self.permissions.clone(),
        });
        host.show_dev_tools(
            Some(&WindowInfo::default()),
            Some(&mut client),
            Some(&BrowserSettings::default()),
            None,
        );
    }
}
