mod blocker;
mod engine;
mod input;
mod model;
#[cfg(target_os = "macos")]
mod native;
mod ui;

#[cfg(not(target_os = "macos"))]
mod native {
    /// Previews have no system setting; MAPBROWSER_REDUCE_MOTION stands in.
    pub fn reduce_motion() -> bool {
        std::env::var_os("MAPBROWSER_REDUCE_MOTION").is_some()
    }
}

use blocker::ContentBlocker;
use engine::{Engine, EngineEvent, Shortcut};
use gpui::http_client::Url;
use gpui::{prelude::*, *};
use input::{InputEvent, TextInput};
use model::{BrowserState, Space};
#[cfg(target_os = "macos")]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    cell::RefCell,
    collections::HashSet,
    rc::Rc,
    time::{Duration, Instant},
};

actions!(
    mapbrowser,
    [
        Quit,
        FocusAddress,
        NewTab,
        CloseTab,
        NextSpace,
        PreviousSpace,
        NextTab,
        Reload,
        Back,
        Forward
    ]
);

pub(crate) struct Slide {
    pub from: Space,
    pub start: Instant,
    pub direction: i32,
    pub frames: Vec<f64>,
}

pub(crate) struct BrowserApp {
    pub state: BrowserState,
    pub engine: Rc<RefCell<Engine>>,
    pub opened: HashSet<u64>,
    pub address: Entity<TextInput>,
    pub space_name: Entity<TextInput>,
    pub editing_space: Option<Option<u64>>,
    pub focus_handle: FocusHandle,
    pub slide: Option<Slide>,
    pub loading: HashSet<u64>,
    pub error: Option<String>,
    save_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// OS open-URL events are web addresses, never address-bar search queries.
fn external_web_url(value: &str) -> Option<Url> {
    let (scheme, authority_and_path) = value.split_once("://")?;
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https")
        || authority_and_path.is_empty()
        || authority_and_path.starts_with(['/', '?', '#'])
        || value.contains('\\')
        || value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return None;
    }
    let url = Url::parse(value).ok()?;
    url.has_host().then_some(url)
}

impl BrowserApp {
    fn new(
        state: BrowserState,
        engine: Rc<RefCell<Engine>>,
        events: async_channel::Receiver<EngineEvent>,
        external_urls: async_channel::Receiver<Url>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let address = cx.new(|cx| TextInput::new(window, cx));
        let space_name = cx.new(|cx| TextInput::new(window, cx));
        space_name.update(cx, |input, cx| input.set_placeholder("Space name", cx));
        let mut result = Self {
            state,
            engine,
            opened: HashSet::new(),
            address: address.clone(),
            space_name: space_name.clone(),
            editing_space: None,
            focus_handle: cx.focus_handle(),
            slide: None,
            loading: HashSet::new(),
            error: None,
            save_task: None,
            _subscriptions: vec![],
        };
        result._subscriptions.push(cx.subscribe_in(
            &address,
            window,
            |this, _, event, window, cx| match event {
                InputEvent::Submit(value) => this.navigate(value, window, cx),
                InputEvent::Cancel => {
                    this.sync_address(cx);
                    window.focus(&this.focus_handle);
                }
            },
        ));
        result._subscriptions.push(cx.subscribe_in(
            &space_name,
            window,
            |this, _, event, window, cx| match event {
                InputEvent::Submit(value) => {
                    let name = value.trim();
                    if name.is_empty() {
                        return;
                    }
                    match this.editing_space.take() {
                        Some(Some(id)) => {
                            this.state.rename_space(id, name);
                        }
                        Some(None) => {
                            this.finish_slide();
                            let from = this.state.active_space().clone();
                            let id = this.state.create_space(name.to_owned());
                            this.state.select_space(id);
                            this.begin_slide(from, 1, window, cx);
                        }
                        None => {}
                    }
                    this.persist();
                    window.focus(&this.focus_handle);
                    cx.notify();
                }
                InputEvent::Cancel => {
                    this.editing_space = None;
                    window.focus(&this.focus_handle);
                    cx.notify();
                }
            },
        ));
        result._subscriptions.push(cx.on_app_quit(|this, _| {
            this.persist();
            async {}
        }));
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this
                    .update_in(cx, |this, window, cx| this.engine_event(event, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        // The channel also retains URLs delivered before the launch window exists.
        // Defer mutations until the native callback has returned to the run loop.
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(url) = external_urls.recv().await {
                if this
                    .update_in(cx, |this, window, cx| {
                        this.open_external_url(url, window, cx)
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        result.restore_tabs();
        result.sync_address(cx);
        result
    }

    fn persist(&mut self) {
        if let Err(error) = self.state.save() {
            self.error = Some(format!("Couldn't save this session: {error}"));
        }
    }

    fn persist_later(&mut self, cx: &mut Context<Self>) {
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let _ = this.update(cx, |this, _| this.persist());
        }));
    }

    fn create_browser(&mut self, tab_id: u64, space_id: u64, url: &str) {
        match self.engine.borrow_mut().create_tab(tab_id, space_id, url) {
            Ok(()) => {
                self.opened.insert(tab_id);
            }
            Err(error) => {
                eprintln!("Couldn't create Chromium tab: {error:#}");
                self.error = Some(format!("Couldn't open page: {error}"));
            }
        }
    }

    fn restore_tabs(&mut self) {
        for space in self.state.spaces.clone() {
            for tab in space.tabs {
                if tab.url != "about:blank" {
                    self.create_browser(tab.id, space.id, &tab.url);
                }
            }
        }
        self.engine
            .borrow_mut()
            .activate_tab(self.state.active_tab().id);
    }

    fn sync_address(&mut self, cx: &mut Context<Self>) {
        let url = &self.state.active_tab().url;
        self.address.update(cx, |input, cx| {
            input.set_text(if url == "about:blank" { "" } else { url }, cx)
        });
    }

    fn open_external_url(&mut self, url: Url, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_slide();
        let space_id = self.state.selected_space_id;
        let Some(tab_id) = self.state.new_tab(space_id, url.as_str()) else {
            return;
        };
        self.error = None;
        self.create_browser(tab_id, space_id, url.as_str());
        self.engine.borrow_mut().activate_tab(tab_id);
        self.sync_address(cx);
        self.persist();
        window.focus(&self.focus_handle);
        window.activate_window();
        cx.activate(true);
        cx.notify();
    }

    pub fn navigate(&mut self, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        if value.trim().is_empty() {
            return;
        }
        self.finish_slide();
        let url = model::normalize_address(value);
        let tab_id = self.state.active_tab().id;
        let space_id = self.state.active_space().id;
        self.error = None;
        self.state.update_tab(tab_id, &url, "");
        if !self.opened.contains(&tab_id) {
            self.create_browser(tab_id, space_id, &url);
        } else {
            self.engine.borrow().navigate(tab_id, &url);
        }
        self.engine.borrow_mut().activate_tab(tab_id);
        self.sync_address(cx);
        self.persist();
        window.focus(&self.focus_handle);
        self.engine.borrow().focus_tab(tab_id);
        cx.notify();
    }

    pub fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.engine.borrow().focus_shell();
        self.finish_slide();
        self.state
            .new_tab(self.state.selected_space_id, "about:blank");
        self.engine
            .borrow_mut()
            .activate_tab(self.state.active_tab().id);
        self.error = None;
        self.sync_address(cx);
        self.persist();
        self.address
            .update(cx, |input, cx| input.select_all_focus(window, cx));
        cx.notify();
    }

    pub fn select_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if !self
            .state
            .active_space()
            .tabs
            .iter()
            .any(|tab| tab.id == id)
        {
            return;
        }
        self.finish_slide();
        self.state.select_tab(self.state.selected_space_id, id);
        self.engine.borrow_mut().activate_tab(id);
        self.error = None;
        self.sync_address(cx);
        self.persist();
        window.focus(&self.focus_handle);
        cx.notify();
    }

    pub fn close_tab(&mut self, id: u64, cx: &mut Context<Self>) {
        if !self
            .state
            .active_space()
            .tabs
            .iter()
            .any(|tab| tab.id == id)
        {
            return;
        }
        self.finish_slide();
        if self.opened.remove(&id) {
            self.engine.borrow_mut().close_tab(id);
        }
        self.state.close_tab(self.state.selected_space_id, id);
        self.loading.remove(&id);
        self.engine
            .borrow_mut()
            .activate_tab(self.state.active_tab().id);
        self.sync_address(cx);
        self.persist();
        cx.notify();
    }

    fn finish_slide(&mut self) {
        if self.slide.take().is_some() {
            self.engine
                .borrow_mut()
                .activate_tab(self.state.active_tab().id);
        }
    }

    pub fn switch_space(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if id == self.state.selected_space_id {
            return;
        }
        self.finish_slide();
        let from = self.state.active_space().clone();
        let from_index = self
            .state
            .spaces
            .iter()
            .position(|space| space.id == from.id)
            .unwrap_or(0);
        let to_index = self
            .state
            .spaces
            .iter()
            .position(|space| space.id == id)
            .unwrap_or(0);
        if !self.state.select_space(id) {
            return;
        }
        self.begin_slide(from, if to_index > from_index { 1 } else { -1 }, window, cx);
        self.persist_later(cx);
    }

    fn begin_slide(
        &mut self,
        from: Space,
        direction: i32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if native::reduce_motion() {
            self.slide = None;
            self.engine
                .borrow_mut()
                .activate_tab(self.state.active_tab().id);
        } else {
            self.engine.borrow().focus_shell();
            self.slide = Some(Slide {
                from,
                start: Instant::now(),
                direction,
                frames: vec![],
            });
        }
        self.error = None;
        self.sync_address(cx);
        window.focus(&self.focus_handle);
        cx.notify();
    }

    pub fn edit_space(&mut self, id: Option<u64>, window: &mut Window, cx: &mut Context<Self>) {
        self.engine.borrow().focus_shell();
        self.editing_space = Some(id);
        let name = id
            .and_then(|id| self.state.space(id))
            .map(|s| s.name.clone())
            .unwrap_or_default();
        self.space_name.update(cx, |input, cx| {
            input.set_text(&name, cx);
            input.select_all_focus(window, cx);
        });
        cx.notify();
    }

    pub fn shortcut(&mut self, shortcut: Shortcut, window: &mut Window, cx: &mut Context<Self>) {
        match shortcut {
            Shortcut::Address => {
                self.engine.borrow().focus_shell();
                self.address
                    .update(cx, |input, cx| input.select_all_focus(window, cx));
            }
            Shortcut::NewTab => self.new_tab(window, cx),
            Shortcut::CloseTab => self.close_tab(self.state.active_tab().id, cx),
            Shortcut::Reload => self.engine.borrow().reload(self.state.active_tab().id),
            Shortcut::Back => self.engine.borrow().back(self.state.active_tab().id),
            Shortcut::Forward => self.engine.borrow().forward(self.state.active_tab().id),
            Shortcut::PreviousSpace | Shortcut::NextSpace => {
                let index = self
                    .state
                    .spaces
                    .iter()
                    .position(|s| s.id == self.state.selected_space_id)
                    .unwrap_or(0);
                let count = self.state.spaces.len();
                let next = if matches!(shortcut, Shortcut::NextSpace) {
                    (index + 1) % count
                } else {
                    (index + count - 1) % count
                };
                self.switch_space(self.state.spaces[next].id, window, cx);
            }
            Shortcut::NextTab => {
                let space = self.state.active_space();
                let index = space
                    .tabs
                    .iter()
                    .position(|t| t.id == space.selected_tab_id)
                    .unwrap_or(0);
                self.select_tab(space.tabs[(index + 1) % space.tabs.len()].id, window, cx);
            }
        }
    }

    fn engine_event(&mut self, event: EngineEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            EngineEvent::Created { .. } => {
                self.engine.borrow_mut().register_pending();
            }
            EngineEvent::Title { tab_id, title } => {
                if let Some(tab) = self.state.tab(tab_id) {
                    let url = tab.url.clone();
                    self.state.update_tab(tab_id, url, title);
                    self.persist_later(cx);
                }
            }
            EngineEvent::Address { tab_id, url } => {
                if let Some(tab) = self.state.tab(tab_id) {
                    let title = tab.title.clone();
                    self.state.update_tab(tab_id, url, title);
                    self.persist_later(cx);
                }
                if tab_id == self.state.active_tab().id
                    && !self.address.read(cx).focus_handle(cx).is_focused(window)
                {
                    self.sync_address(cx);
                }
            }
            EngineEvent::Loading {
                tab_id, loading, ..
            } => {
                if loading {
                    self.loading.insert(tab_id);
                } else {
                    self.loading.remove(&tab_id);
                }
            }
            EngineEvent::OpenTab { opener_tab_id, url } => {
                let space_id = self
                    .state
                    .spaces
                    .iter()
                    .find(|s| s.tabs.iter().any(|t| t.id == opener_tab_id))
                    .map(|s| s.id)
                    .unwrap_or(self.state.selected_space_id);
                if let Some(id) = self.state.new_tab(space_id, &url) {
                    self.create_browser(id, space_id, &url);
                    if space_id == self.state.selected_space_id {
                        self.engine.borrow_mut().activate_tab(id);
                        self.sync_address(cx);
                    }
                }
                self.persist();
            }
            EngineEvent::LoadError { tab_id, message } => {
                if tab_id == self.state.active_tab().id {
                    self.error = Some(message);
                }
                self.loading.remove(&tab_id);
            }
            EngineEvent::Closed { tab_id } => {
                self.opened.remove(&tab_id);
                self.engine.borrow_mut().forget_closed(tab_id);
            }
            EngineEvent::Shortcut(shortcut) => self.shortcut(shortcut, window, cx),
        }
        cx.notify();
    }

    pub fn accent(&self, id: u64) -> u32 {
        const COLORS: [u32; 6] = [0xb6dace, 0xc6c0eb, 0xe8ca9d, 0xaecded, 0xe2b9c8, 0xc9d99f];
        COLORS[self
            .state
            .spaces
            .iter()
            .position(|s| s.id == id)
            .unwrap_or(0)
            % COLORS.len()]
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let state = BrowserState::load()?;
    let blocker = ContentBlocker::load_in_background(state.root_dir().join("blocker"));
    let (mut engine, events) = Engine::initialize(state.root_dir())?;
    engine.set_blocker(blocker);
    let engine = Rc::new(RefCell::new(engine));
    let retained_engine = engine.clone();
    let (external_urls_tx, external_urls_rx) = async_channel::unbounded();
    let application = Application::new();
    application.on_open_urls(move |urls| {
        for url in urls.iter().filter_map(|url| external_web_url(url)) {
            let _ = external_urls_tx.try_send(url);
        }
    });
    application.run(move |cx: &mut App| {
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-l", FocusAddress, Some("Browser")),
            KeyBinding::new("cmd-t", NewTab, Some("Browser")),
            KeyBinding::new("cmd-w", CloseTab, Some("Browser")),
            KeyBinding::new("cmd-alt-right", NextSpace, Some("Browser")),
            KeyBinding::new("cmd-alt-left", PreviousSpace, Some("Browser")),
            KeyBinding::new("ctrl-tab", NextTab, Some("Browser")),
            KeyBinding::new("cmd-r", Reload, Some("Browser")),
            KeyBinding::new("cmd-[", Back, Some("Browser")),
            KeyBinding::new("cmd-]", Forward, Some("Browser")),
        ]);
        cx.set_menus(vec![Menu {
            name: "Map".into(),
            items: vec![MenuItem::action("Quit Map", Quit)],
        }]);
        let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(820.), px(540.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Map Browser".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(20.), px(24.))),
                }),
                ..Default::default()
            },
            |window, cx| {
                #[cfg(target_os = "macos")]
                if let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window)
                    .expect("native window handle")
                    .as_raw()
                {
                    engine
                        .borrow_mut()
                        .attach(handle.ns_view.as_ptr())
                        .expect("attach browser view");
                }
                window.on_window_should_close(cx, |_, cx| {
                    cx.quit();
                    true
                });
                cx.new(|cx| BrowserApp::new(state, engine, events, external_urls_rx, window, cx))
            },
        )
        .expect("open browser window");
        cx.activate(true);
    });
    retained_engine.borrow_mut().shutdown()?;
    Ok(())
}

#[cfg(test)]
mod external_url_tests {
    use super::external_web_url;

    #[test]
    fn accepts_absolute_http_urls_and_preserves_navigation_parts() {
        for (input, expected) in [
            (
                "https://example.com/path?q=hello%20world#part",
                "https://example.com/path?q=hello%20world#part",
            ),
            ("HTTP://EXAMPLE.COM:80/page", "http://example.com/page"),
            ("http://localhost:8123/test", "http://localhost:8123/test"),
            ("http://[::1]:8123/test", "http://[::1]:8123/test"),
            ("https://bücher.example/", "https://xn--bcher-kva.example/"),
        ] {
            assert_eq!(external_web_url(input).unwrap().as_str(), expected);
        }
    }

    #[test]
    fn rejects_non_web_or_malformed_external_urls_instead_of_searching() {
        for input in [
            "",
            "example.com",
            "/relative",
            "//example.com",
            "http:example.com",
            "https://",
            "https:///example.com",
            "https://?query",
            "https://#fragment",
            "https://bad host/",
            " https://example.com",
            "https://example.com\n",
            "https://example.com\\@other.example",
            "https://[::1",
            "https://example.com:99999",
            "javascript:alert(1)",
            "javascript://example.com/alert(1)",
            "file:///tmp/page.html",
            "data:text/html,hello",
            "ftp://example.com/file",
            "mailto:test@example.com",
            "mapbrowser://settings",
        ] {
            assert!(external_web_url(input).is_none(), "accepted {input:?}");
        }
    }
}
