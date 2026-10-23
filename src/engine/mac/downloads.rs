//! Downloads go straight to ~/Downloads, without a dialog. Without a
//! handler CEF 152 hands them to Chrome's download code, which saves them
//! there too but reports nothing to the app, and holds files it would ask
//! about in a download bubble Alloy browsers don't have. Popup windows
//! download the same way; one closed while downloading keeps its browser,
//! hidden, until its downloads end, since CEF reports a download only
//! while the browser that started it lives.

use super::{BrowserLink, frame_url, popups::popup_window};
use crate::engine::{
    EngineEvent, download_file_name, download_progress_worth_reporting, unique_file_name,
};
use cef::*;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::Instant,
};

/// Downloads in progress, shared by every browser's handler. Chromium
/// numbers downloads per profile (space), and a download stays with the
/// browser that started it, so the browser instance and Chromium's id
/// identify one.
#[derive(Clone, Default)]
pub(super) struct Downloads(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
    next_id: u64,
    active: HashMap<(u64, u32), Active>,
    /// Popups closed while downloading, by browser instance: they close
    /// once their downloads end.
    closing: HashMap<u64, Browser>,
}

struct Active {
    id: u64,
    path: PathBuf,
    reported: Option<(Instant, u64)>,
    /// The latest callback, which can still cancel the download.
    callback: Option<DownloadItemCallback>,
}

impl Downloads {
    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().expect("download state")
    }

    /// Picks a free name in the Downloads folder: one no file has, and no
    /// download in progress will write (it creates its file later).
    fn start(&self, key: (u64, u32), name: &str) -> (u64, PathBuf) {
        let folder = downloads_folder();
        let mut state = self.state();
        let name = unique_file_name(name, |name| {
            let path = folder.join(name);
            std::fs::symlink_metadata(&path).is_ok()
                || state.active.values().any(|active| active.path == path)
        });
        state.next_id += 1;
        let (id, path) = (state.next_id, folder.join(name));
        state.active.insert(
            key,
            Active {
                id,
                path: path.clone(),
                reported: None,
                callback: None,
            },
        );
        (id, path)
    }

    fn update(
        &self,
        key: (u64, u32),
        item: &DownloadItem,
        callback: Option<&DownloadItemCallback>,
    ) -> Option<EngineEvent> {
        let mut state = self.state();
        let active = state.active.get_mut(&key)?;
        let id = active.id;
        if item.is_complete() != 0 || item.is_canceled() != 0 || item.is_interrupted() != 0 {
            state.active.remove(&key);
            return Some(EngineEvent::DownloadFinished {
                id,
                ok: item.is_complete() != 0,
            });
        }
        if let Some(callback) = callback {
            active.callback = Some(callback.clone());
        }
        let received = u64::try_from(item.received_bytes()).unwrap_or(0);
        let total = u64::try_from(item.total_bytes())
            .ok()
            .filter(|&total| total > 0);
        let now = Instant::now();
        if !download_progress_worth_reporting(active.reported, now, received, total) {
            return None;
        }
        active.reported = Some((now, received));
        Some(EngineEvent::DownloadProgress {
            id,
            received,
            total,
        })
    }

    /// Whether a download of the browser `instance` is in progress.
    pub(super) fn running_for(&self, instance: u64) -> bool {
        self.state()
            .active
            .keys()
            .any(|&(owner, _)| owner == instance)
    }

    /// Closes the popup `browser` (`instance`) once its downloads end.
    pub(super) fn close_when_done(&self, instance: u64, browser: Browser) {
        self.state().closing.insert(instance, browser);
    }

    /// The popup waiting for the downloads of `instance`, now they ended.
    fn done_closing(&self, instance: u64) -> Option<Browser> {
        let mut state = self.state();
        if state.active.keys().any(|&(owner, _)| owner == instance) {
            return None;
        }
        state.closing.remove(&instance)
    }

    /// Cancels every download in progress, for quitting: Chromium would
    /// otherwise interrupt them and leave a partial file under the final
    /// name, where cancelling deletes it. The cancels run as tasks while
    /// quitting waits for the browsers to close.
    pub(super) fn cancel_all(&self) {
        let callbacks: Vec<_> = self
            .state()
            .active
            .values_mut()
            .filter_map(|active| active.callback.take())
            .collect();
        for callback in callbacks {
            callback.cancel();
        }
    }
}

fn downloads_folder() -> PathBuf {
    dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join("Downloads")))
        .unwrap_or_else(std::env::temp_dir)
}

/// A page that never committed shows nothing: its main frame is still the
/// initial empty document.
fn shows_no_page(browser: Option<&Browser>) -> bool {
    let url = frame_url(browser.and_then(|browser| browser.main_frame()).as_ref());
    url.is_empty() || url == "about:blank"
}

fn path_string(path: &Path) -> CefString {
    CefString::from(path.to_string_lossy().as_ref())
}

// `popup`: a popup window's, which leaves its opener tab alone.
wrap_download_handler! {
    pub(super) struct TabDownloads { link: BrowserLink, popup: bool }
    impl DownloadHandler {
        fn can_download(&self, _browser: Option<&mut Browser>, _url: Option<&CefString>, _request_method: Option<&CefString>) -> i32 {
            1
        }
        fn on_before_download(&self, browser: Option<&mut Browser>, download_item: Option<&mut DownloadItem>, suggested_name: Option<&CefString>, callback: Option<&mut BeforeDownloadCallback>) -> i32 {
            let (Some(item), Some(callback)) = (download_item, callback) else { return 0; };
            let name = download_file_name(&suggested_name.map(ToString::to_string).unwrap_or_default());
            let (id, path) = self.link.downloads.start((self.link.instance, item.id()), &name);
            callback.cont(Some(&path_string(&path)), 0);
            let blank = shows_no_page(browser.as_deref());
            self.link.send(EngineEvent::DownloadStarted {
                id,
                tab_id: self.link.tab_id,
                name: path.file_name().map_or(name, |name| name.to_string_lossy().into_owned()),
                path,
                blank_tab: blank && !self.popup,
            });
            // A popup opened only for a file has nothing to show: it goes,
            // and closes once the file is in.
            if blank && self.popup && let Some(browser) = browser {
                if let Some(window) = popup_window(browser) {
                    window.orderOut(None);
                }
                self.link.downloads.close_when_done(self.link.instance, browser.clone());
            }
            1
        }
        fn on_download_updated(&self, _browser: Option<&mut Browser>, download_item: Option<&mut DownloadItem>, callback: Option<&mut DownloadItemCallback>) {
            let Some(item) = download_item else { return; };
            if let Some(event) = self.link.downloads.update((self.link.instance, item.id()), item, callback.as_deref()) {
                self.link.send(event);
            }
            if let Some(popup) = self.link.downloads.done_closing(self.link.instance)
                && let Some(host) = popup.host()
            {
                host.close_browser(1);
            }
        }
    }
}
