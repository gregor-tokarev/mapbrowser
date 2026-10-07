//! Engine for platforms without the Chromium embedding (Linux previews and
//! tests). It shows no web pages, but every tab gets an in-memory history and
//! reports the events a quick page load would, so the shell's flows (titles,
//! progress, navigation buttons, closing) behave as they do on macOS.
//!
//! Addresses containing "download-demo" download a file instead (one with
//! "fail" fails halfway), and pages whose address contains
//! "permission-demo" ask to use the camera and microphone ("location" or
//! "notifications" in the address ask for those). On pages whose address
//! contains "status-demo" the pointer rests on a long link.

use super::{EngineEvent, Permission, PermissionDecision, ZoomStep, step_zoom};
use crate::blocker::ContentBlocker;
use anyhow::Result;
use async_channel::{Receiver, Sender};
use gpui::http_client::Url;
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    ffi::c_void,
    path::Path,
    time::Duration,
};

/// A demo download takes this many steps of `DOWNLOAD_STEP`.
const DOWNLOAD_STEPS: u64 = 20;
const DOWNLOAD_STEP: Duration = Duration::from_millis(600);
const DOWNLOAD_SIZE: u64 = 48_000_000;

/// Methods mirror the macOS engine; `allow(dead_code)` marks those this
/// platform's shell never calls.
pub struct Engine {
    events: Sender<EngineEvent>,
    pages: RefCell<HashMap<u64, History>>,
    next_id: Cell<u64>,
    /// Unanswered permission requests and their tabs.
    permission_requests: RefCell<HashMap<u64, u64>>,
}

struct History {
    entries: Vec<String>,
    index: usize,
    /// Page zoom in percent.
    zoom: u32,
    /// The selected find match, 1-based; 0 without one.
    find_active: u32,
}

impl History {
    fn can_go_back(&self) -> bool {
        self.index > 0
    }

    fn can_go_forward(&self) -> bool {
        self.index + 1 < self.entries.len()
    }
}

/// Stand-in page title, so previews keep readable titles: a search's query,
/// else the last segment of the path ("/wiki/Arc_(web_browser)" is "Arc (web
/// browser)"), else the host without "www.", else the URL itself.
fn page_title(url: &str) -> String {
    let Ok(parsed) = Url::parse(url) else {
        return url.to_owned();
    };
    let query = parsed
        .query_pairs()
        .find_map(|(name, value)| (name == "q").then(|| value.trim().to_owned()))
        .filter(|query| !query.is_empty());
    let from_path = parsed
        .path_segments()
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
        .map(|segment| {
            let segment = percent_decode(segment);
            let segment = [".html", ".htm", ".php"]
                .iter()
                .find_map(|extension| segment.strip_suffix(extension))
                .unwrap_or(&segment);
            let words = segment.replace(['_', '-', '+'], " ");
            let words = words.trim();
            let mut chars = words.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect::<String>())
                .unwrap_or_default()
        })
        .filter(|title| !title.is_empty());
    query
        .or(from_path)
        .or_else(|| {
            parsed
                .host_str()
                .map(|host| host.trim_start_matches("www.").to_owned())
        })
        .unwrap_or_else(|| url.to_owned())
}

fn is_download(url: &str) -> bool {
    url.contains("download-demo")
}

/// Decodes %XX escapes; invalid UTF-8 becomes U+FFFD.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escaped = (bytes[index] == b'%')
            .then(|| text.get(index + 1..index + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match escaped {
            Some(byte) => {
                decoded.push(byte);
                index += 3;
            }
            None => {
                decoded.push(bytes[index]);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

impl Engine {
    pub fn initialize(_data_dir: &Path) -> Result<(Self, Receiver<EngineEvent>)> {
        let (events, receiver) = async_channel::unbounded();
        let engine = Self {
            events,
            pages: RefCell::default(),
            next_id: Cell::new(0),
            permission_requests: RefCell::default(),
        };
        Ok((engine, receiver))
    }

    #[allow(dead_code)]
    pub fn attach(&mut self, _parent: *mut c_void) -> Result<()> {
        Ok(())
    }

    pub fn create_tab(&mut self, tab_id: u64, _workspace_id: u64, url: &str) -> Result<()> {
        if self.has_tab(tab_id) {
            self.navigate(tab_id, url);
            return Ok(());
        }
        let download = is_download(url);
        self.pages.borrow_mut().insert(
            tab_id,
            History {
                entries: if download {
                    Vec::new()
                } else {
                    vec![url.to_owned()]
                },
                index: 0,
                zoom: 100,
                find_active: 0,
            },
        );
        self.send(EngineEvent::Created { tab_id });
        if download {
            self.download(tab_id, url, true);
        } else {
            self.load(tab_id);
        }
        Ok(())
    }

    pub fn set_blocker(&mut self, _blocker: ContentBlocker) {}
    pub fn set_blocking_exceptions(&self, _hosts: HashSet<String>) {}
    pub fn forget_space(&mut self, _space_id: u64) {}
    pub fn register_pending(&mut self) {}
    pub fn activate_tab(&mut self, _tab_id: u64) {}
    pub fn set_bounds(&mut self, _x: f64, _y: f64, _width: f64, _height: f64) {}
    pub fn set_transition(&mut self, _from: u64, _to: u64, _progress: f32, _direction: i32) {}
    pub fn focus_tab(&self, _tab_id: u64) {}
    pub fn focus_shell(&self) {}
    pub fn undo_in_page(&self) {}
    pub fn redo_in_page(&self) {}

    pub fn navigate(&self, tab_id: u64, url: &str) {
        {
            let mut pages = self.pages.borrow_mut();
            let Some(page) = pages.get_mut(&tab_id) else {
                return;
            };
            if is_download(url) {
                let blank = page.entries.is_empty();
                drop(pages);
                return self.download(tab_id, url, blank);
            }
            page.entries.truncate(page.index + 1);
            page.entries.push(url.to_owned());
            page.index = page.entries.len() - 1;
        }
        self.load(tab_id);
    }

    pub fn back(&self, tab_id: u64) {
        self.go(tab_id, -1);
    }

    pub fn forward(&self, tab_id: u64) {
        self.go(tab_id, 1);
    }

    pub fn reload(&self, tab_id: u64) {
        self.load(tab_id);
    }

    /// Loads finish at once here, so there is nothing to stop.
    pub fn stop(&self, _tab_id: u64) {}

    /// The tab's zoom after `step`, in percent, in Chrome's steps.
    pub fn zoom(&self, tab_id: u64, step: ZoomStep) -> Option<u32> {
        let mut pages = self.pages.borrow_mut();
        let page = pages.get_mut(&tab_id)?;
        page.zoom = step_zoom(page.zoom, step);
        Some(page.zoom)
    }

    pub fn toggle_dev_tools(&self, _tab_id: u64) {}

    /// Previews have no print panel.
    pub fn print(&self, _tab_id: u64) {}

    /// Previews have no page text: matches are counted in the page's
    /// stand-in title and address, so the find bar shows real numbers.
    pub fn find(&self, tab_id: u64, text: &str, forward: bool, find_next: bool) {
        let (count, active) = {
            let mut pages = self.pages.borrow_mut();
            let Some(page) = pages.get_mut(&tab_id) else {
                return;
            };
            // A tab that only downloaded a file has no page.
            let Some(url) = page.entries.get(page.index).cloned() else {
                return;
            };
            let text = text.to_lowercase();
            let count = if text.is_empty() {
                0
            } else {
                let page_text = format!("{} {url}", page_title(&url)).to_lowercase();
                page_text.matches(text.as_str()).count() as u32
            };
            let current = page.find_active.clamp(1, count.max(1));
            page.find_active = match count {
                0 => 0,
                _ if !find_next => 1,
                _ if forward => current % count + 1,
                _ => (current + count - 2) % count + 1,
            };
            (count, page.find_active)
        };
        self.send(EngineEvent::FindResult {
            tab_id,
            count,
            active,
            final_update: true,
        });
    }

    pub fn stop_finding(&self, tab_id: u64) {
        if let Some(page) = self.pages.borrow_mut().get_mut(&tab_id) {
            page.find_active = 0;
        }
    }

    /// Whether the tab has a page. As on macOS, a `Closed` for a tab that has
    /// one again (closed and created in one go) is stale.
    pub fn has_tab(&self, tab_id: u64) -> bool {
        self.pages.borrow().contains_key(&tab_id)
    }

    pub fn forget_closed(&mut self, _tab_id: u64) {}

    /// Reports `Closed` at once; macOS reports it once Chromium is done, and
    /// not at all if the tab was created again by then.
    pub fn close_tab(&mut self, tab_id: u64) {
        if self.pages.borrow_mut().remove(&tab_id).is_some() {
            self.dismiss_requests(tab_id);
            self.send(EngineEvent::Closed { tab_id });
        }
    }

    pub fn respond_permission(&self, id: u64, _decision: PermissionDecision) {
        self.permission_requests.borrow_mut().remove(&id);
    }

    pub fn shutdown(&mut self) -> Result<()> {
        self.pages.borrow_mut().clear();
        Ok(())
    }

    fn go(&self, tab_id: u64, step: isize) {
        {
            let mut pages = self.pages.borrow_mut();
            let Some(page) = pages.get_mut(&tab_id) else {
                return;
            };
            let Some(index) = page
                .index
                .checked_add_signed(step)
                .filter(|&index| index < page.entries.len())
            else {
                return;
            };
            page.index = index;
        }
        self.load(tab_id);
    }

    /// Reports one complete, instant load of the tab's current entry.
    fn load(&self, tab_id: u64) {
        let (url, can_go_back, can_go_forward) = {
            let pages = self.pages.borrow();
            let Some((page, url)) = pages
                .get(&tab_id)
                .and_then(|page| Some((page, page.entries.get(page.index)?)))
            else {
                return;
            };
            (url.clone(), page.can_go_back(), page.can_go_forward())
        };
        self.dismiss_requests(tab_id);
        let loading = |loading| EngineEvent::Loading {
            tab_id,
            loading,
            can_go_back,
            can_go_forward,
        };
        let title = page_title(&url);
        let asks = url.contains("permission-demo").then(|| url.clone());
        let hovered = url.contains("status-demo").then(|| {
            format!("{url}/2026/10/the-quick-brown-fox-jumps-over-the-lazy-dog?utm_source=newsletter&utm_medium=email")
        });
        self.send(loading(true));
        self.send(EngineEvent::NewDocument { tab_id });
        self.send(EngineEvent::Address { tab_id, url });
        self.send(EngineEvent::Title { tab_id, title });
        for progress in [0.4, 1.0] {
            self.send(EngineEvent::Progress { tab_id, progress });
        }
        self.send(loading(false));
        if let Some(text) = hovered {
            self.send(EngineEvent::Status { tab_id, text });
        }
        if let Some(url) = asks {
            self.request_permissions(tab_id, &url);
        }
    }

    fn next_id(&self) -> u64 {
        self.next_id.set(self.next_id.get() + 1);
        self.next_id.get()
    }

    /// Downloads the demo file from `url` (named after its last path
    /// segment) in steps on another thread, as Chromium reports progress.
    fn download(&self, tab_id: u64, url: &str, blank_tab: bool) {
        let id = self.next_id();
        let name = Url::parse(url)
            .ok()
            .and_then(|url| Some(percent_decode(url.path_segments()?.next_back()?)))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "download-demo".to_owned());
        let path = dirs::download_dir().unwrap_or_default().join(&name);
        self.send(EngineEvent::DownloadStarted {
            id,
            tab_id,
            name,
            path,
            blank_tab,
        });
        let (events, fails) = (self.events.clone(), url.contains("fail"));
        std::thread::spawn(move || {
            for step in 1..=DOWNLOAD_STEPS {
                std::thread::sleep(DOWNLOAD_STEP);
                let event = if fails && step > DOWNLOAD_STEPS / 2 {
                    EngineEvent::DownloadFinished { id, ok: false }
                } else {
                    EngineEvent::DownloadProgress {
                        id,
                        received: DOWNLOAD_SIZE * step / DOWNLOAD_STEPS,
                        total: Some(DOWNLOAD_SIZE),
                    }
                };
                let finished = matches!(event, EngineEvent::DownloadFinished { .. });
                if events.try_send(event).is_err() || finished {
                    return;
                }
            }
            let _ = events.try_send(EngineEvent::DownloadFinished { id, ok: true });
        });
    }

    fn request_permissions(&self, tab_id: u64, url: &str) {
        let permissions = if url.contains("location") {
            vec![Permission::Location]
        } else if url.contains("notifications") {
            vec![Permission::Notifications]
        } else {
            vec![Permission::Camera, Permission::Microphone]
        };
        let id = self.next_id();
        self.permission_requests.borrow_mut().insert(id, tab_id);
        self.send(EngineEvent::PermissionRequest {
            id,
            tab_id,
            origin: Url::parse(url)
                .map(|url| format!("{}/", url.origin().ascii_serialization()))
                .unwrap_or_default(),
            permissions,
        });
    }

    /// The tab's page is gone, and with it its requests.
    fn dismiss_requests(&self, tab_id: u64) {
        let mut ids: Vec<u64> = self
            .permission_requests
            .borrow()
            .iter()
            .filter(|&(_, &tab)| tab == tab_id)
            .map(|(&id, _)| id)
            .collect();
        ids.sort();
        for id in ids {
            self.permission_requests.borrow_mut().remove(&id);
            self.send(EngineEvent::PermissionDismissed { id });
        }
    }

    fn send(&self, event: EngineEvent) {
        // Unbounded, so this never blocks; it fails only once the shell has
        // dropped the receiver while quitting.
        let _ = self.events.try_send(event);
    }
}

/// Window and card chrome. Linux previews draw these parts in GPUI.
impl Engine {
    pub fn set_corner_caps(&mut self, _radius: f64, _colors: [u32; 4]) {}
    pub fn set_fullscreen_layout(&mut self, _fullscreen: bool) {}
    pub fn exit_fullscreen(&self, _tab_id: u64) {}
    pub fn set_traffic_lights_hidden(&self, _hidden: bool) {}
    /// The command bar is a child window only on macOS.
    #[allow(dead_code)]
    pub fn attach_child_window(&self, _child_view: *mut c_void) {}
    #[allow(dead_code)]
    pub fn detach_child_window(&self, _child_view: *mut c_void) {}
    #[allow(dead_code)]
    pub fn place_child_window(&self, _child_view: *mut c_void, _x: f64, _y: f64) {}
    #[allow(dead_code)]
    pub fn show_overlay_window(&self, _child_view: *mut c_void, _shown: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(events: &Receiver<EngineEvent>) -> Vec<EngineEvent> {
        std::iter::from_fn(|| events.try_recv().ok()).collect()
    }

    fn nav_state(events: &[EngineEvent]) -> Option<(bool, bool)> {
        events.iter().rev().find_map(|event| match event {
            EngineEvent::Loading {
                loading: false,
                can_go_back,
                can_go_forward,
                ..
            } => Some((*can_go_back, *can_go_forward)),
            _ => None,
        })
    }

    fn address(events: &[EngineEvent]) -> Option<&str> {
        events.iter().find_map(|event| match event {
            EngineEvent::Address { url, .. } => Some(url.as_str()),
            _ => None,
        })
    }

    #[test]
    fn creating_a_tab_reports_a_complete_load() {
        let (mut engine, events) = Engine::initialize(Path::new("/unused")).unwrap();
        engine
            .create_tab(7, 1, "https://www.example.com/page")
            .unwrap();
        let events = drain(&events);
        let summary: Vec<String> = events
            .iter()
            .map(|event| match event {
                EngineEvent::Created { tab_id } => format!("created {tab_id}"),
                EngineEvent::Loading { loading, .. } => format!("loading {loading}"),
                EngineEvent::Title { title, .. } => format!("title {title}"),
                EngineEvent::Address { url, .. } => format!("address {url}"),
                EngineEvent::Progress { progress, .. } => format!("progress {progress}"),
                EngineEvent::NewDocument { tab_id } => format!("new document {tab_id}"),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            summary,
            [
                "created 7",
                "loading true",
                "new document 7",
                "address https://www.example.com/page",
                "title Page",
                "progress 0.4",
                "progress 1",
                "loading false",
            ]
        );
        assert!(engine.has_tab(7));
    }

    #[test]
    fn history_drives_back_and_forward() {
        let (mut engine, events) = Engine::initialize(Path::new("/unused")).unwrap();
        engine.create_tab(1, 1, "https://a.test/").unwrap();
        assert_eq!(nav_state(&drain(&events)), Some((false, false)));
        engine.navigate(1, "https://b.test/");
        assert_eq!(nav_state(&drain(&events)), Some((true, false)));
        engine.back(1);
        let loaded = drain(&events);
        assert_eq!(address(&loaded), Some("https://a.test/"));
        assert_eq!(nav_state(&loaded), Some((false, true)));
        engine.back(1);
        assert!(
            drain(&events).is_empty(),
            "no history before the first page"
        );
        engine.forward(1);
        assert_eq!(address(&drain(&events)), Some("https://b.test/"));
        engine.back(1);
        drain(&events);
        engine.navigate(1, "https://c.test/");
        assert_eq!(nav_state(&drain(&events)), Some((true, false)));
        engine.reload(1);
        assert_eq!(address(&drain(&events)), Some("https://c.test/"));
    }

    #[test]
    fn creating_an_existing_tab_navigates_it() {
        let (mut engine, events) = Engine::initialize(Path::new("/unused")).unwrap();
        engine.create_tab(1, 1, "https://a.test/").unwrap();
        drain(&events);
        engine.create_tab(1, 1, "https://b.test/").unwrap();
        let loaded = drain(&events);
        assert!(
            !loaded
                .iter()
                .any(|event| matches!(event, EngineEvent::Created { .. }))
        );
        assert_eq!(nav_state(&loaded), Some((true, false)));
    }

    #[test]
    fn closing_unloads_and_allows_creating_again() {
        let (mut engine, events) = Engine::initialize(Path::new("/unused")).unwrap();
        engine.create_tab(1, 1, "about:blank").unwrap();
        drain(&events);
        engine.close_tab(1);
        assert!(matches!(
            drain(&events).as_slice(),
            [EngineEvent::Closed { tab_id: 1 }]
        ));
        assert!(!engine.has_tab(1));
        engine.navigate(1, "https://ignored.test/");
        assert!(drain(&events).is_empty());
        engine.create_tab(1, 1, "https://a.test/").unwrap();
        let loaded = drain(&events);
        assert!(matches!(
            loaded.first(),
            Some(EngineEvent::Created { tab_id: 1 })
        ));
        assert_eq!(nav_state(&loaded), Some((false, false)));
    }

    #[test]
    fn closed_before_creating_again_is_stale_for_the_shell() {
        let (mut engine, events) = Engine::initialize(Path::new("/unused")).unwrap();
        engine.create_tab(1, 1, "https://a.test/").unwrap();
        drain(&events);
        // Unloading and reselecting in one go, as "Unload Space" can.
        engine.close_tab(1);
        engine.create_tab(1, 1, "https://a.test/").unwrap();
        let events = drain(&events);
        assert!(matches!(
            events.first(),
            Some(EngineEvent::Closed { tab_id: 1 })
        ));
        // The shell ignores `Closed` while the tab has a page, as on macOS.
        assert!(engine.has_tab(1));
    }

    #[test]
    fn zoom_steps_like_chrome_per_tab() {
        let (mut engine, _events) = Engine::initialize(Path::new("/unused")).unwrap();
        engine.create_tab(1, 1, "https://a.test/").unwrap();
        engine.create_tab(2, 1, "https://b.test/").unwrap();
        assert_eq!(engine.zoom(1, ZoomStep::In), Some(110));
        assert_eq!(engine.zoom(1, ZoomStep::In), Some(125));
        assert_eq!(engine.zoom(2, ZoomStep::Out), Some(90));
        assert_eq!(engine.zoom(1, ZoomStep::Reset), Some(100));
        assert_eq!(engine.zoom(3, ZoomStep::In), None);
    }

    #[test]
    fn find_counts_matches_and_wraps_around() {
        let (mut engine, events) = Engine::initialize(Path::new("/unused")).unwrap();
        engine
            .create_tab(1, 1, "https://example.com/wiki/Example")
            .unwrap();
        drain(&events);
        let result = |events: &Receiver<EngineEvent>| match drain(events).as_slice() {
            [EngineEvent::FindResult { count, active, .. }] => (*count, *active),
            other => panic!("{other:?}"),
        };
        // The title "Example" and twice in the address.
        engine.find(1, "EXAMPLE", true, false);
        assert_eq!(result(&events), (3, 1));
        engine.find(1, "example", true, true);
        assert_eq!(result(&events), (3, 2));
        engine.find(1, "example", true, true);
        engine.find(1, "example", true, true);
        assert_eq!(drain(&events).len(), 2);
        engine.find(1, "example", false, true);
        assert_eq!(result(&events), (3, 3));
        engine.find(1, "nowhere", true, false);
        assert_eq!(result(&events), (0, 0));
    }

    #[test]
    fn demo_downloads_leave_the_page_alone() {
        let (mut engine, events) = Engine::initialize(Path::new("/unused")).unwrap();
        engine.create_tab(1, 1, "https://a.test/").unwrap();
        drain(&events);
        engine.navigate(1, "https://files.test/download-demo/Q3%20report.pdf");
        let started = drain(&events);
        assert!(matches!(
            started.as_slice(),
            [EngineEvent::DownloadStarted { tab_id: 1, name, blank_tab: false, .. }]
                if name == "Q3 report.pdf"
        ));
        // A new tab for a download never shows a page.
        engine
            .create_tab(2, 1, "https://files.test/download-demo")
            .unwrap();
        let events = drain(&events);
        assert!(matches!(
            events.as_slice(),
            [
                EngineEvent::Created { tab_id: 2 },
                EngineEvent::DownloadStarted {
                    tab_id: 2,
                    blank_tab: true,
                    ..
                }
            ]
        ));
        assert_eq!(engine.pages.borrow()[&1].entries, ["https://a.test/"]);
    }

    #[test]
    fn permission_requests_end_with_their_page() {
        let (mut engine, events) = Engine::initialize(Path::new("/unused")).unwrap();
        engine
            .create_tab(1, 1, "https://meet.test/permission-demo")
            .unwrap();
        let request = drain(&events).into_iter().find_map(|event| match event {
            EngineEvent::PermissionRequest {
                id,
                origin,
                permissions,
                ..
            } => Some((id, origin, permissions)),
            _ => None,
        });
        let (id, origin, permissions) = request.expect("a request");
        assert_eq!(origin, "https://meet.test/");
        assert_eq!(permissions, [Permission::Camera, Permission::Microphone]);
        // Answered requests are gone; a new page ends unanswered ones.
        engine.reload(1);
        assert!(drain(&events).iter().any(
            |event| matches!(event, EngineEvent::PermissionDismissed { id: dismissed } if *dismissed == id)
        ));
        let id = id + 1;
        engine.respond_permission(id, PermissionDecision::Allow);
        engine.close_tab(1);
        assert!(
            !drain(&events)
                .iter()
                .any(|event| matches!(event, EngineEvent::PermissionDismissed { .. }))
        );
    }

    #[test]
    fn titles_stay_readable() {
        for (url, title) in [
            (
                "https://en.wikipedia.org/wiki/Arc_(web_browser)",
                "Arc (web browser)",
            ),
            ("https://en.wikipedia.org/wiki/Caf%C3%A9", "Café"),
            ("https://www.example.com/", "example.com"),
            ("https://example.com", "example.com"),
            ("https://docs.rs/gpui/latest/", "Latest"),
            (
                "https://blog.example/2026/10/my-first-post.html",
                "My first post",
            ),
            ("https://www.google.com/search?q=rust+gpui", "rust gpui"),
            ("http://localhost:8080/?tab=1", "localhost"),
            ("about:blank", "about:blank"),
            ("not a url", "not a url"),
        ] {
            assert_eq!(page_title(url), title, "{url}");
        }
        assert_eq!(percent_decode("a%20b%zz%"), "a b%zz%");
    }
}
