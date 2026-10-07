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

/// Stand-in page title: the host without "www.", or the URL if it has none.
fn page_title(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|url| {
            url.host_str()
                .map(|host| host.trim_start_matches("www.").to_owned())
        })
        .unwrap_or_else(|| url.to_owned())
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

    pub fn has_tab(&self, tab_id: u64) -> bool {
        self.pages.borrow().contains_key(&tab_id)
    }

    pub fn forget_closed(&mut self, _tab_id: u64) {}

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
                "title example.com",
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
}
