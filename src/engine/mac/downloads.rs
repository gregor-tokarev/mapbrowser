//! Downloads go straight to ~/Downloads, without a dialog. Without a
//! handler CEF 152 hands them to Chrome's download code, which saves them
//! there too but reports nothing to the app, and holds files it would ask
//! about in a download bubble Alloy browsers don't have.

use super::{BrowserLink, Engine, frame_url};
use crate::engine::{
    CANCELLED_BY_USER, DownloadEnd, EngineEvent, download_failure, download_file_name,
    download_progress_worth_reporting, unique_file_name,
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
}

struct Active {
    id: u64,
    path: PathBuf,
    reported: Option<(Instant, u64)>,
    /// The latest callback, which can still cancel the download.
    callback: Option<DownloadItemCallback>,
    /// Cancelled before Chromium handed over a callback: it cancels with
    /// the first one.
    cancel: bool,
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
                cancel: false,
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
        let received = u64::try_from(item.received_bytes()).unwrap_or(0);
        let end = if item.is_complete() != 0 {
            Some(DownloadEnd::Complete { size: received })
        } else if item.is_canceled() != 0 {
            Some(DownloadEnd::Cancelled)
        } else if item.is_interrupted() != 0 {
            let reason = item.interrupt_reason().get_raw();
            Some(if reason == CANCELLED_BY_USER {
                DownloadEnd::Cancelled
            } else {
                DownloadEnd::Failed {
                    reason: download_failure(reason),
                }
            })
        } else {
            None
        };
        if let Some(end) = end {
            state.active.remove(&key);
            return Some(EngineEvent::DownloadFinished { id, end });
        }
        if let Some(callback) = callback {
            if active.cancel {
                // Cancelling can call back in here: not with the lock held.
                let callback = callback.clone();
                drop(state);
                callback.cancel();
                return None;
            }
            active.callback = Some(callback.clone());
        }
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

    /// Cancels download `id`. Chromium deletes what it saved so far, and
    /// `DownloadFinished` follows.
    fn cancel(&self, id: u64) {
        let callback = {
            let mut state = self.state();
            let Some(active) = state.active.values_mut().find(|active| active.id == id) else {
                return;
            };
            active.cancel = true;
            active.callback.take()
        };
        if let Some(callback) = callback {
            callback.cancel();
        }
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

impl Engine {
    /// Cancels a download in progress (the library's ×).
    pub fn cancel_download(&self, id: u64) {
        self.downloads.cancel(id);
    }

    /// Downloads `url` again through the tab's browser, so with its space's
    /// profile, as a new download. False when the tab has no browser.
    pub fn start_download(&self, tab_id: u64, url: &str) -> bool {
        let Some(host) = self.live_browser(tab_id).and_then(|browser| browser.host()) else {
            return false;
        };
        host.start_download(Some(&CefString::from(url)));
        true
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

/// Where a download comes from, before redirects: signed links that
/// redirect expire, the address that hands them out doesn't.
fn source_url(item: &DownloadItem) -> String {
    let original = CefString::from(&item.original_url()).to_string();
    if original.is_empty() {
        CefString::from(&item.url()).to_string()
    } else {
        original
    }
}

wrap_download_handler! {
    pub(super) struct TabDownloads { link: BrowserLink }
    impl DownloadHandler {
        fn can_download(&self, _browser: Option<&mut Browser>, _url: Option<&CefString>, _request_method: Option<&CefString>) -> i32 {
            1
        }
        fn on_before_download(&self, browser: Option<&mut Browser>, download_item: Option<&mut DownloadItem>, suggested_name: Option<&CefString>, callback: Option<&mut BeforeDownloadCallback>) -> i32 {
            let (Some(item), Some(callback)) = (download_item, callback) else { return 0; };
            let name = download_file_name(&suggested_name.map(ToString::to_string).unwrap_or_default());
            let (id, path) = self.link.downloads.start((self.link.instance, item.id()), &name);
            callback.cont(Some(&path_string(&path)), 0);
            self.link.send(EngineEvent::DownloadStarted {
                id,
                tab_id: self.link.tab_id,
                name: path.file_name().map_or(name, |name| name.to_string_lossy().into_owned()),
                path,
                url: source_url(item),
                blank_tab: shows_no_page(browser.as_deref()),
            });
            1
        }
        fn on_download_updated(&self, _browser: Option<&mut Browser>, download_item: Option<&mut DownloadItem>, callback: Option<&mut DownloadItemCallback>) {
            let Some(item) = download_item else { return; };
            if let Some(event) = self.link.downloads.update((self.link.instance, item.id()), item, callback.as_deref()) {
                self.link.send(event);
            }
        }
    }
}
