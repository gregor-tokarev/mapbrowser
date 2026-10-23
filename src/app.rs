//! The browser window's state and behaviour: spaces and tabs, the engine's
//! page events, the command bar and persistence. Rendering lives in `ui`.

use crate::{
    engine::{Engine, EngineEvent, Navigation, Shortcut, ZoomStep, navigation_for},
    favicons::{FaviconStore, host_of},
    input::{InputEvent, InputStyle, TextInput},
    model::{self, BrowserState, ClosedTab, Pin, Recovery, Space, SpaceId, Tab, TabId},
    native,
    recent::{RecentTabs, TabSwitch},
    ui::{
        command_bar::{
            self, ActiveTab, Command, CommandBar, CommandBarEvent, CommandContext, CommandMode,
            TabEntry,
        },
        downloads::Downloads,
        drag::{self, DropZones, TabDrag},
        find_bar::Find,
        link_status::LinkStatus,
        menu::{MenuKind, OpenMenu},
        permissions::PermissionPrompts,
        redraw::{self, Drawn},
        shortcuts::ShortcutsWindow,
        swipe::SwipeTracker,
        theme::Theme,
        toast_window::{Toast, ToastKind, ToastWindow},
        window_frame::WindowFrame,
    },
};
use gpui::{
    App, AppContext, ClipboardItem, Context, Entity, ExternalPaths, FocusHandle, Focusable,
    MouseDownEvent, Pixels, Point, PromptLevel, ScrollWheelEvent, SharedString, Subscription, Task,
    Window, WindowAppearance, WindowHandle, http_client::Url, px,
};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    time::{Duration, Instant},
};

/// Changes are saved this long after the first one, so a burst writes once.
const SAVE_DELAY: Duration = Duration::from_millis(500);
/// Page titles and addresses can change every second (timers, chat badges,
/// tickers), in every space: those changes alone are saved this rarely.
const PAGE_SAVE_DELAY: Duration = Duration::from_secs(10);

/// Navigation state of a loaded tab, for the toolbar and the progress bar.
#[derive(Clone, Copy, Debug, Default)]
pub struct PageStatus {
    pub loading: bool,
    pub progress: f64,
    pub can_go_back: bool,
    pub can_go_forward: bool,
}

/// A space switch in progress: the sidebar pages, the window colours and
/// the native page views move from `from` to the active space.
pub struct Slide {
    pub from: Space,
    pub start: Instant,
    /// 1 when the new space is to the right of the old one, else -1.
    pub direction: f32,
    /// Milliseconds since `start` of each frame, for MAPBROWSER_TRACE_ANIMATION.
    pub frames: Vec<f32>,
}

pub struct CommandBarWindow {
    pub mode: CommandMode,
    handle: WindowHandle<CommandBar>,
    /// The pop-up's NSView, attached to the browser window as a child.
    #[cfg(target_os = "macos")]
    native_view: Option<*mut std::ffi::c_void>,
    _subscription: Subscription,
}

pub struct BrowserApp {
    pub state: BrowserState,
    pub engine: Rc<RefCell<Engine>>,
    pub favicons: FaviconStore,
    /// Tabs that have a browser, created or still being created. Every other
    /// tab is unloaded: it shows its saved title and cached icon only.
    loaded: HashSet<TabId>,
    pages: HashMap<TabId, PageStatus>,
    /// The active tab's last load error.
    pub error: Option<String>,
    pub slide: Option<Slide>,
    pub menu: Option<OpenMenu>,
    pub toast: Option<Toast>,
    pub sidebar_hovered: bool,
    /// The row under the pointer, which shows its close button.
    pub hovered_tab: Option<TabId>,
    /// The URL pill shows its buttons only under the pointer.
    pub url_pill_hovered: bool,
    pub tab_drag: Option<TabDrag>,
    /// Measured while drawing the sidebar, for dropping dragged tabs.
    pub drop_zones: Rc<RefCell<DropZones>>,
    swipe: SwipeTracker,
    pub renaming: Option<SpaceId>,
    pub rename_input: Entity<TextInput>,
    pub command_bar: Option<CommandBarWindow>,
    pub dark: bool,
    /// A page is in HTML fullscreen: no sidebar, no card insets.
    pub fullscreen: bool,
    /// The window entered fullscreen for the page and leaves with it.
    window_fullscreen_for_page: bool,
    /// Whether the window was fullscreen when its bounds last changed.
    window_fullscreen: bool,
    pub focus_handle: FocusHandle,
    save_task: Option<Task<()>>,
    /// When `save_task` writes.
    save_due: Option<Instant>,
    pub downloads: Downloads,
    pub permissions: PermissionPrompts,
    /// Tabs pages opened this session, and the tab that opened each.
    pub openers: HashMap<TabId, TabId>,
    _subscriptions: Vec<Subscription>,
    recent: RecentTabs,
    /// A ⌃Tab walk, while ⌃ is held; the sidebar marks its target.
    pub tab_switch: Option<TabSwitch>,
    window_frame: WindowFrame,
    pub find: Find,
    /// The toast over the card while the sidebar is hidden.
    pub(crate) toast_window: Option<ToastWindow>,
    pub link_status: LinkStatus,
    /// Why the last save failed, until one succeeds; a notice says so.
    pub(crate) save_failure: Option<SharedString>,
    /// What the menu bar shows now.
    menu_bar: crate::MenuBarState,
    /// Help > Keyboard Shortcuts, while open.
    pub(crate) shortcuts: Option<ShortcutsWindow>,
}

/// OS open-URL events are web addresses, never address-bar search queries.
pub fn external_web_url(value: &str) -> Option<Url> {
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

/// Files a tab shows, as the bundle declares for Finder: pages, PDFs,
/// images and text. Web locations (.webloc) open their address.
const SHOWN_FILE_EXTENSIONS: [&str; 15] = [
    "html", "htm", "xhtml", "xht", "shtml", "pdf", "svg", "txt", "png", "jpg", "jpeg", "gif",
    "webp", "avif", "bmp",
];

/// What a link or file opened from another app (Finder, the Dock, `open
/// -a`) opens: a web address, a file a tab can show, or a .webloc's
/// address. Only this path, and dropping files on the sidebar, opens file
/// URLs; other files and schemes open nothing.
pub fn opened_address(value: &str) -> Option<Url> {
    if let Some(url) = external_web_url(value) {
        return Some(url);
    }
    let url = Url::parse(value)
        .ok()
        .filter(|url| url.scheme() == "file")?;
    opened_file(&url.to_file_path().ok()?)
}

/// The address a file opens at, if a tab can show it.
pub fn opened_file(path: &std::path::Path) -> Option<Url> {
    if !path.is_file() {
        return None;
    }
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if extension == "webloc" {
        return external_web_url(&native::webloc_address(path)?);
    }
    SHOWN_FILE_EXTENSIONS
        .contains(&extension.as_str())
        .then(|| Url::from_file_path(path).ok())
        .flatten()
}

/// The address in a .webloc's XML property list: the string after its URL
/// key.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn webloc_address_in_xml(plist: &str) -> Option<String> {
    const KEY: &str = "<key>URL</key>";
    let after_key = &plist[plist.find(KEY)? + KEY.len()..];
    let value = after_key.trim_start().strip_prefix("<string>")?;
    let value = &value[..value.find("</string>")?];
    Some(
        value
            .trim()
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

/// A preview's command-line argument: an address or a file's path.
#[cfg(not(target_os = "macos"))]
pub fn argument_address(argument: &str) -> Option<Url> {
    opened_address(argument).or_else(|| opened_file(&std::path::absolute(argument).ok()?))
}

/// What the shell draws of a tab's URL: on macOS the host (the URL pill,
/// the favicon) or, without one, the whole URL. Linux previews print the
/// whole URL on the card.
fn shown_url(url: &str) -> String {
    match host_of(url) {
        Some(host) if cfg!(target_os = "macos") => host,
        _ => model::display_url(url),
    }
}

fn is_dark(appearance: WindowAppearance) -> bool {
    matches!(
        appearance,
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

/// ⌘1–⌘8 pick tabs in sidebar order; ⌘9 always picks the last tab.
pub fn tab_for_number(tabs: &[Tab], number: u8) -> Option<TabId> {
    let tab = if number == 9 {
        tabs.last()
    } else {
        tabs.get(usize::from(number).checked_sub(1)?)
    };
    tab.map(|tab| tab.id)
}

/// The tab after (or before) `current` in sidebar order, wrapping around.
pub fn neighbour_tab(tabs: &[Tab], current: Option<TabId>, forward: bool) -> Option<TabId> {
    let count = tabs.len();
    let next = match current.and_then(|id| tabs.iter().position(|tab| tab.id == id)) {
        Some(index) if forward => (index + 1) % count,
        Some(index) => (index + count - 1) % count,
        None if forward => 0,
        None => count.checked_sub(1)?,
    };
    tabs.get(next).map(|tab| tab.id)
}

impl BrowserApp {
    pub fn new(
        state: BrowserState,
        engine: Rc<RefCell<Engine>>,
        events: async_channel::Receiver<EngineEvent>,
        external_urls: async_channel::Receiver<Url>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let rename_input = cx.new(|cx| TextInput::new(window, cx));
        let favicons = FaviconStore::new(state.root_dir());
        let mut this = Self {
            state,
            engine,
            favicons,
            loaded: HashSet::new(),
            pages: HashMap::new(),
            error: None,
            slide: None,
            menu: None,
            toast: None,
            sidebar_hovered: false,
            hovered_tab: None,
            url_pill_hovered: false,
            tab_drag: None,
            drop_zones: Rc::default(),
            swipe: SwipeTracker::default(),
            renaming: None,
            rename_input: rename_input.clone(),
            command_bar: None,
            dark: is_dark(window.appearance()),
            fullscreen: false,
            window_fullscreen_for_page: false,
            window_fullscreen: window.is_fullscreen(),
            focus_handle: cx.focus_handle(),
            save_task: None,
            save_due: None,
            downloads: Downloads::default(),
            permissions: PermissionPrompts::default(),
            openers: HashMap::new(),
            _subscriptions: Vec::new(),
            recent: RecentTabs::default(),
            tab_switch: None,
            window_frame: WindowFrame::default(),
            find: Find::default(),
            toast_window: None,
            link_status: LinkStatus::default(),
            save_failure: None,
            menu_bar: crate::MenuBarState::default(),
            shortcuts: None,
        };
        this._subscriptions.extend([
            cx.subscribe_in(
                &rename_input,
                window,
                |this, _, event, window, cx| match event {
                    InputEvent::Submit(name) => this.finish_rename(Some(name), window, cx),
                    InputEvent::Cancel => this.finish_rename(None, window, cx),
                    InputEvent::Changed(_) => {}
                },
            ),
            cx.on_blur(
                &rename_input.focus_handle(cx),
                window,
                |this, window, cx| this.end_rename(window, cx),
            ),
            cx.observe_window_appearance(window, |this, window, cx| {
                let dark = is_dark(window.appearance());
                if this.dark != dark {
                    this.dark = dark;
                    cx.notify();
                }
            }),
            // Only the change from fullscreen to windowed: entering is
            // asynchronous, and a page's request must not be undone early.
            cx.observe_window_bounds(window, |this, window, cx| {
                let fullscreen = window.is_fullscreen();
                if std::mem::replace(&mut this.window_fullscreen, fullscreen) && !fullscreen {
                    this.window_left_fullscreen(window, cx);
                }
                let frame = this
                    .window_frame
                    .saved(window, cx, this.window_fullscreen_for_page);
                if this.state.window.as_ref() != Some(&frame) {
                    this.state.window = Some(frame);
                    this.persist_page_later(cx);
                }
            }),
            cx.on_app_quit(|this, cx| {
                // The name being typed counts, as when a rename ends otherwise.
                if let Some(id) = this.renaming.take() {
                    let name = this.rename_input.read(cx).text().to_owned();
                    this.state.rename_space(id, name);
                }
                this.save_now();
                async {}
            }),
            // ⌃ can be let go of in another app.
            cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() && this.tab_switch.take().is_some() {
                    cx.notify();
                }
            }),
        ]);
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
        // Links that launched the app open now, before the saved selection
        // gets a browser: only the last of them loads (lazy restore).
        let space_id = this.state.selected_space_id;
        let mut launched_with_links = false;
        while let Ok(url) = external_urls.try_recv() {
            let address = model::normalize_address(url.as_str());
            launched_with_links |= this.state.new_tab(space_id, address).is_some();
        }
        if launched_with_links {
            this.persist_later(cx);
        }
        // Links opened later arrive here.
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(url) = external_urls.recv().await {
                if this
                    .update_in(cx, |this, window, cx| {
                        this.open_url(url.as_str(), cx);
                        window.activate_window();
                        cx.activate(true);
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        {
            let engine = this.engine.borrow();
            engine.set_blocking_exceptions(this.state.blocking_exceptions());
            engine.set_traffic_lights_hidden(this.state.sidebar_hidden);
        }
        let closed = this.state.closed_tabs().iter().map(|closed| &closed.tab);
        let hosts = this
            .state
            .spaces
            .iter()
            .flat_map(|space| &space.tabs)
            .chain(closed)
            .filter_map(|tab| host_of(&tab.url))
            .collect();
        this.load_favicons(hosts, cx);
        // Lazy restore: only the tab on screen gets a browser now.
        this.show_selected();
        if let Some(recovery) = this.state.recovery.take() {
            cx.defer_in(window, move |this, window, cx| {
                this.tell_recovery(recovery, window, cx)
            });
        }
        this
    }

    pub fn theme(&self) -> Theme {
        Theme::new(self.state.active_space().theme(), self.dark)
    }

    pub fn is_loaded(&self, tab_id: TabId) -> bool {
        self.loaded.contains(&tab_id)
    }

    pub fn page(&self, tab_id: TabId) -> PageStatus {
        self.pages.get(&tab_id).copied().unwrap_or_default()
    }

    pub fn active_tab_id(&self) -> Option<TabId> {
        self.state.active_tab().map(|tab| tab.id)
    }

    fn is_active(&self, tab_id: TabId) -> bool {
        self.active_tab_id() == Some(tab_id)
    }

    /// Whether the tab is in a space the sidebar shows, if it is shown.
    fn is_visible(&self, tab_id: TabId) -> bool {
        let shown = |space: &Space| space.tabs.iter().any(|tab| tab.id == tab_id);
        shown(self.state.active_space()) || self.slide.as_ref().is_some_and(|s| shown(&s.from))
    }

    /// What the window draws of the tab, so its page's events redraw only
    /// for changes to that.
    fn drawn(&self, tab_id: TabId) -> Drawn {
        redraw::drawn(
            self.sidebar_width().is_some(),
            self.is_visible(tab_id),
            self.state.tab(tab_id).map_or(Pin::Today, |tab| tab.pin),
            self.is_active(tab_id),
            self.fullscreen,
        )
    }

    fn load_favicons(&mut self, hosts: Vec<String>, cx: &mut Context<Self>) {
        let task = self.favicons.load_for(hosts, cx.background_executor());
        cx.spawn(async move |this, cx| {
            let loaded = task.await;
            this.update(cx, |this, cx| {
                // Only the sidebar shows favicons.
                if this.favicons.insert_loaded(loaded) && this.sidebar_width().is_some() {
                    cx.notify();
                }
            })
        })
        .detach();
    }

    fn ensure_loaded(&mut self, tab_id: TabId) {
        if self.loaded.contains(&tab_id) {
            return;
        }
        let (Some(space_id), Some(tab)) = (self.state.space_of_tab(tab_id), self.state.tab(tab_id))
        else {
            return;
        };
        let url = tab.url.clone();
        match self.engine.borrow_mut().create_tab(tab_id, space_id, &url) {
            Ok(()) => {
                self.loaded.insert(tab_id);
            }
            Err(error) => {
                eprintln!("Couldn't create Chromium tab: {error:#}");
                self.error = Some(format!("Couldn't open page: {error}"));
            }
        }
    }

    /// A tab still downloading keeps its browser until the download ends:
    /// Chromium reports nothing more once it closes.
    fn drop_browser(&mut self, tab_id: TabId) {
        if self.loaded.remove(&tab_id) && !self.downloads.running_in(tab_id) {
            self.engine.borrow_mut().close_tab(tab_id);
        }
        self.pages.remove(&tab_id);
    }

    /// Shows the active space's selected tab, creating its browser if needed.
    fn show_selected(&mut self) {
        let active = self.active_tab_id();
        if let Some(id) = active {
            self.ensure_loaded(id);
        }
        // The engine hides every page for an unknown id; 0 is never a tab id.
        self.engine.borrow_mut().activate_tab(active.unwrap_or(0));
        self.recent.visit(self.state.active_space());
    }

    pub(crate) fn focus_page(&self) {
        if let Some(id) = self.active_tab_id() {
            self.engine.borrow().focus_tab(id);
        }
    }

    fn finish_slide(&mut self) {
        if self.slide.take().is_some() {
            self.show_selected();
        }
    }

    fn exit_page_fullscreen(&self) {
        if self.fullscreen
            && let Some(id) = self.active_tab_id()
        {
            self.engine.borrow().exit_fullscreen(id);
        }
    }

    /// Selects `tab_id` (or keeps the space's selection) in `space_id`,
    /// sliding over from the current space if it is another one.
    fn go_to(
        &mut self,
        space_id: SpaceId,
        tab_id: Option<TabId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.end_rename(window, cx);
        self.finish_slide();
        self.exit_page_fullscreen();
        self.menu = None;
        self.tab_switch = None;
        self.close_command_bar(cx);
        let from = self.state.selected_space_id;
        let selected = match tab_id {
            Some(tab_id) => self.state.select_tab(space_id, tab_id),
            None => self.state.select_space(space_id),
        };
        if !selected {
            return;
        }
        self.error = None;
        match self.state.space(from).filter(|_| from != space_id).cloned() {
            Some(from) => self.begin_slide(from, window),
            None => self.show_selected(),
        }
        self.persist_later(cx);
        cx.notify();
    }

    fn begin_slide(&mut self, from: Space, window: &mut Window) {
        let index = |id| self.state.spaces.iter().position(|space| space.id == id);
        let direction = if index(self.state.selected_space_id) >= index(from.id) {
            1.
        } else {
            -1.
        };
        if native::reduce_motion() {
            self.show_selected();
            return;
        }
        // The incoming page's browser starts loading while the views slide.
        if let Some(id) = self.active_tab_id() {
            self.ensure_loaded(id);
        }
        self.recent.visit(self.state.active_space());
        self.engine.borrow().focus_shell();
        window.focus(&self.focus_handle);
        self.slide = Some(Slide {
            from,
            start: Instant::now(),
            direction,
            frames: Vec::new(),
        });
    }

    pub fn select_tab(&mut self, tab_id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(space_id) = self.state.space_of_tab(tab_id) {
            self.go_to(space_id, Some(tab_id), window, cx);
        }
    }

    pub fn switch_space(&mut self, space_id: SpaceId, window: &mut Window, cx: &mut Context<Self>) {
        if space_id != self.state.selected_space_id {
            self.go_to(space_id, None, window, cx);
        }
    }

    /// Opens a new Today tab in the active space, for an address or a search.
    pub fn open_url(&mut self, input: &str, cx: &mut Context<Self>) {
        if input.trim().is_empty() {
            return;
        }
        self.finish_slide();
        self.exit_page_fullscreen();
        let space_id = self.state.selected_space_id;
        let Some(tab_id) = self
            .state
            .new_tab(space_id, model::normalize_address(input))
        else {
            return;
        };
        self.error = None;
        self.show_selected();
        if let Some(host) = self.state.tab(tab_id).and_then(|tab| host_of(&tab.url)) {
            self.load_favicons(vec![host], cx);
        }
        self.persist_later(cx);
        cx.notify();
    }

    /// Loads an address or search in the active tab (⌘L).
    pub fn navigate_active(&mut self, input: &str, cx: &mut Context<Self>) {
        let Some(tab_id) = self.active_tab_id() else {
            return self.open_url(input, cx);
        };
        if input.trim().is_empty() {
            return;
        }
        let url = model::normalize_address(input);
        self.error = None;
        if self.loaded.contains(&tab_id) {
            // The tab follows the page: `Address` reports the new page once
            // it loads, `LoadError` an address that failed. Downloads and
            // empty responses leave the page, and so the tab, as they were.
            self.engine.borrow().navigate(tab_id, &url);
            self.engine.borrow().focus_tab(tab_id);
        } else {
            self.state.update_tab(tab_id, &url, "");
            self.show_selected();
            self.persist_later(cx);
        }
        cx.notify();
    }

    pub fn reload(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if self.is_active(tab_id) {
            self.error = None;
            cx.notify();
        }
        if self.loaded.contains(&tab_id) {
            self.engine.borrow().reload(tab_id);
        } else if self.is_active(tab_id) {
            self.show_selected();
        }
    }

    /// ⌘W: Today tabs close; pinned tabs and favorites only unload.
    pub fn close_tab(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        match self.state.tab(tab_id).map(|tab| tab.pin) {
            Some(Pin::Today) => self.remove_tab(tab_id, cx),
            Some(_) => self.unload_tab(tab_id, cx),
            None => {}
        }
    }

    /// Removes the tab from its space; ⇧⌘T brings it back.
    pub fn remove_tab(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let Some(space_id) = self.state.space_of_tab(tab_id) else {
            return;
        };
        self.finish_slide();
        if self.is_active(tab_id) {
            self.exit_page_fullscreen();
            self.error = None;
        }
        let Some(tab) = self.state.close_tab(space_id, tab_id) else {
            return;
        };
        self.drop_browser(tab_id);
        self.state.remember_closed(space_id, tab);
        if space_id == self.state.selected_space_id {
            self.show_selected();
        }
        self.persist_later(cx);
        cx.notify();
    }

    /// Closes the tab's browser but keeps its entry. The selection moves on,
    /// unless no other tab could take it.
    pub fn unload_tab(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let Some(space) = self
            .state
            .space_of_tab(tab_id)
            .and_then(|id| self.state.space(id))
        else {
            return;
        };
        let (space_id, selected) = (space.id, space.selected_tab_id == Some(tab_id));
        let successor = space.successor_of(tab_id);
        self.finish_slide();
        if self.is_active(tab_id) {
            self.exit_page_fullscreen();
            self.error = None;
        }
        // Hidden spaces keep their selection; it loads again when shown.
        if selected
            && space_id == self.state.selected_space_id
            && let Some(next) = successor
        {
            self.state.select_tab(space_id, next);
        }
        self.drop_browser(tab_id);
        if space_id == self.state.selected_space_id {
            if self.is_active(tab_id) {
                self.engine.borrow_mut().activate_tab(0);
            } else {
                self.show_selected();
            }
        }
        self.persist_later(cx);
        cx.notify();
    }

    /// Reopens a closed tab (the most recent one by default) at the top of
    /// Today in its space.
    pub fn reopen_closed(
        &mut self,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ClosedTab { space_id, tab }) = self.state.take_closed(index) else {
            return;
        };
        if let Some(id) = self.state.new_background_tab(space_id, &tab.url) {
            self.state.update_tab(id, &tab.url, &tab.title);
            self.go_to(space_id, Some(id), window, cx);
        }
    }

    /// ⇧⌘K: closes every Today tab of the active space.
    pub fn clear_today(&mut self, cx: &mut Context<Self>) {
        self.finish_slide();
        let space_id = self.state.selected_space_id;
        let cleared = self.state.clear_today(space_id);
        if cleared.is_empty() {
            return;
        }
        if cleared.iter().any(|tab| self.loaded.contains(&tab.id)) {
            self.exit_page_fullscreen();
        }
        // Pushed bottom first, so ⇧⌘T restores the top tab first.
        for tab in cleared.into_iter().rev() {
            self.drop_browser(tab.id);
            self.state.remember_closed(space_id, tab);
        }
        self.error = None;
        self.show_selected();
        self.persist_later(cx);
        cx.notify();
    }

    pub fn set_pin(&mut self, tab_id: TabId, pin: Pin, cx: &mut Context<Self>) {
        if self.state.set_pin(tab_id, pin) {
            self.persist_later(cx);
            cx.notify();
        }
    }

    /// ⌘D: Today tabs become pinned; pinned tabs and favorites return to Today.
    pub fn toggle_pin(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let pin = match self.state.tab(tab_id).map(|tab| tab.pin) {
            Some(Pin::Today) => Pin::Pinned,
            Some(_) => Pin::Today,
            None => return,
        };
        self.set_pin(tab_id, pin, cx);
    }

    pub fn toggle_favorite(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let pin = match self.state.tab(tab_id).map(|tab| tab.pin) {
            Some(Pin::Favorite) => Pin::Pinned,
            Some(_) => Pin::Favorite,
            None => return,
        };
        self.set_pin(tab_id, pin, cx);
    }

    pub fn duplicate_tab(&mut self, tab_id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(space_id), Some(tab)) = (self.state.space_of_tab(tab_id), self.state.tab(tab_id))
        else {
            return;
        };
        let (url, title) = (tab.url.clone(), tab.title.clone());
        if let Some(id) = self.state.new_background_tab(space_id, &url) {
            self.state.update_tab(id, &url, &title);
            self.go_to(space_id, Some(id), window, cx);
        }
    }

    /// Moves a tab to another space. Each space is a profile of its own, so
    /// its page opens again there, with that space's logins, when shown.
    /// The window stays in the space on screen, as in Arc.
    pub fn move_tab_to_space(&mut self, tab_id: TabId, space_id: SpaceId, cx: &mut Context<Self>) {
        let Some(from) = self.state.space_of_tab(tab_id) else {
            return;
        };
        self.finish_slide();
        if self.is_active(tab_id) {
            self.exit_page_fullscreen();
            self.error = None;
        }
        if self.state.move_tab_to_space(tab_id, space_id).is_none() {
            return;
        }
        self.drop_browser(tab_id);
        self.openers.remove(&tab_id);
        if self.state.selected_space_id == from || self.state.selected_space_id == space_id {
            self.show_selected();
        }
        if let Some(space) = self.state.space(space_id) {
            self.show_toast(format!("Moved to {}", space.name), cx);
        }
        self.persist_later(cx);
        cx.notify();
    }

    pub fn copy_url(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if let Some(tab) = self.state.tab(tab_id) {
            cx.write_to_clipboard(ClipboardItem::new_string(tab.url.clone()));
            self.show_toast("Copied URL", cx);
        }
    }

    pub fn back(&mut self) {
        if let Some(id) = self.active_tab_id() {
            self.engine.borrow().back(id);
        }
    }

    pub fn forward(&mut self) {
        if let Some(id) = self.active_tab_id() {
            self.engine.borrow().forward(id);
        }
    }

    /// Creates an empty space, switches to it and starts naming it.
    pub fn new_space(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.end_rename(window, cx);
        self.finish_slide();
        self.exit_page_fullscreen();
        let from = self.state.active_space().clone();
        let id = self.state.create_space("");
        self.error = None;
        self.begin_slide(from, window);
        self.start_rename(id, window, cx);
        self.persist_later(cx);
        cx.notify();
    }

    /// Closes every browser of the space but keeps its tabs. In the active
    /// space the page on screen stays.
    pub fn unload_space(&mut self, space_id: SpaceId, cx: &mut Context<Self>) {
        let Some(space) = self.state.space(space_id) else {
            return;
        };
        let keep = (space_id == self.state.selected_space_id)
            .then_some(space.selected_tab_id)
            .flatten();
        let tabs: Vec<TabId> = space
            .tabs
            .iter()
            .map(|tab| tab.id)
            .filter(|&id| Some(id) != keep)
            .collect();
        let name = space.name.clone();
        for id in tabs {
            self.drop_browser(id);
        }
        self.show_toast(format!("Unloaded {name}"), cx);
    }

    /// Deletes a space and its tabs; the last space stays. Chromium keeps
    /// the space's profile open until it quits, so its site data goes at
    /// the next launch (`BrowserState::remove_orphaned_profiles`).
    pub fn delete_space(&mut self, space_id: SpaceId, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.spaces.len() < 2 || self.state.space(space_id).is_none() {
            return;
        }
        let active = space_id == self.state.selected_space_id;
        self.end_rename(window, cx);
        self.finish_slide();
        if active {
            self.exit_page_fullscreen();
            self.error = None;
        }
        let Some(space) = self.state.remove_space(space_id) else {
            return;
        };
        for tab in &space.tabs {
            self.drop_browser(tab.id);
        }
        self.engine.borrow_mut().forget_space(space_id);
        self.recent.forget_space(space_id);
        self.tab_switch = None;
        self.menu = None;
        self.close_command_bar(cx);
        if active {
            self.show_selected();
        }
        self.show_toast(format!("Deleted {}", space.name), cx);
        self.persist_later(cx);
    }

    pub fn set_space_theme(&mut self, space_id: SpaceId, theme: u8, cx: &mut Context<Self>) {
        if self.state.set_space_theme(space_id, theme) {
            self.persist_later(cx);
            cx.notify();
        }
    }

    pub fn set_space_icon(
        &mut self,
        space_id: SpaceId,
        icon: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        if self.state.set_space_icon(space_id, icon) {
            self.persist_later(cx);
            cx.notify();
        }
    }

    /// Edits the space's name in place, in its title row. Only the active
    /// space's title row is drawn, and only with the sidebar shown.
    pub fn start_rename(&mut self, space_id: SpaceId, window: &mut Window, cx: &mut Context<Self>) {
        if space_id != self.state.selected_space_id || self.sidebar_width().is_none() {
            return;
        }
        let Some(name) = self.state.space(space_id).map(|space| space.name.clone()) else {
            return;
        };
        let theme = Theme::new(
            self.state.space(space_id).map_or(0, Space::theme),
            self.dark,
        );
        self.renaming = Some(space_id);
        self.engine.borrow().focus_shell();
        self.rename_input.update(cx, |input, cx| {
            input.set_style(
                InputStyle {
                    text: theme.title(),
                    placeholder: theme.tertiary(),
                    cursor: theme.focus(),
                    selection: theme.focus().opacity(0.35),
                    font_size: px(13.5),
                    line_height: px(18.),
                },
                cx,
            );
            input.set_text_and_select_all(&name, window, cx);
        });
        cx.notify();
    }

    fn finish_rename(&mut self, name: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(space_id) = self.renaming.take() else {
            return;
        };
        if let Some(name) = name
            && self.state.rename_space(space_id, name)
        {
            self.persist_later(cx);
        }
        window.focus(&self.focus_handle);
        // A menu opening ended the rename; it keeps keyboard focus.
        if self.menu.is_none() {
            self.focus_page();
        }
        cx.notify();
    }

    /// Ends a rename, keeping the name typed so far. Shortcuts, presses on
    /// the page and sidebar buttons that show another page move keyboard
    /// focus to the page without GPUI noticing, so they end it here.
    pub(crate) fn end_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.renaming.is_some() {
            let name = self.rename_input.read(cx).text().to_owned();
            self.finish_rename(Some(&name), window, cx);
        }
    }

    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.set_sidebar_hidden(!self.state.sidebar_hidden, cx);
    }

    fn set_sidebar_hidden(&mut self, hidden: bool, cx: &mut Context<Self>) {
        if self.state.sidebar_hidden == hidden {
            return;
        }
        self.state.sidebar_hidden = hidden;
        self.menu = None;
        self.engine.borrow().set_traffic_lights_hidden(hidden);
        self.persist_later(cx);
        cx.notify();
    }

    /// Follows the sidebar's edge, dragged with the pointer at `x`. Far
    /// enough left the sidebar hides, keeping its width from before the
    /// drag for when it shows again; coming back shows it at once.
    pub fn drag_sidebar_edge(&mut self, x: f32, start_width: f32, cx: &mut Context<Self>) {
        let width = drag::edge_width(x);
        self.set_sidebar_hidden(width.is_none(), cx);
        let width = width.unwrap_or(start_width);
        if width != self.state.sidebar_width() {
            self.set_sidebar_width(width, cx);
        }
    }

    /// A double click on the sidebar's edge: back to Arc's standard width.
    pub fn reset_sidebar_width(&mut self, cx: &mut Context<Self>) {
        self.set_sidebar_width(model::DEFAULT_SIDEBAR_WIDTH, cx);
    }

    fn set_sidebar_width(&mut self, width: f32, cx: &mut Context<Self>) {
        self.state.set_sidebar_width(width);
        self.persist_later(cx);
        cx.notify();
    }

    /// Two-finger swipes on the sidebar go to the next or previous space.
    pub fn swipe_spaces(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(next) = self.swipe.update(event.touch_phase, event.delta) else {
            return;
        };
        let spaces = &self.state.spaces;
        let index = spaces
            .iter()
            .position(|space| space.id == self.state.selected_space_id)
            .unwrap_or(0);
        let index = if next {
            Some(index + 1)
        } else {
            index.checked_sub(1)
        };
        if let Some(id) = index
            .and_then(|index| spaces.get(index))
            .map(|space| space.id)
        {
            self.switch_space(id, window, cx);
        }
    }

    /// Drops a dragged tab where the sidebar showed it would go. Dropping it
    /// back in its place counts as a click, so a press that wobbled past
    /// the drag threshold still selects the tab.
    pub fn drop_tab(&mut self, tab_id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.tab_drag.take().and_then(|drag| drag.target) else {
            return;
        };
        let place = |state: &BrowserState| {
            let tabs = &state.active_space().tabs;
            let index = tabs.iter().position(|tab| tab.id == tab_id)?;
            Some((index, tabs[index].pin))
        };
        let Some(start) = place(&self.state) else {
            return;
        };
        let before = self
            .state
            .active_space()
            .section(target.pin)
            .nth(target.index)
            .map(|tab| tab.id);
        self.state.move_tab(tab_id, before, target.pin);
        if place(&self.state) == Some(start) {
            self.select_tab(tab_id, window, cx);
        } else {
            self.persist_later(cx);
            cx.notify();
        }
    }

    /// Turns content blocking off (or back on) for `host` and reloads the
    /// active page if it is on that host.
    pub fn toggle_blocking(&mut self, host: &str, cx: &mut Context<Self>) {
        let disabled = !self.state.is_blocking_disabled(host);
        if !self.state.set_blocking_disabled(host, disabled) {
            return;
        }
        self.engine
            .borrow()
            .set_blocking_exceptions(self.state.blocking_exceptions());
        if let Some(tab) = self.state.active_tab()
            && host_of(&tab.url).as_deref() == Some(host)
        {
            let id = tab.id;
            self.reload(id, cx);
        }
        self.show_toast_kind(
            ToastKind::Info,
            if disabled {
                format!("Content blocking off for {host}")
            } else {
                format!("Content blocking on for {host}")
            },
            cx,
        );
        self.persist_later(cx);
    }

    /// A toast saying something was done (✓); `show_toast_kind` for others.
    pub fn show_toast(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.show_toast_kind(ToastKind::Done, text, cx);
    }

    pub fn open_menu(
        &mut self,
        kind: MenuKind,
        anchor: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The window's root takes the keys, so the menu works from the
        // keyboard.
        self.engine.borrow().focus_shell();
        window.focus(&self.focus_handle);
        self.menu = Some(OpenMenu::new(kind, anchor, window.viewport_size().height));
        cx.notify();
    }

    pub fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            self.focus_page();
            cx.notify();
        }
    }

    /// A press on empty sidebar space moves the window; a double click opens
    /// the command bar for a new tab.
    pub fn sidebar_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.click_count >= 2 {
            if self.command_bar.is_none() {
                self.toggle_command_bar(CommandMode::NewTab, window, cx);
            }
            return;
        }
        #[cfg(target_os = "macos")]
        native::begin_window_drag();
        #[cfg(not(target_os = "macos"))]
        window.start_window_move();
    }

    /// ⌘T / ⌘L: opens the command bar in `mode`, or closes it if it is open
    /// in that mode already.
    pub fn toggle_command_bar(
        &mut self,
        mode: CommandMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self.command_bar.as_ref().map(|bar| bar.mode);
        self.close_command_bar(cx);
        if open != Some(mode) && !self.fullscreen {
            self.open_command_bar(mode, window, cx);
        } else {
            window.activate_window();
            self.focus_page();
        }
    }

    fn open_command_bar(&mut self, mode: CommandMode, window: &mut Window, cx: &mut Context<Self>) {
        // The bar taking key status would end it too, but only later.
        self.end_rename(window, cx);
        self.menu = None;
        self.finish_slide();
        let prefill = match mode {
            CommandMode::EditAddress => self
                .state
                .active_tab()
                .map(|tab| tab.url.clone())
                .unwrap_or_default(),
            CommandMode::NewTab => String::new(),
        };
        let context = self.command_context();
        let icons = context
            .tabs
            .iter()
            .filter_map(|tab| host_of(&tab.url))
            .filter_map(|host| Some((host.clone(), self.favicons.get(&host)?)))
            .collect();
        let theme = self.theme();
        let bounds = command_bar::window_bounds(
            self.card_bounds(window.viewport_size()),
            window.bounds().origin,
        );
        let display = window.display(cx).map(|display| display.id());
        let mut bar = None;
        let opened = cx.open_window(
            command_bar::window_options(bounds, display),
            |window, cx| {
                let entity =
                    cx.new(|cx| CommandBar::new(mode, &prefill, context, theme, icons, window, cx));
                bar = Some(entity.clone());
                entity
            },
        );
        let (Ok(handle), Some(bar)) = (opened, bar) else {
            eprintln!("Couldn't open the command bar");
            return;
        };
        #[cfg(target_os = "macos")]
        let native_view = handle
            .update(cx, |_, window, _| native::ns_view(window))
            .ok()
            .flatten();
        #[cfg(target_os = "macos")]
        if let Some(view) = native_view {
            self.engine.borrow().attach_child_window(view, true);
        }
        let subscription = cx.subscribe_in(&bar, window, |this, _, event, window, cx| {
            this.command_bar_event(event, window, cx)
        });
        self.command_bar = Some(CommandBarWindow {
            mode,
            handle,
            #[cfg(target_os = "macos")]
            native_view,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn close_command_bar(&mut self, cx: &mut Context<Self>) {
        let Some(bar) = self.command_bar.take() else {
            return;
        };
        #[cfg(target_os = "macos")]
        if let Some(view) = bar.native_view {
            self.engine.borrow().detach_child_window(view);
        }
        let _ = bar.handle.update(cx, |_, window, _| window.remove_window());
        cx.notify();
    }

    fn command_bar_event(
        &mut self,
        event: &CommandBarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_command_bar(cx);
        match event {
            CommandBarEvent::Run(command) => {
                window.activate_window();
                self.focus_page();
                self.run_command(command.clone(), window, cx);
            }
            CommandBarEvent::Dismiss { refocus } => {
                if *refocus {
                    window.activate_window();
                    self.focus_page();
                }
            }
        }
    }

    fn command_context(&self) -> CommandContext {
        let active_space = self.state.active_space();
        // The active space's tabs first, then the others in space order.
        let spaces = std::iter::once(active_space).chain(
            self.state
                .spaces
                .iter()
                .filter(|space| space.id != active_space.id),
        );
        let tabs = spaces
            .flat_map(|space| {
                space.tabs.iter().map(|tab| TabEntry {
                    id: tab.id,
                    space_id: space.id,
                    space_name: space.name.clone(),
                    title: tab.title.clone(),
                    url: tab.url.clone(),
                })
            })
            .collect();
        let active = self.state.active_tab().map(|tab| {
            let host = host_of(&tab.url);
            ActiveTab {
                id: tab.id,
                url: tab.url.clone(),
                pin: tab.pin,
                blocking_disabled: host
                    .as_deref()
                    .is_some_and(|host| self.state.is_blocking_disabled(host)),
                host,
            }
        });
        CommandContext {
            space_id: active_space.id,
            tabs,
            active,
            spaces: self
                .state
                .spaces
                .iter()
                .map(|space| (space.id, space.name.clone()))
                .collect(),
            can_reopen: !self.state.closed_tabs().is_empty(),
            sidebar_hidden: self.state.sidebar_hidden,
            has_today: active_space.section(Pin::Today).next().is_some(),
        }
    }

    fn run_command(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.active_tab_id();
        match command {
            Command::Open(input) => self.open_url(&input, cx),
            Command::Navigate(input) => self.navigate_active(&input, cx),
            Command::SwitchToTab(id) => self.select_tab(id, window, cx),
            Command::SwitchToSpace(id) => self.switch_space(id, window, cx),
            Command::ToggleSidebar => self.toggle_sidebar(cx),
            Command::ClearToday => self.clear_today(cx),
            Command::NewSpace => self.new_space(window, cx),
            Command::ReopenClosedTab => self.reopen_closed(None, window, cx),
            Command::ToggleBlocking(host) => self.toggle_blocking(&host, cx),
            Command::Reload | Command::TogglePin | Command::ToggleFavorite | Command::CopyUrl => {
                let Some(id) = active else {
                    return;
                };
                match command {
                    Command::Reload => self.reload(id, cx),
                    Command::TogglePin => self.toggle_pin(id, cx),
                    Command::ToggleFavorite => self.toggle_favorite(id, cx),
                    _ => self.copy_url(id, cx),
                }
            }
        }
    }

    pub fn shortcut(&mut self, shortcut: Shortcut, window: &mut Window, cx: &mut Context<Self>) {
        self.end_rename(window, cx);
        let active = self.active_tab_id();
        let tabs = &self.state.active_space().tabs;
        match shortcut {
            Shortcut::CommandBar => self.toggle_command_bar(CommandMode::NewTab, window, cx),
            Shortcut::EditAddress => self.toggle_command_bar(CommandMode::EditAddress, window, cx),
            Shortcut::CloseTab if self.command_bar.is_some() => self.close_command_bar(cx),
            Shortcut::CloseTab => {
                if let Some(id) = active {
                    self.close_tab(id, cx);
                }
            }
            Shortcut::ReopenClosedTab => self.reopen_closed(None, window, cx),
            Shortcut::NextTab => self.walk_recent_tabs(true, window, cx),
            Shortcut::PreviousTab => self.walk_recent_tabs(false, window, cx),
            Shortcut::NextTabInList => {
                if let Some(id) = neighbour_tab(tabs, active, true) {
                    self.select_tab(id, window, cx);
                }
            }
            Shortcut::PreviousTabInList => {
                if let Some(id) = neighbour_tab(tabs, active, false) {
                    self.select_tab(id, window, cx);
                }
            }
            Shortcut::SelectTab(number) => {
                if let Some(id) = tab_for_number(tabs, number) {
                    self.select_tab(id, window, cx);
                }
            }
            Shortcut::SelectSpace(number) => {
                let space = usize::from(number)
                    .checked_sub(1)
                    .and_then(|index| self.state.spaces.get(index));
                if let Some(id) = space.map(|space| space.id) {
                    self.switch_space(id, window, cx);
                }
            }
            Shortcut::PreviousSpace | Shortcut::NextSpace => {
                let spaces = &self.state.spaces;
                let index = spaces
                    .iter()
                    .position(|space| space.id == self.state.selected_space_id)
                    .unwrap_or(0);
                let next = if shortcut == Shortcut::NextSpace {
                    (index + 1) % spaces.len()
                } else {
                    (index + spaces.len() - 1) % spaces.len()
                };
                let id = spaces[next].id;
                self.switch_space(id, window, cx);
            }
            Shortcut::Reload => {
                if let Some(id) = active {
                    self.reload(id, cx);
                }
            }
            Shortcut::Back => self.back(),
            Shortcut::Forward => self.forward(),
            Shortcut::ToggleSidebar => self.toggle_sidebar(cx),
            Shortcut::TogglePin => {
                if let Some(id) = active {
                    self.toggle_pin(id, cx);
                }
            }
            Shortcut::CopyUrl => {
                if let Some(id) = active {
                    self.copy_url(id, cx);
                }
            }
            Shortcut::ClearToday => self.clear_today(cx),
            Shortcut::ZoomIn | Shortcut::ZoomOut | Shortcut::ZoomReset => {
                let step = match shortcut {
                    Shortcut::ZoomIn => ZoomStep::In,
                    Shortcut::ZoomOut => ZoomStep::Out,
                    _ => ZoomStep::Reset,
                };
                let percent = active.and_then(|id| self.engine.borrow().zoom(id, step));
                if let Some(percent) = percent {
                    self.show_toast_kind(ToastKind::Info, format!("Zoom {percent}%"), cx);
                }
            }
            Shortcut::Stop => {
                if let Some(id) = active {
                    self.engine.borrow().stop(id);
                }
            }
            Shortcut::DevTools => {
                if let Some(id) = active {
                    self.engine.borrow().toggle_dev_tools(id);
                }
            }
            Shortcut::Find => self.find_in_page(window, cx),
            Shortcut::FindNext => self.find_again(true, window, cx),
            Shortcut::FindPrevious => self.find_again(false, window, cx),
            Shortcut::Print => {
                if let Some(id) = active {
                    self.engine.borrow().print(id);
                }
            }
        }
    }

    /// ⌃Tab (`back`) and ⌃⇧Tab: a step through the space's recently used
    /// tabs while ⌃ is held. The sidebar outlines the tab reached, and
    /// releasing ⌃ goes there; nothing loads on the way.
    fn walk_recent_tabs(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(switch) = &mut self.tab_switch {
            switch.step(back);
        } else {
            let order = self.recent.order(self.state.active_space());
            let Some(switch) = TabSwitch::start(order, back) else {
                return;
            };
            self.finish_slide();
            self.menu = None;
            self.close_command_bar(cx);
            // GPUI reports ⌃'s release along the focus path, which
            // includes the window's root then.
            window.focus(&self.focus_handle);
            self.tab_switch = Some(switch);
        }
        cx.notify();
    }

    /// ⌃ was released: a ⌃Tab walk goes to its tab.
    pub fn finish_tab_switch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(switch) = self.tab_switch.take() {
            self.select_tab(switch.target(), window, cx);
        }
    }

    /// Page events redraw only when they change something on screen (see
    /// `ui::redraw`): the sidebar's rows, the active tab's toolbar and
    /// progress.
    fn engine_event(&mut self, event: EngineEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            EngineEvent::Created { .. } => {
                self.engine.borrow_mut().register_pending();
                // The active tab's page takes keyboard focus once it is
                // ready; a rename or menu in progress keeps it.
                if self.renaming.is_some() || self.menu.is_some() {
                    self.engine.borrow().focus_shell();
                }
            }
            EngineEvent::Title { tab_id, title } => {
                let Some(tab) = self.state.tab(tab_id) else {
                    return;
                };
                if tab.title != title {
                    let url = tab.url.clone();
                    self.state.update_tab(tab_id, url, title);
                    self.persist_page_later(cx);
                    if self.drawn(tab_id).title {
                        cx.notify();
                    }
                }
            }
            EngineEvent::Address { tab_id, url } => {
                // Only pages that loaded report their address: any load
                // error shown is over (Back, a link on the error page).
                if self.is_active(tab_id) && self.error.take().is_some() {
                    cx.notify();
                }
                let Some(tab) = self.state.tab(tab_id) else {
                    return;
                };
                let (shown, title) = (shown_url(&tab.url), tab.title.clone());
                if self.state.update_tab(tab_id, &url, title) {
                    self.persist_page_later(cx);
                    // Paths, queries and fragments (map pans, scroll spies)
                    // change nothing on screen.
                    if shown_url(&url) != shown && self.is_visible(tab_id) {
                        // Also with the sidebar hidden, ready for showing it.
                        if let Some(host) = host_of(&url) {
                            self.load_favicons(vec![host], cx);
                        }
                        if self.drawn(tab_id).address {
                            cx.notify();
                        }
                    }
                }
            }
            EngineEvent::Loading {
                tab_id,
                loading,
                can_go_back,
                can_go_forward,
            } => {
                if !self.loaded.contains(&tab_id) {
                    return;
                }
                let page = self.pages.entry(tab_id).or_default();
                let changed = (page.loading, page.can_go_back, page.can_go_forward)
                    != (loading, can_go_back, can_go_forward);
                if loading && !page.loading {
                    page.progress = 0.;
                }
                page.loading = loading;
                page.can_go_back = can_go_back;
                page.can_go_forward = can_go_forward;
                if changed && self.drawn(tab_id).status {
                    cx.notify();
                }
            }
            EngineEvent::Progress { tab_id, progress } => {
                if !self.loaded.contains(&tab_id) {
                    return;
                }
                let page = self.pages.entry(tab_id).or_default();
                page.progress = progress;
                // The bar shows only while the page loads.
                if page.loading && self.drawn(tab_id).status {
                    cx.notify();
                }
            }
            EngineEvent::Favicon { host, png, .. } => {
                let shown = self
                    .state
                    .active_space()
                    .tabs
                    .iter()
                    .any(|tab| host_of(&tab.url).as_deref() == Some(&host));
                let changed = self.favicons.store(&host, png);
                self.favicons.release_replaced(cx);
                if changed && shown && self.sidebar_width().is_some() {
                    cx.notify();
                }
            }
            EngineEvent::LoadError {
                tab_id,
                url,
                message,
            } => {
                // Links to apps never load; one that slipped through is
                // neither the tab's address nor an error worth showing.
                if navigation_for(&url) != Navigation::Load {
                    return;
                }
                if let Some(page) = self.pages.get_mut(&tab_id) {
                    page.loading = false;
                }
                self.page_replaced(tab_id, cx);
                // The tab shows the address that failed, as Chrome's address
                // bar does, so ⌘L can fix a typo and Reload tries it again.
                if !url.is_empty()
                    && self
                        .state
                        .tab(tab_id)
                        .is_some_and(|tab| tab.url != model::display_url(&url))
                {
                    self.state.update_tab(tab_id, &url, "");
                    self.persist_page_later(cx);
                    let drawn = self.drawn(tab_id);
                    if drawn.title || drawn.address {
                        cx.notify();
                    }
                }
                if self.is_active(tab_id) {
                    self.error = Some(message);
                    cx.notify();
                }
            }
            EngineEvent::OpenTab {
                opener_tab_id,
                url,
                background,
            } => self.open_from_page(opener_tab_id, &url, background, cx),
            EngineEvent::Fullscreen { tab_id, fullscreen } => {
                self.page_fullscreen(tab_id, fullscreen, window, cx)
            }
            EngineEvent::Closed { tab_id } => {
                self.engine.borrow_mut().forget_closed(tab_id);
                // A browser that went away on its own (the page called
                // window.close()) leaves the tab unloaded.
                if !self.engine.borrow().has_tab(tab_id) && self.loaded.remove(&tab_id) {
                    self.pages.remove(&tab_id);
                    if self.is_active(tab_id) {
                        // Hides the emptied web container and gives keyboard
                        // focus back to the shell; the tab loads again when
                        // selected.
                        self.engine.borrow_mut().activate_tab(tab_id);
                        window.focus(&self.focus_handle);
                        cx.notify();
                    }
                }
            }
            EngineEvent::Shortcut(shortcut) => self.shortcut(shortcut, window, cx),
            EngineEvent::CopyText(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.show_toast("Copied Link", cx);
            }
            // The page takes keyboard focus from the press itself.
            EngineEvent::PagePressed => {
                self.end_rename(window, cx);
                if self.menu.take().is_some() {
                    cx.notify();
                }
            }
            EngineEvent::ControlReleased => self.finish_tab_switch(window, cx),
            EngineEvent::FindResult {
                tab_id,
                count,
                active,
                ..
            } => self.find_result(tab_id, count, active, cx),
            EngineEvent::DownloadStarted {
                id,
                tab_id,
                name,
                path,
                blank_tab,
            } => self.download_started(id, tab_id, name, path, blank_tab, cx),
            EngineEvent::DownloadProgress {
                id,
                received,
                total,
            } => self.download_progress(id, received, total, cx),
            EngineEvent::DownloadFinished { id, ok } => self.download_finished(id, ok, cx),
            EngineEvent::PermissionRequest {
                id,
                tab_id,
                origin,
                permissions,
            } => self.permission_requested(id, tab_id, origin, permissions, window, cx),
            EngineEvent::PermissionDismissed { id } => self.permission_dismissed(id, cx),
            EngineEvent::NewDocument { tab_id } => self.page_replaced(tab_id, cx),
            EngineEvent::Status { tab_id, text } => self.show_link_status(tab_id, text, window, cx),
        }
    }

    /// A page asked for a tab: it opens in the opener's space, behind the
    /// current tab for ⌘-clicks.
    fn open_from_page(
        &mut self,
        opener: TabId,
        url: &str,
        background: bool,
        cx: &mut Context<Self>,
    ) {
        let space_id = self
            .state
            .space_of_tab(opener)
            .unwrap_or(self.state.selected_space_id);
        let shown = space_id == self.state.selected_space_id;
        let id = if background || !shown {
            self.state.new_background_tab(space_id, url)
        } else {
            self.finish_slide();
            self.exit_page_fullscreen();
            self.state.new_tab(space_id, url)
        };
        let Some(id) = id else {
            return;
        };
        let state = &self.state;
        self.openers.retain(|tab, _| state.tab(*tab).is_some());
        self.openers.insert(id, opener);
        self.ensure_loaded(id);
        if shown {
            if !background {
                self.error = None;
                self.show_selected();
            }
            if let Some(host) = host_of(url) {
                self.load_favicons(vec![host], cx);
            }
            cx.notify();
        }
        self.persist_later(cx);
    }

    /// HTML fullscreen (videos, slides) takes the whole window: the window
    /// enters fullscreen and the page fills it without sidebar or insets.
    fn page_fullscreen(
        &mut self,
        tab_id: TabId,
        fullscreen: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if fullscreen && !self.is_active(tab_id) {
            self.engine.borrow().exit_fullscreen(tab_id);
            return;
        }
        self.set_fullscreen(fullscreen, window, cx);
    }

    /// The window left fullscreen on its own (the green button, ⌃⌘F): a page
    /// in HTML fullscreen leaves it too, and the sidebar and insets return
    /// at once rather than when the page confirms.
    fn window_left_fullscreen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.window_fullscreen_for_page = false;
        self.exit_page_fullscreen();
        self.set_fullscreen(false, window, cx);
    }

    fn set_fullscreen(&mut self, fullscreen: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.fullscreen == fullscreen {
            return;
        }
        self.fullscreen = fullscreen;
        self.menu = None;
        self.close_command_bar(cx);
        if fullscreen {
            if !window.is_fullscreen() {
                window.toggle_fullscreen();
                self.window_fullscreen_for_page = true;
            }
        } else if std::mem::take(&mut self.window_fullscreen_for_page) && window.is_fullscreen() {
            window.toggle_fullscreen();
        }
        self.engine.borrow_mut().set_fullscreen_layout(fullscreen);
        cx.notify();
    }

    fn persist_later(&mut self, cx: &mut Context<Self>) {
        self.schedule_save(SAVE_DELAY, cx);
    }

    /// For frequent changes nobody waits for: pages' titles and addresses,
    /// the window's frame while it moves.
    fn persist_page_later(&mut self, cx: &mut Context<Self>) {
        self.schedule_save(PAGE_SAVE_DELAY, cx);
    }

    /// A pending save is never put off, so changes that never pause are
    /// saved too; a sooner one replaces it. The snapshot is taken when the
    /// save runs, so it has every change up to then.
    fn schedule_save(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let due = Instant::now() + delay;
        if self.save_due.is_some_and(|pending| pending <= due) {
            return;
        }
        self.save_due = Some(due);
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let Ok(snapshot) = this.update(cx, |this, _| {
                this.save_due = None;
                this.state.snapshot()
            }) else {
                return;
            };
            let snapshot = match snapshot {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| this.save_finished(Err(error), cx));
                    return;
                }
            };
            // Writing waits on fsync; keep it off the main thread. It runs
            // to the end even if a newer save replaces this task; only its
            // outcome is reported back.
            let written = cx
                .background_executor()
                .spawn(async move { snapshot.write().map(drop) });
            let mut async_cx = cx.clone();
            cx.foreground_executor()
                .spawn(async move {
                    let written = written.await;
                    let _ = this.update(&mut async_cx, |this, cx| this.save_finished(written, cx));
                })
                .detach();
        }));
    }

    /// A failed save puts up a notice that stays until a save succeeds:
    /// otherwise new tabs would be lost at the next launch without a word.
    fn save_finished(&mut self, result: std::io::Result<()>, cx: &mut Context<Self>) {
        let failure = match result {
            Ok(()) => None,
            Err(error) => {
                eprintln!("Couldn't save this session: {error}");
                Some(SharedString::from(save_failure_reason(&error)))
            }
        };
        if self.save_failure != failure {
            self.save_failure = failure;
            cx.notify();
        }
    }

    /// The notice's Try Again: saves at once.
    pub(crate) fn save_again(&mut self, cx: &mut Context<Self>) {
        self.save_due = None;
        self.schedule_save(Duration::ZERO, cx);
    }

    fn save_now(&mut self) {
        self.save_task = None;
        self.save_due = None;
        if let Err(error) = self.state.save() {
            eprintln!("Couldn't save this session: {error}");
        }
    }

    /// Keeps the menu bar's changing items (Show or Hide Sidebar, Pin or
    /// Unpin Tab, the spaces) in step with the window. Called on every
    /// frame; the menu bar is rebuilt only when they change, which is rare.
    pub(crate) fn sync_menu_bar(&mut self, cx: &mut Context<Self>) {
        let sidebar_hidden = self.state.sidebar_hidden;
        let tab_pinned = self
            .state
            .active_tab()
            .is_some_and(|tab| tab.pin != Pin::Today);
        let spaces = self.state.spaces.iter().take(9).map(|space| &space.name);
        let shown = &self.menu_bar;
        if shown.sidebar_hidden == sidebar_hidden
            && shown.tab_pinned == tab_pinned
            && shown.spaces.iter().eq(spaces.clone())
        {
            return;
        }
        let state = crate::MenuBarState {
            sidebar_hidden,
            tab_pinned,
            spaces: spaces.cloned().collect(),
        };
        self.menu_bar = state.clone();
        cx.defer(move |cx| cx.set_menus(crate::menus(&state)));
    }

    /// Map Browser > Set as Default Browser…: macOS asks the user to
    /// confirm, and changes https along with http.
    pub fn set_default_browser(&mut self, cx: &mut Context<Self>) {
        if native::is_default_browser() {
            self.show_toast_kind(
                ToastKind::Info,
                "Map Browser is already your default browser",
                cx,
            );
        } else {
            native::set_as_default_browser();
        }
    }

    /// Files dropped on the sidebar open as tabs, as from Finder.
    pub fn open_dropped(&mut self, paths: &ExternalPaths, cx: &mut Context<Self>) {
        let mut refused = None;
        for path in paths.paths() {
            match opened_file(path) {
                Some(url) => self.open_url(url.as_str(), cx),
                None => refused = refused.or(path.file_name()),
            }
        }
        if let Some(name) = refused {
            let name = name.to_string_lossy();
            self.show_toast_kind(ToastKind::Failed, format!("Can't open “{name}”"), cx);
        }
    }

    /// Says once, after a launch that couldn't read state.json, where the
    /// session came from and where the damaged file was kept.
    fn tell_recovery(&mut self, recovery: Recovery, window: &mut Window, cx: &mut Context<Self>) {
        let folder = self.state.root_dir().display().to_string();
        let kept = recovery.kept.as_ref().map(|kept| {
            let name = kept.file_name().unwrap_or_default().to_string_lossy();
            format!("The damaged file was kept as “{name}” in {folder}.")
        });
        let (message, detail) = if recovery.from_backup {
            (
                "Map Browser restored your tabs from a backup.",
                "Your saved spaces and tabs couldn't be read, so they were opened from the last backup. Changes made since then may be missing.",
            )
        } else {
            (
                "Map Browser couldn't restore your tabs.",
                "Your saved spaces and tabs couldn't be read, and there was no backup to open, so Map Browser started with new spaces.",
            )
        };
        let detail = match &kept {
            Some(kept) => format!("{detail}\n\n{kept}"),
            None => detail.to_owned(),
        };
        let buttons: &[&str] = if kept.is_some() {
            &["OK", "Show in Finder"]
        } else {
            &["OK"]
        };
        let answer = window.prompt(PromptLevel::Warning, message, Some(&detail), buttons, cx);
        let kept = recovery.kept;
        cx.spawn(async move |_, _| {
            if answer.await == Ok(1)
                && let Some(kept) = kept
            {
                native::reveal_in_finder(&kept);
            }
        })
        .detach();
    }
}

/// Why a save failed, in a few words for the notice.
fn save_failure_reason(error: &std::io::Error) -> String {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem => {
            "its folder can't be written to".to_owned()
        }
        ErrorKind::StorageFull | ErrorKind::QuotaExceeded => "the disk is full".to_owned(),
        _ => error.to_string(),
    }
}

/// Runs a browser shortcut in the browser window, wherever focus is.
pub fn run_shortcut(window: WindowHandle<BrowserApp>, shortcut: Shortcut, cx: &mut App) {
    let _ = window.update(cx, |app, window, cx| app.shortcut(shortcut, window, cx));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tabs(ids: &[TabId]) -> Vec<Tab> {
        ids.iter()
            .map(|&id| Tab {
                id,
                url: format!("https://{id}.test/"),
                title: id.to_string(),
                pin: Pin::Today,
            })
            .collect()
    }

    #[test]
    fn number_shortcuts_pick_tabs_in_sidebar_order_and_nine_picks_the_last() {
        let list = tabs(&[10, 11, 12]);
        assert_eq!(tab_for_number(&list, 1), Some(10));
        assert_eq!(tab_for_number(&list, 3), Some(12));
        assert_eq!(tab_for_number(&list, 4), None);
        assert_eq!(tab_for_number(&list, 9), Some(12));
        assert_eq!(tab_for_number(&list, 0), None);
        assert_eq!(tab_for_number(&[], 9), None);
    }

    #[test]
    fn neighbouring_tabs_wrap_around() {
        let list = tabs(&[10, 11, 12]);
        assert_eq!(neighbour_tab(&list, Some(10), true), Some(11));
        assert_eq!(neighbour_tab(&list, Some(12), true), Some(10));
        assert_eq!(neighbour_tab(&list, Some(10), false), Some(12));
        assert_eq!(neighbour_tab(&list, None, true), Some(10));
        assert_eq!(neighbour_tab(&list, None, false), Some(12));
        assert_eq!(neighbour_tab(&[], None, true), None);
        assert_eq!(neighbour_tab(&[], Some(1), false), None);
    }

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
