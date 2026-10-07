//! Engine for platforms without the Chromium embedding (Linux previews and
//! tests). It shows no web pages, but every tab gets an in-memory history and
//! reports the events a quick page load would, so the shell's flows (titles,
//! progress, navigation buttons, closing) behave as they do on macOS.

use super::EngineEvent;
use crate::blocker::ContentBlocker;
use anyhow::Result;
use async_channel::{Receiver, Sender};
use gpui::http_client::Url;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    ffi::c_void,
    path::Path,
};

/// Methods mirror the macOS engine; `allow(dead_code)` marks those this
/// platform's shell never calls.
pub struct Engine {
    events: Sender<EngineEvent>,
    pages: RefCell<HashMap<u64, History>>,
}

struct History {
    entries: Vec<String>,
    index: usize,
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
        self.pages.borrow_mut().insert(
            tab_id,
            History {
                entries: vec![url.to_owned()],
                index: 0,
            },
        );
        self.send(EngineEvent::Created { tab_id });
        self.load(tab_id);
        Ok(())
    }

    pub fn set_blocker(&mut self, _blocker: ContentBlocker) {}
    pub fn set_blocking_exceptions(&self, _hosts: HashSet<String>) {}
    pub fn register_pending(&mut self) {}
    pub fn activate_tab(&mut self, _tab_id: u64) {}
    pub fn set_bounds(&mut self, _x: f64, _y: f64, _width: f64, _height: f64) {}
    pub fn set_transition(&mut self, _from: u64, _to: u64, _progress: f32, _direction: i32) {}
    pub fn focus_tab(&self, _tab_id: u64) {}
    pub fn focus_shell(&self) {}

    pub fn navigate(&self, tab_id: u64, url: &str) {
        {
            let mut pages = self.pages.borrow_mut();
            let Some(page) = pages.get_mut(&tab_id) else {
                return;
            };
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

    #[allow(dead_code)]
    pub fn stop(&self, _tab_id: u64) {}

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
            self.send(EngineEvent::Closed { tab_id });
        }
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
            let Some(page) = pages.get(&tab_id) else {
                return;
            };
            (
                page.entries[page.index].clone(),
                page.can_go_back(),
                page.can_go_forward(),
            )
        };
        let loading = |loading| EngineEvent::Loading {
            tab_id,
            loading,
            can_go_back,
            can_go_forward,
        };
        let title = page_title(&url);
        self.send(loading(true));
        self.send(EngineEvent::Address { tab_id, url });
        self.send(EngineEvent::Title { tab_id, title });
        for progress in [0.4, 1.0] {
            self.send(EngineEvent::Progress { tab_id, progress });
        }
        self.send(loading(false));
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
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            summary,
            [
                "created 7",
                "loading true",
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
