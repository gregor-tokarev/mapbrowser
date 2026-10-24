//! The browser window's state and behaviour: spaces and tabs, the engine's
//! page events, the command bar and persistence. Rendering lives in `ui`.

use crate::{
    engine::{Engine, EngineEvent, Navigation, Shortcut, ZoomStep, is_insecure, navigation_for},
    favicons::{FaviconStore, host_of},
    history::{self, History},
    input::{InputEvent, InputStyle, TextInput},
    model::{self, BrowserState, Pin, Recovery, SearchEngine, Space, SpaceId, Tab, TabId},
    native,
    recent::{RecentTabs, TabSwitch},
    ui::{
        command_bar::{
            self, ActiveTab, BarInput, ClosedEntry, Command, CommandBar, CommandBarEvent,
            CommandContext, CommandMode, HistoryEntry, TabEntry,
        },
        downloads::Downloads,
        drag::{self, DropZones, TabDrag},
        elide::Cut,
        find_bar::Find,
        link_status::LinkStatus,
        menu::{MenuKind, OpenMenu},
        page_notice::{Leaving, PageNotices},
        permissions::PermissionPrompts,
        redraw::{self, Drawn},
        shortcuts::ShortcutsWindow,
        sidebar::TabLists,
        swipe::SwipeTracker,
        theme::Theme,
        toast_window::{Toast, ToastKind, ToastWindow},
        window_frame::WindowFrame,
    },
};
use gpui::{
    App, AppContext, Bounds, ClipboardItem, Context, Entity, ExternalPaths, FocusHandle, Focusable,
    Image, MouseDownEvent, Pixels, Point, PromptLevel, ScrollWheelEvent, SharedString,
    Subscription, Task, Window, WindowAppearance, WindowHandle, http_client::Url, px,
};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

/// Changes are saved this long after the first one, so a burst writes once.
const SAVE_DELAY: Duration = Duration::from_millis(500);
/// Page titles and addresses can change every second (timers, chat badges,
/// tickers), in every space: those changes alone are saved this rarely.
const PAGE_SAVE_DELAY: Duration = Duration::from_secs(10);
/// Favicons of visited pages the command bar reads from the disk cache
/// when it opens, the most recent first.
const HISTORY_ICONS: usize = 100;
/// The pointer rests this long at the hidden sidebar's edge before the
/// sidebar shows, so a pointer passing by doesn't open it.
const PEEK_DELAY: Duration = Duration::from_millis(150);

/// Navigation state of a loaded tab, for the toolbar and the progress bar,
/// and what its page uses, for its row.
#[derive(Clone, Copy, Debug, Default)]
pub struct PageStatus {
    pub loading: bool,
    pub progress: f64,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    /// Its audio or video elements play sound.
    pub audible: bool,
    /// The user muted the tab.
    pub muted: bool,
    /// It uses the camera or records the screen.
    pub camera: bool,
    pub microphone: bool,
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
    /// The card the window was placed over.
    card: Bounds<Pixels>,
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
    /// Whether a dragged tab is over the space switcher, where its preview
    /// makes way for the space buttons.
    pub tab_over_spaces: Rc<Cell<bool>>,
    /// Measured while drawing the sidebar, for dropping dragged tabs.
    pub drop_zones: Rc<RefCell<DropZones>>,
    swipe: SwipeTracker,
    pub renaming: Option<SpaceId>,
    /// While a new space is being named, the space it was created from:
    /// Esc means it was never wanted, and goes back there.
    new_space_from: Option<SpaceId>,
    pub rename_input: Entity<TextInput>,
    /// The tab whose name is being edited in its row, and the field.
    pub renaming_tab: Option<TabId>,
    pub tab_name_input: Entity<TextInput>,
    /// Each space's tab list and favorites scroll positions.
    pub tab_lists: TabLists,
    /// The hidden sidebar shows while the pointer is over it, after resting
    /// at the window's left edge; `sidebar_hidden` stays as it was.
    pub sidebar_peek: bool,
    peek_task: Option<Task<()>>,
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
    /// Crashes, sign-ins, certificate errors and failed loads the card
    /// shows in place of a page.
    pub notices: PageNotices,
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
    history: History,
    history_save_task: Option<Task<()>>,
    history_save_due: Option<Instant>,
    /// Tabs whose page started a new document: its next address is a visit.
    new_documents: HashSet<TabId>,
    /// Tabs from the last session that haven't loaded yet: loading them
    /// again is no new visit.
    restored: HashSet<TabId>,
    /// Whether the window's close, minimize and zoom buttons are hidden, as
    /// last set: they show whenever the sidebar does.
    traffic_lights_hidden: bool,
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
/// the favicon), marked "Not Secure" for plain HTTP, or, without one, the
/// whole URL. Linux previews print the whole URL on the card.
fn shown_url(url: &str) -> String {
    match host_of(url) {
        Some(host) if cfg!(target_os = "macos") && is_insecure(url) => format!("http://{host}"),
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
        let tab_name_input = cx.new(|cx| TextInput::new(window, cx));
        let notices = PageNotices::new(cx);
        let favicons = FaviconStore::new(state.root_dir());
        let mut history = History::load(state.root_dir());
        // A deleted space's visits go with it, as its profile does, and on
        // the same trust: only the session saved last says it's gone. The
        // next save writes the file.
        if state.from_state_file() {
            history.retain_spaces(&state.spaces.iter().map(|space| space.id).collect());
        }
        let restored = state
            .spaces
            .iter()
            .flat_map(|space| &space.tabs)
            .map(|tab| tab.id)
            .collect();
        let downloads = Downloads::restore(state.downloads());
        let traffic_lights_hidden = state.sidebar_hidden;
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
            tab_over_spaces: Rc::default(),
            drop_zones: Rc::default(),
            swipe: SwipeTracker::default(),
            renaming: None,
            new_space_from: None,
            rename_input: rename_input.clone(),
            renaming_tab: None,
            tab_name_input: tab_name_input.clone(),
            tab_lists: TabLists::default(),
            sidebar_peek: false,
            peek_task: None,
            command_bar: None,
            dark: is_dark(window.appearance()),
            fullscreen: false,
            window_fullscreen_for_page: false,
            window_fullscreen: window.is_fullscreen(),
            focus_handle: cx.focus_handle(),
            save_task: None,
            save_due: None,
            downloads,
            permissions: PermissionPrompts::default(),
            notices,
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
            history,
            history_save_task: None,
            history_save_due: None,
            new_documents: HashSet::new(),
            restored,
            traffic_lights_hidden,
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
            cx.subscribe_in(
                &tab_name_input,
                window,
                |this, _, event, window, cx| match event {
                    InputEvent::Submit(name) => this.finish_tab_rename(Some(name), window, cx),
                    InputEvent::Cancel => this.finish_tab_rename(None, window, cx),
                    InputEvent::Changed(_) => {}
                },
            ),
            cx.on_blur(
                &tab_name_input.focus_handle(cx),
                window,
                |this, window, cx| this.end_tab_rename(window, cx),
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
                if let Some(id) = this.renaming_tab.take() {
                    let name = this.tab_name_input.read(cx).text().to_owned();
                    this.state.rename_tab(id, Some(&name));
                }
                this.save_now();
                this.history_save_task = None;
                this.history.save();
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

    pub(crate) fn ensure_loaded(&mut self, tab_id: TabId) {
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
        self.end_page_notices(tab_id);
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

    /// Gives the active tab's page keyboard focus. A page the card covers
    /// with a notice (`ui::page_notice`) is hidden: the notice takes it, so
    /// Return and Esc answer it again after a menu or bar closes.
    pub(crate) fn focus_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_tab_id() else {
            return;
        };
        if self.notices.covers(id) {
            self.focus_page_notice(window, cx);
        } else {
            self.engine.borrow().focus_tab(id);
        }
    }

    fn finish_slide(&mut self) {
        if self.slide.take().is_some() {
            self.show_selected();
        }
    }

    pub(crate) fn exit_page_fullscreen(&self) {
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
            Some(from) => {
                self.begin_slide(from, window);
                // Without the sidebar only the window's colour would change.
                if self.state.sidebar_hidden && self.sidebar_width().is_none() {
                    let name = self.state.active_space().name.clone();
                    self.show_toast_kind(ToastKind::Info, name, cx);
                }
            }
            None => self.show_selected(),
        }
        // A tab that asks something (a sign-in) takes the keys.
        self.focus_page_notice(window, cx);
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

    /// ⌥⌘→ and a swipe to the left go to the next space, ⌥⌘← and a swipe
    /// to the right to the previous one, wrapping around at the ends. The
    /// slide always runs the way of the key or the fingers.
    fn step_space(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let spaces = &self.state.spaces;
        let count = spaces.len();
        let Some(index) = spaces
            .iter()
            .position(|space| space.id == self.state.selected_space_id)
        else {
            return;
        };
        let next = if forward {
            (index + 1) % count
        } else {
            (index + count - 1) % count
        };
        let id = spaces[next].id;
        self.switch_space(id, window, cx);
        if let Some(slide) = &mut self.slide {
            slide.direction = if forward { 1. } else { -1. };
        }
    }

    /// Puts a space dragged along the switcher in the place of `onto`, drawn
    /// at `index`. Moves the pointer makes before the switcher is drawn
    /// again carry old places, and are ignored.
    pub fn move_space(
        &mut self,
        space_id: SpaceId,
        onto: SpaceId,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        if self.state.spaces.get(index).map(|space| space.id) != Some(onto) {
            return;
        }
        if self.state.move_space(space_id, index) {
            self.persist_later(cx);
            cx.notify();
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
        let previous = self.active_tab_id();
        let Some(tab_id) = self
            .state
            .new_tab(space_id, model::normalize_address(input))
        else {
            return;
        };
        // An address that turns out to be a download closes its tab again
        // and goes back to the tab this one replaced on screen.
        if let Some(previous) = previous {
            let state = &self.state;
            self.openers.retain(|tab, _| state.tab(*tab).is_some());
            self.openers.insert(tab_id, previous);
        }
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
        // A page that stopped responding is ended first, then reloads.
        if !self.reload_notices(tab_id) {
            return;
        }
        if self.loaded.contains(&tab_id) {
            self.engine.borrow().reload(tab_id);
        } else if self.is_active(tab_id) {
            self.show_selected();
        }
    }

    /// ⌘W: Today tabs close; pinned tabs and favorites only unload. A page
    /// with unsaved changes asks first (`leave_tab`).
    pub fn close_tab(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        match self.state.tab(tab_id).map(|tab| tab.pin) {
            Some(Pin::Today) => self.leave_tab(tab_id, Leaving::Close, cx),
            Some(_) => self.leave_tab(tab_id, Leaving::Unload, cx),
            None => {}
        }
    }

    /// Mutes or unmutes the tab's page, from its row or its menu.
    pub fn toggle_mute(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if !self.loaded.contains(&tab_id) {
            return;
        }
        let page = self.pages.entry(tab_id).or_default();
        page.muted = !page.muted;
        self.engine.borrow().set_muted(tab_id, page.muted);
        cx.notify();
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
        let index = self.state.section_index(tab_id);
        let Some(tab) = self.state.close_tab(space_id, tab_id) else {
            return;
        };
        self.drop_browser(tab_id);
        self.release_tab_ui(tab_id);
        self.state.remember_closed_at(space_id, tab, index);
        if space_id == self.state.selected_space_id {
            self.show_selected();
        }
        self.persist_later(cx);
        cx.notify();
    }

    /// A tab that closes or moves takes its menu and its name field along:
    /// left open, they would hang over other rows.
    fn release_tab_ui(&mut self, tab_id: TabId) {
        if self.has_open_menu(tab_id) {
            self.menu = None;
        }
        if self.renaming_tab == Some(tab_id) {
            self.renaming_tab = None;
        }
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
        self.release_tab_ui(tab_id);
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

    /// Reopens a closed tab in its space, in its section and at its place
    /// there, and selects it. By default (⇧⌘T) that is the most recent
    /// close, with every tab closed together with it, in their order; that
    /// undoes the close, so the tab selected then is selected again, and
    /// the toast offering it goes.
    pub fn reopen_closed(
        &mut self,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut closed = match index {
            Some(index) => self.state.take_closed(Some(index)).into_iter().collect(),
            None => self.state.take_last_closed(),
        };
        // Top to bottom: each one's old place counts the ones above it.
        closed.sort_by_key(|closed| closed.index);
        let Some((space_id, batch)) = closed.first().map(|closed| (closed.space_id, closed.batch))
        else {
            return;
        };
        let undo = batch.filter(|_| index.is_none());
        if let Some(batch) = undo {
            self.dismiss_undo_toast(batch, cx);
        }
        let reopened: Vec<(TabId, bool)> = closed
            .into_iter()
            .filter_map(|closed| {
                let selected = closed.selected;
                Some((self.state.reopen_closed(closed)?, selected))
            })
            .collect();
        let Some(&(first, _)) = reopened.first() else {
            // Its space is gone; the list changed all the same.
            return self.persist_later(cx);
        };
        // Undoing Close Other Tabs or Close Tabs Below keeps the tab that
        // stayed; undoing Clear goes back to the tab selected before it.
        let kept = self
            .state
            .space(space_id)
            .and_then(|space| space.selected_tab_id)
            .filter(|id| undo.is_some() && !reopened.iter().any(|(tab, _)| tab == id));
        let select = reopened
            .iter()
            .find(|(_, selected)| *selected)
            .map(|&(tab, _)| tab)
            .or(kept)
            .unwrap_or(first);
        self.go_to(space_id, Some(select), window, cx);
    }

    /// ⇧⌘K: closes every Today tab of the active space.
    pub fn clear_today(&mut self, cx: &mut Context<Self>) {
        self.finish_slide();
        let space_id = self.state.selected_space_id;
        let selected = self.state.active_space().selected_tab_id;
        let cleared = self.state.clear_today(space_id);
        let cleared = cleared
            .into_iter()
            .enumerate()
            .map(|(index, tab)| (tab, index))
            .collect();
        self.closed_together(space_id, cleared, selected, cx);
    }

    /// "Close Other Tabs", or with `below` "Close Tabs Below": Today tabs
    /// close together, as with Clear, and the tab stays, selected if the
    /// selection was among them.
    pub fn close_today_tabs_around(&mut self, tab_id: TabId, below: bool, cx: &mut Context<Self>) {
        let Some(space) = self
            .state
            .space_of_tab(tab_id)
            .and_then(|id| self.state.space(id))
        else {
            return;
        };
        let space_id = space.id;
        let today: Vec<TabId> = space.section(Pin::Today).map(|tab| tab.id).collect();
        let Some(at) = today.iter().position(|&id| id == tab_id) else {
            return;
        };
        let ids = if below {
            today[at + 1..].to_vec()
        } else {
            today.into_iter().filter(|&id| id != tab_id).collect()
        };
        let selected = space.selected_tab_id;
        self.finish_slide();
        let closed = self.state.close_today_tabs(space_id, &ids);
        if selected.is_some_and(|id| ids.contains(&id)) {
            self.state.select_tab_in_space(space_id, tab_id);
        }
        self.closed_together(space_id, closed, selected, cx);
    }

    /// Tabs that closed together, each with its place in Today: their pages
    /// go, and one ⇧⌘T brings them all back, as a toast says, with the
    /// tab that was `selected` before if it was among them.
    fn closed_together(
        &mut self,
        space_id: SpaceId,
        closed: Vec<(Tab, usize)>,
        selected: Option<TabId>,
        cx: &mut Context<Self>,
    ) {
        let Some(batch) = closed.first().map(|(tab, _)| tab.id) else {
            return;
        };
        if closed.iter().any(|(tab, _)| self.loaded.contains(&tab.id)) {
            self.exit_page_fullscreen();
        }
        for (tab, _) in &closed {
            self.drop_browser(tab.id);
            self.release_tab_ui(tab.id);
        }
        let count = closed.len();
        self.state
            .remember_closed_together(space_id, closed, selected);
        if space_id == self.state.selected_space_id {
            self.error = None;
            self.show_selected();
        }
        let toast = if count == 1 {
            "Closed 1 tab · ⇧⌘T reopens it".to_owned()
        } else {
            format!("Closed {count} tabs · ⇧⌘T reopens them")
        };
        self.show_undo_toast(toast, batch, cx);
        self.persist_later(cx);
        cx.notify();
    }

    pub fn set_pin(&mut self, tab_id: TabId, pin: Pin, cx: &mut Context<Self>) {
        if self.state.tab(tab_id).is_some_and(|tab| tab.pin != pin) {
            self.release_tab_ui(tab_id);
        }
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

    /// Opens a copy of the tab right below it, in its section, and selects it.
    pub fn duplicate_tab(&mut self, tab_id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(space_id) = self.state.space_of_tab(tab_id) else {
            return;
        };
        if let Some(id) = self.state.duplicate_tab(tab_id) {
            self.go_to(space_id, Some(id), window, cx);
        }
    }

    /// "Go to Pinned Page": a pinned tab or favorite goes back to the
    /// address it was pinned at, and is selected.
    pub fn go_to_pinned_page(
        &mut self,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(url) = self
            .state
            .tab(tab_id)
            .and_then(|tab| tab.pinned_url.clone())
        else {
            return;
        };
        if self.loaded.contains(&tab_id) {
            // `Address` reports the page once it loads, as for ⌘L.
            self.engine.borrow().navigate(tab_id, &url);
        } else if self.state.update_tab(tab_id, &url, "") {
            // Selecting it loads it there.
            self.persist_later(cx);
        }
        if self.is_active(tab_id) {
            self.error = None;
            cx.notify();
        }
        self.select_tab(tab_id, window, cx);
    }

    /// Edits the tab's name in place, in its row. Only rows of the active
    /// space are drawn, and only with the sidebar shown; favorites show no
    /// name.
    pub fn start_tab_rename(&mut self, tab_id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self
            .state
            .active_space()
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id && tab.pin != Pin::Favorite)
        else {
            return;
        };
        // A hidden sidebar shows until the rename ends (`sidebar_width`),
        // but a page in fullscreen covers it.
        if self.fullscreen {
            return;
        }
        let (name, title) = (tab.shown_title().to_owned(), tab.title.clone());
        let space_id = self.state.selected_space_id;
        // The command bar renames the selected tab, whose row may be
        // scrolled out of sight: it comes into view with its field.
        if self.state.active_space().selected_tab_id == Some(tab_id) {
            self.tab_lists.forget_reveal(space_id);
        }
        self.end_rename(window, cx);
        let theme = self.theme();
        self.renaming_tab = Some(tab_id);
        self.sync_traffic_lights();
        self.engine.borrow().focus_shell();
        self.tab_name_input.update(cx, |input, cx| {
            input.set_style(
                InputStyle {
                    text: theme.title(),
                    placeholder: theme.tertiary(),
                    cursor: theme.focus(),
                    selection: theme.focus().opacity(0.35),
                    font_size: px(14.),
                    line_height: px(18.),
                },
                cx,
            );
            // Emptied, the field shows the page's title, which Return keeps.
            input.set_placeholder(&title, cx);
            input.set_text_and_select_all(&name, window, cx);
        });
        cx.notify();
    }

    fn finish_tab_rename(
        &mut self,
        name: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_id) = self.renaming_tab.take() else {
            return;
        };
        if let Some(name) = name
            && self.state.rename_tab(tab_id, Some(name))
        {
            self.persist_later(cx);
        }
        self.sync_traffic_lights();
        window.focus(&self.focus_handle);
        // A menu opening ended the rename; it keeps keyboard focus.
        if self.menu.is_none() {
            self.focus_page(window, cx);
        }
        cx.notify();
    }

    /// Ends a tab's rename, keeping the name typed so far, like `end_rename`.
    pub(crate) fn end_tab_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.renaming_tab.is_some() {
            let name = self.tab_name_input.read(cx).text().to_owned();
            self.finish_tab_rename(Some(&name), window, cx);
        }
    }

    /// "Reset Name": the tab shows its page's title again.
    pub fn reset_tab_name(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if self.state.rename_tab(tab_id, None) {
            self.persist_later(cx);
            cx.notify();
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
        self.release_tab_ui(tab_id);
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

    /// Creates an empty space, switches to it and starts naming it, in the
    /// sidebar even if it is hidden.
    pub fn new_space(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.end_rename(window, cx);
        self.finish_slide();
        self.exit_page_fullscreen();
        let from = self.state.active_space().clone();
        let id = self.state.create_space("");
        self.error = None;
        self.begin_slide(from.clone(), window);
        self.start_rename(id, window, cx);
        if self.renaming == Some(id) {
            self.new_space_from = Some(from.id);
        }
        self.persist_later(cx);
        cx.notify();
    }

    /// Esc while a new space is named: the space goes again, if nothing was
    /// opened in it, and the sidebar slides back to where it came from.
    fn discard_new_space(
        &mut self,
        space_id: SpaceId,
        back_to: SpaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let unused = self
            .state
            .space(space_id)
            .is_some_and(|space| space.tabs.is_empty());
        if !unused || space_id != self.state.selected_space_id {
            return;
        }
        self.finish_slide();
        let Some(space) = self.state.remove_space(space_id) else {
            return;
        };
        self.engine.borrow_mut().forget_space(space_id);
        self.recent.forget_space(space_id);
        self.state.select_space(back_to);
        self.begin_slide(space, window);
        if let Some(slide) = &mut self.slide {
            slide.direction = -1.;
        }
        self.persist_later(cx);
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
        // The space's history goes with it, and the icons of the sites only
        // it had.
        if self.history.forget_space(space_id) {
            self.schedule_history_save(SAVE_DELAY, cx);
        }
        self.forget_unused_favicons(cx);
    }

    /// Deletes the cached icons of sites that no tab, closed tab or visit
    /// in any space has any more: the cache names every site it holds.
    fn forget_unused_favicons(&mut self, cx: &mut Context<Self>) {
        let tabs = self.state.spaces.iter().flat_map(|space| &space.tabs);
        let closed = self.state.closed_tabs().iter().map(|closed| &closed.tab);
        let keep = tabs
            .chain(closed)
            .map(|tab| tab.url.as_str())
            .chain(self.history.urls())
            .filter_map(host_of)
            .collect();
        self.favicons.retain_hosts(&keep);
        self.favicons.release_replaced(cx);
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
    /// space's title row is drawn; a hidden sidebar shows until the rename
    /// ends (`sidebar_width`), but a page in fullscreen covers it.
    pub fn start_rename(&mut self, space_id: SpaceId, window: &mut Window, cx: &mut Context<Self>) {
        if space_id != self.state.selected_space_id || self.fullscreen {
            return;
        }
        let Some(name) = self.state.space(space_id).map(|space| space.name.clone()) else {
            return;
        };
        let theme = Theme::new(
            self.state.space(space_id).map_or(0, Space::theme),
            self.dark,
        );
        // A tab's name being edited is kept, as `start_tab_rename` keeps a
        // space's: its field gives up the keys before this one takes them.
        self.end_tab_rename(window, cx);
        self.renaming = Some(space_id);
        self.new_space_from = None;
        self.sync_traffic_lights();
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
            input.set_placeholder("Space name", cx);
            input.set_text_and_select_all(&name, window, cx);
        });
        cx.notify();
    }

    fn finish_rename(&mut self, name: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(space_id) = self.renaming.take() else {
            return;
        };
        let new_space_from = self.new_space_from.take();
        if let Some(name) = name
            && self.state.rename_space(space_id, name)
        {
            self.persist_later(cx);
        }
        if name.is_none()
            && let Some(back_to) = new_space_from
        {
            self.discard_new_space(space_id, back_to, window, cx);
        }
        self.sync_traffic_lights();
        window.focus(&self.focus_handle);
        // A menu opening ended the rename; it keeps keyboard focus.
        if self.menu.is_none() {
            self.focus_page(window, cx);
        }
        cx.notify();
    }

    /// On macOS the traffic lights show with the sidebar: also while a
    /// hidden one peeks, shows for a name being edited or holds a
    /// permission card. Called where those change and on every frame; the
    /// engine hears only of changes.
    pub(crate) fn sync_traffic_lights(&mut self) {
        let hidden = self.state.sidebar_hidden && self.sidebar_width().is_none();
        if self.traffic_lights_hidden != hidden {
            self.traffic_lights_hidden = hidden;
            self.engine.borrow().set_traffic_lights_hidden(hidden);
        }
    }

    /// Whether a hidden sidebar shows only because a space or a tab is
    /// being named in it. Ending that hides it again, which is all ⌘S and
    /// the sidebar button do then.
    pub(crate) fn shown_for_naming(&self) -> bool {
        self.state.sidebar_hidden && !self.sidebar_peek && self.naming()
    }

    /// Whether a space's or a tab's name is being edited. Its field keeps
    /// the keys: a page that finishes loading, a permission card or a page
    /// notice takes them only once the name is done.
    pub(crate) fn naming(&self) -> bool {
        self.renaming.is_some() || self.renaming_tab.is_some()
    }

    /// Whether ⌘S, the sidebar button and the command bar's toggle would
    /// show the sidebar for good: it is hidden, or only peeks. One that
    /// shows for a permission card or a name being edited hides again.
    pub(crate) fn sidebar_toggle_shows(&self) -> bool {
        self.state.sidebar_hidden && self.permission_shown().is_none() && !self.shown_for_naming()
    }

    /// Ends a rename, keeping the name typed so far. Shortcuts, presses on
    /// the page and sidebar buttons that show another page move keyboard
    /// focus to the page without GPUI noticing, so they end it here, a
    /// tab's rename too.
    pub(crate) fn end_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.end_tab_rename(window, cx);
        if self.renaming.is_some() {
            let name = self.rename_input.read(cx).text().to_owned();
            self.finish_rename(Some(&name), window, cx);
        }
    }

    /// ⌘S and the sidebar button: hides the sidebar on screen, or shows it.
    /// A permission card holds the sidebar open, also a hidden one: hiding
    /// it then puts the question away too (the site can ask again), so the
    /// sidebar always goes as asked.
    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_width().is_some() && self.permission_shown().is_some() {
            self.dismiss_shown_permissions(cx);
            return self.set_sidebar_hidden(true, cx);
        }
        self.set_sidebar_hidden(!self.state.sidebar_hidden, cx);
    }

    fn set_sidebar_hidden(&mut self, hidden: bool, cx: &mut Context<Self>) {
        if self.state.sidebar_hidden == hidden {
            return;
        }
        self.state.sidebar_hidden = hidden;
        // Showing it while it peeks keeps it.
        self.sidebar_peek = false;
        self.peek_task = None;
        self.menu = None;
        self.sync_traffic_lights();
        self.persist_later(cx);
        cx.notify();
    }

    /// The pointer came to (or left) the hidden sidebar's edge, at the
    /// window's left: after `PEEK_DELAY` there, the sidebar shows until the
    /// pointer leaves it (`update_peek`). Only hover events and a one-off
    /// timer drive it.
    pub fn point_at_sidebar_edge(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if !hovered {
            self.peek_task = None;
            return;
        }
        if !self.state.sidebar_hidden || self.sidebar_peek || self.peek_task.is_some() {
            return;
        }
        self.peek_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PEEK_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                this.peek_task = None;
                if this.state.sidebar_hidden && !this.fullscreen && !this.sidebar_peek {
                    this.sidebar_peek = true;
                    // It shows under the pointer, which only moving would report.
                    this.sidebar_hovered = true;
                    this.sync_traffic_lights();
                    cx.notify();
                }
            });
        }));
    }

    /// Hides a peeking sidebar again, for the frame being drawn.
    pub(crate) fn end_peek(&mut self) {
        if std::mem::take(&mut self.sidebar_peek) {
            self.sidebar_hovered = false;
            self.sync_traffic_lights();
        }
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
        if let Some(next) = self.swipe.update(event.touch_phase, event.delta) {
            self.step_space(next, window, cx);
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
        let lead = if disabled {
            "Ad blocking off for "
        } else {
            "Ad blocking on for "
        };
        self.show_toast_about(ToastKind::Info, lead, host, Cut::Start, cx);
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
        // A hidden sidebar showing for a name or a permission card goes
        // with them, which the menu ends or outlasts: it peeks instead,
        // until the menu closes and the pointer leaves (`peek_ends_at`).
        if self.state.sidebar_hidden && !self.sidebar_peek && !self.fullscreen {
            self.sidebar_peek = true;
            self.sync_traffic_lights();
        }
        if kind == MenuKind::Library {
            self.library_opened();
        }
        self.menu = Some(OpenMenu::new(kind, anchor, window.viewport_size().height));
        cx.notify();
    }

    pub fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            self.focus_page(window, cx);
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

    /// ⌘T / ⌘L / ⌘Y / ⌘,: opens the command bar in `mode`, or closes it if
    /// it is open in that mode already.
    pub fn toggle_command_bar(
        &mut self,
        mode: CommandMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self.command_bar.as_ref().map(|bar| bar.mode);
        self.close_command_bar(cx);
        if open != Some(mode) {
            // A page in fullscreen leaves it first, as in Chrome; the
            // sidebar and insets return at once, so the bar goes over the
            // card, and the page confirms later.
            if self.fullscreen {
                self.exit_page_fullscreen();
                self.set_fullscreen(false, window, cx);
            }
            self.open_command_bar(mode, window, cx);
        } else {
            window.activate_window();
            self.focus_page(window, cx);
        }
    }

    fn open_command_bar(&mut self, mode: CommandMode, window: &mut Window, cx: &mut Context<Self>) {
        let prefill = match mode {
            CommandMode::EditAddress => self
                .state
                .active_tab()
                .map(|tab| tab.url.clone())
                .unwrap_or_default(),
            CommandMode::NewTab | CommandMode::History | CommandMode::Settings => String::new(),
        };
        self.show_command_bar(mode, BarInput::Text(prefill), window, cx);
    }

    /// Opens the bar's window over the card, its field starting with `input`.
    fn show_command_bar(
        &mut self,
        mode: CommandMode,
        input: BarInput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The bar taking key status would end them too, but only later.
        self.end_rename(window, cx);
        self.close_shortcuts(false, window, cx);
        self.menu = None;
        self.finish_slide();
        let context = self.command_context();
        let icons = self.command_bar_icons(&context);
        let theme = self.theme();
        let card = self.card_bounds(window.viewport_size());
        let bounds = command_bar::window_bounds(card, window.bounds().origin);
        let rows = command_bar::visible_rows(card);
        let display = window.display(cx).map(|display| display.id());
        // Visited pages' icons not read yet come from the disk cache.
        let hosts: Vec<String> = context
            .closed
            .iter()
            .map(|closed| closed.url.as_str())
            .chain(
                context
                    .history
                    .iter()
                    .take(HISTORY_ICONS)
                    .map(|page| page.url.as_str()),
            )
            .filter_map(host_of)
            .collect();
        let mut bar = None;
        let opened = cx.open_window(
            command_bar::window_options(bounds, display),
            |window, cx| {
                let entity = cx.new(|cx| {
                    CommandBar::new(mode, input, context, theme, icons, rows, window, cx)
                });
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
            card,
            #[cfg(target_os = "macos")]
            native_view,
            _subscription: subscription,
        });
        let task = self.favicons.load_for(hosts, cx.background_executor());
        cx.spawn(async move |this, cx| {
            let loaded = task.await;
            this.update(cx, |this, cx| {
                let icons = loaded.iter().cloned().collect();
                this.favicons.insert_loaded(loaded);
                if let Some(bar) = &this.command_bar {
                    let _ = bar.handle.update(cx, |bar, _, cx| bar.add_icons(icons, cx));
                }
            })
        })
        .detach();
        cx.notify();
    }

    /// The favicons the bar can show at once: those already read.
    fn command_bar_icons(&self, context: &CommandContext) -> HashMap<String, Arc<Image>> {
        let tabs = context.tabs.iter().map(|tab| tab.url.as_str());
        let closed = context.closed.iter().map(|closed| closed.url.as_str());
        let pages = context.history.iter().map(|page| page.url.as_str());
        tabs.chain(closed)
            .chain(pages)
            .filter_map(host_of)
            .filter_map(|host| Some((host.clone(), self.favicons.get(&host)?)))
            .collect()
    }

    /// Called on every frame of the browser window: a card that moved or
    /// changed size (the sidebar shown or hidden, the window resized) gets
    /// the bar centred over it again, with what was typed. GPUI can move a
    /// window only on some platforms, so the bar's window is replaced.
    pub(crate) fn sync_command_bar(
        &mut self,
        card: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .command_bar
            .as_ref()
            .is_some_and(|bar| bar.card != card)
        {
            cx.defer_in(window, |this, window, cx| {
                this.replace_command_bar(window, cx)
            });
        }
    }

    fn replace_command_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let card = self.card_bounds(window.viewport_size());
        let Some(bar) = self.command_bar.as_ref().filter(|bar| bar.card != card) else {
            return;
        };
        let mode = bar.mode;
        let Ok(input) = bar.handle.read(cx).map(CommandBar::replacement) else {
            return;
        };
        self.close_command_bar(cx);
        if !self.fullscreen {
            self.show_command_bar(mode, input, window, cx);
        }
    }

    /// The open bar's results again, after a shortcut changed what they
    /// offer: ⌘D pinned the tab, ⇧⌘K cleared Today.
    fn refresh_command_bar(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.command_bar.as_ref().map(|bar| bar.handle) else {
            return;
        };
        let context = self.command_context();
        let icons = self.command_bar_icons(&context);
        let _ = handle.update(cx, |bar, window, cx| {
            bar.set_context(context, icons, window, cx)
        });
    }

    pub(crate) fn close_command_bar(&mut self, cx: &mut Context<Self>) {
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
                self.focus_page(window, cx);
                self.run_command(command.clone(), window, cx);
            }
            CommandBarEvent::Dismiss { refocus } => {
                if *refocus {
                    window.activate_window();
                    self.focus_page(window, cx);
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
                    title: tab.shown_title().to_owned(),
                    url: tab.url.clone(),
                })
            })
            .collect();
        let active = self.state.active_tab().map(|tab| {
            let host = host_of(&tab.url);
            let today: Vec<TabId> = active_space.section(Pin::Today).map(|tab| tab.id).collect();
            let at = today.iter().position(|&id| id == tab.id);
            ActiveTab {
                id: tab.id,
                url: tab.url.clone(),
                pin: tab.pin,
                blocking_disabled: host
                    .as_deref()
                    .is_some_and(|host| self.state.is_blocking_disabled(host)),
                host,
                today_others: at.is_some() && today.len() > 1,
                today_below: at.is_some_and(|at| at + 1 < today.len()),
                loaded: self.is_loaded(tab.id),
                muted: self.page(tab.id).muted,
                pinned_away: tab.pin != Pin::Today
                    && tab.pinned_url.as_ref().is_some_and(|url| *url != tab.url),
            }
        });
        let closed = self
            .state
            .closed_tabs()
            .iter()
            .rev()
            .map(|closed| ClosedEntry {
                id: closed.tab.id,
                space_id: closed.space_id,
                title: closed.tab.shown_title().to_owned(),
                url: closed.tab.url.clone(),
            })
            .collect();
        CommandContext {
            space_id: active_space.id,
            tabs,
            recent: self.recent.order(active_space),
            active,
            spaces: self
                .state
                .spaces
                .iter()
                .map(|space| (space.id, space.name.clone()))
                .collect(),
            closed,
            reopen_count: self.state.last_closed_count(),
            history: self
                .history
                .pages()
                .into_iter()
                .map(HistoryEntry::from)
                .collect(),
            now: history::now(),
            sidebar_hidden: self.sidebar_toggle_shows(),
            has_today: active_space.section(Pin::Today).next().is_some(),
            search_engine: self.state.search_engine(),
        }
    }

    fn run_command(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.active_tab_id();
        let space_id = self.state.selected_space_id;
        match command {
            Command::Open(input) => self.open_url(&input, cx),
            Command::Navigate(input) => self.navigate_active(&input, cx),
            Command::OpenInApp(url) => self.engine.borrow().open_in_app(&url),
            Command::SwitchToTab(id) => self.select_tab(id, window, cx),
            Command::SwitchToSpace(id) => self.switch_space(id, window, cx),
            Command::ToggleSidebar => self.toggle_sidebar(cx),
            Command::ClearToday => self.clear_today(cx),
            Command::NewSpace => self.new_space(window, cx),
            Command::ReopenClosedTab => self.reopen_closed(None, window, cx),
            Command::ReopenTab(id) => {
                let closed = self.state.closed_tabs();
                if let Some(index) = closed.iter().position(|closed| closed.tab.id == id) {
                    self.reopen_closed(Some(index), window, cx);
                }
            }
            Command::ToggleBlocking(host) => self.toggle_blocking(&host, cx),
            // The name is edited in the sidebar's title row, which a hidden
            // sidebar shows for that long.
            Command::RenameSpace => self.start_rename(space_id, window, cx),
            Command::UnloadSpace => self.unload_space(space_id, cx),
            Command::Shortcut(shortcut) => self.shortcut(shortcut, window, cx),
            Command::ShowHistory => self.toggle_command_bar(CommandMode::History, window, cx),
            Command::ShowDownloads => self.show_downloads(window, cx),
            Command::ShowSettings => self.toggle_command_bar(CommandMode::Settings, window, cx),
            Command::ClearHistory => self.ask_to_clear_history(window, cx),
            Command::SearchWith(engine) => self.set_search_engine(engine, cx),
            Command::SetDefaultBrowser => self.set_default_browser(cx),
            Command::ShowShortcuts => self.show_shortcuts(window, cx),
            Command::Reload
            | Command::TogglePin
            | Command::ToggleFavorite
            | Command::CopyUrl
            | Command::DuplicateTab
            | Command::RenameTab
            | Command::GoToPinnedPage
            | Command::CloseTab
            | Command::ToggleMute
            | Command::CloseOtherTabs
            | Command::CloseTabsBelow
            | Command::MoveTab(_)
            | Command::SiteSettings
            | Command::ClearSiteData => {
                let Some(id) = active else {
                    return;
                };
                match command {
                    Command::Reload => self.reload(id, cx),
                    Command::TogglePin => self.toggle_pin(id, cx),
                    Command::ToggleFavorite => self.toggle_favorite(id, cx),
                    Command::DuplicateTab => self.duplicate_tab(id, window, cx),
                    // In the tab's row, which a hidden sidebar shows for that long.
                    Command::RenameTab => self.start_tab_rename(id, window, cx),
                    Command::GoToPinnedPage => self.go_to_pinned_page(id, window, cx),
                    Command::ToggleMute => self.toggle_mute(id, cx),
                    Command::CloseOtherTabs => self.close_today_tabs_around(id, false, cx),
                    Command::CloseTabsBelow => self.close_today_tabs_around(id, true, cx),
                    // A page with unsaved changes asks first.
                    Command::CloseTab => self.leave_tab(id, Leaving::Close, cx),
                    Command::MoveTab(space_id) => self.leave_tab(id, Leaving::Move(space_id), cx),
                    // Below the address, which a hidden sidebar shows for
                    // as long as the menu is open.
                    Command::SiteSettings => self.open_site_menu(MenuKind::Site(id), window, cx),
                    Command::ClearSiteData => {
                        self.open_site_menu(MenuKind::ClearSiteData(id), window, cx)
                    }
                    _ => self.copy_url(id, cx),
                }
            }
        }
    }

    /// Asks before forgetting the history: nothing brings it back.
    pub fn ask_to_clear_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.activate_window();
        let answer = window.prompt(
            PromptLevel::Warning,
            "Clear all history?",
            Some(
                "Map Browser forgets the pages you visited, your recently closed tabs and \
                 your list of downloads, in every space. Downloaded files stay.",
            ),
            &["Clear History", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update(cx, |this, cx| this.clear_history(cx));
            }
        })
        .detach();
    }

    /// Forgets every visited page, closed tab and finished download, in
    /// every space.
    fn clear_history(&mut self, cx: &mut Context<Self>) {
        if self.history.clear() {
            self.schedule_history_save(SAVE_DELAY, cx);
        }
        if self.state.clear_closed() {
            self.persist_later(cx);
        }
        self.clear_downloads(cx);
        self.forget_unused_favicons(cx);
        self.show_toast("Cleared history", cx);
        self.refresh_command_bar(cx);
    }

    fn set_search_engine(&mut self, engine: SearchEngine, cx: &mut Context<Self>) {
        if self.state.set_search_engine(engine) {
            self.persist_later(cx);
        }
        // Short: the sidebar's toast is narrow.
        self.show_toast_kind(ToastKind::Info, format!("Using {}", engine.name()), cx);
    }

    /// A tab's page committed a new document at `url`: a visit, in the
    /// history of the tab's space.
    fn record_visit(&mut self, tab_id: TabId, url: &str, cx: &mut Context<Self>) {
        let Some(space_id) = self.state.space_of_tab(tab_id) else {
            return;
        };
        let restored = self.restored.remove(&tab_id);
        if self
            .history
            .visit(tab_id, space_id, url, restored, history::now())
        {
            self.schedule_history_save(PAGE_SAVE_DELAY, cx);
        }
    }

    /// Like `schedule_save`, for the history's own file: never put off by
    /// later changes, and written on a background thread.
    fn schedule_history_save(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let due = Instant::now() + delay;
        if self.history_save_due.is_some_and(|pending| pending <= due) {
            return;
        }
        self.history_save_due = Some(due);
        self.history_save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let Ok(Some(snapshot)) = this.update(cx, |this, _| {
                this.history_save_due = None;
                this.history.snapshot()
            }) else {
                return;
            };
            cx.background_executor()
                .spawn(async move {
                    if let Err(error) = snapshot.write() {
                        eprintln!("Couldn't save the browsing history: {error}");
                    }
                })
                .detach();
        }));
    }

    pub fn shortcut(&mut self, shortcut: Shortcut, window: &mut Window, cx: &mut Context<Self>) {
        let naming_in_hidden_sidebar = self.shown_for_naming();
        self.end_rename(window, cx);
        // A tab's menu lists ⌘W, ⌘D, ⌘R and ⇧⌘C beside its items: while it
        // is open they act on its tab, as a macOS menu's key equivalents
        // do. Any key then closes the menu, which would otherwise stay over
        // a tab that moved or closed.
        let menu_tab = self.menu.as_ref().and_then(|menu| match menu.kind {
            MenuKind::Tab(id) | MenuKind::MoveTab(id) => Some(id),
            _ => None,
        });
        self.close_menu(window, cx);
        // Shortcuts that act on the page, or open something of their own,
        // close the bar and run with the page in focus. The others leave it
        // open, with its results brought up to date.
        let bar_open = self.command_bar.is_some();
        if bar_open
            && matches!(
                shortcut,
                Shortcut::Reload
                    | Shortcut::Stop
                    | Shortcut::Back
                    | Shortcut::Forward
                    | Shortcut::Find
                    | Shortcut::FindNext
                    | Shortcut::FindPrevious
                    | Shortcut::Print
                    | Shortcut::DevTools
            )
        {
            self.close_command_bar(cx);
            window.activate_window();
            self.focus_page(window, cx);
        }
        let active = self.active_tab_id();
        // The tab that ⌘W, ⌘D, ⌘R and ⇧⌘C act on.
        let target = menu_tab.or(active);
        let tabs = &self.state.active_space().tabs;
        match shortcut {
            Shortcut::CommandBar => self.toggle_command_bar(CommandMode::NewTab, window, cx),
            // ⌘L again selects the whole address, as in Chrome.
            Shortcut::EditAddress
                if self
                    .command_bar
                    .as_ref()
                    .is_some_and(|bar| bar.mode == CommandMode::EditAddress) =>
            {
                if let Some(bar) = &self.command_bar {
                    let _ = bar
                        .handle
                        .update(cx, |bar, window, cx| bar.select_all(window, cx));
                }
            }
            Shortcut::EditAddress => self.toggle_command_bar(CommandMode::EditAddress, window, cx),
            Shortcut::CloseTab if self.command_bar.is_some() => self.close_command_bar(cx),
            Shortcut::CloseTab => {
                if let Some(id) = target {
                    self.close_tab(id, cx);
                }
            }
            Shortcut::ReopenClosedTab => self.reopen_closed(None, window, cx),
            Shortcut::NextTab => self.walk_recent_tabs(true, window, cx),
            Shortcut::PreviousTab => self.walk_recent_tabs(false, window, cx),
            // Releasing ⌃ then finds no walk, and the tab on screen stays.
            Shortcut::CancelTabSwitch => {
                if self.tab_switch.take().is_some() {
                    cx.notify();
                }
            }
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
                self.step_space(shortcut == Shortcut::NextSpace, window, cx)
            }
            Shortcut::Reload => {
                if let Some(id) = target {
                    self.reload(id, cx);
                }
            }
            Shortcut::Back => self.back(),
            Shortcut::Forward => self.forward(),
            Shortcut::ToggleSidebar if naming_in_hidden_sidebar => {}
            Shortcut::ToggleSidebar => self.toggle_sidebar(cx),
            Shortcut::TogglePin => {
                if let Some(id) = target {
                    self.toggle_pin(id, cx);
                }
            }
            Shortcut::CopyUrl => {
                if let Some(id) = target {
                    self.copy_url(id, cx);
                }
            }
            Shortcut::ClearToday => self.clear_today(cx),
            Shortcut::ZoomIn => self.zoom_page(ZoomStep::In, cx),
            Shortcut::ZoomOut => self.zoom_page(ZoomStep::Out, cx),
            Shortcut::ZoomReset => self.zoom_page(ZoomStep::Reset, cx),
            Shortcut::Stop => {
                if let Some(id) = active {
                    self.stop_page_questions(id, window, cx);
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
        // ⌘T and ⌘L open, close or keep the bar themselves.
        if bar_open && !matches!(shortcut, Shortcut::CommandBar | Shortcut::EditAddress) {
            self.refresh_command_bar(cx);
        }
    }

    /// Zooms the page on screen a step, as ⌘+, ⌘− and ⌘0 do, and says how
    /// far. The site menu's buttons call it too, and stay open.
    pub(crate) fn zoom_page(&mut self, step: ZoomStep, cx: &mut Context<Self>) {
        let percent = self
            .active_tab_id()
            .and_then(|id| self.engine.borrow().zoom(id, step));
        if let Some(percent) = percent {
            self.show_toast_kind(ToastKind::Info, format!("Zoom {percent}%"), cx);
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
                if self.naming() || self.menu.is_some() {
                    self.engine.borrow().focus_shell();
                }
            }
            EngineEvent::Title { tab_id, title } => {
                // Also when the tab's title stays: its new page has one.
                if self.history.set_title(tab_id, &title, history::now()) {
                    self.schedule_history_save(PAGE_SAVE_DELAY, cx);
                }
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
                // A new document's first address is a visit; addresses the
                // page changes itself (History API, fragments) aren't.
                if self.new_documents.remove(&tab_id) {
                    self.record_visit(tab_id, &url, cx);
                }
                self.page_address_changed(tab_id, &url);
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
                code,
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
                // A failed page is no visit.
                self.new_documents.remove(&tab_id);
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
                let can_go_back = self.page(tab_id).can_go_back;
                self.page_load_failed(tab_id, url, code, message, can_go_back, window, cx);
            }
            EngineEvent::OpenTab {
                opener_tab_id,
                url,
                background,
            } => self.open_from_page(opener_tab_id, &url, background, cx),
            EngineEvent::Fullscreen { tab_id, fullscreen } => {
                self.page_fullscreen(tab_id, fullscreen, window, cx);
                if fullscreen && self.fullscreen && self.is_active(tab_id) {
                    self.show_fullscreen_hint(tab_id, cx);
                }
            }
            EngineEvent::Closed { tab_id } => {
                self.engine.borrow_mut().forget_closed(tab_id);
                if self.engine.borrow().has_tab(tab_id) || !self.loaded.remove(&tab_id) {
                    return;
                }
                self.pages.remove(&tab_id);
                self.new_documents.remove(&tab_id);
                // A page asked to let go did: its tab closes, unloads or
                // moves now.
                if self.page_left(tab_id, cx) {
                    return;
                }
                // A browser that went away on its own (the page called
                // window.close()) leaves the tab unloaded.
                self.end_page_notices(tab_id);
                // Its menu goes with the page, as when ⌘W closes it.
                if self.has_open_menu(tab_id) {
                    self.menu = None;
                    cx.notify();
                }
                if self.is_active(tab_id) {
                    // Hides the emptied web container and gives keyboard
                    // focus back to the shell; the tab loads again when
                    // selected.
                    self.engine.borrow_mut().activate_tab(tab_id);
                    window.focus(&self.focus_handle);
                    cx.notify();
                } else if self.drawn(tab_id).address {
                    // Its row or tile now looks asleep.
                    cx.notify();
                }
            }
            EngineEvent::Shortcut(shortcut) => self.shortcut(shortcut, window, cx),
            EngineEvent::CopyText { text, image } => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                // In the words of the menu item that copied it.
                self.show_toast(
                    if image {
                        "Copied Image Address"
                    } else {
                        "Copied Link Address"
                    },
                    cx,
                );
            }
            EngineEvent::CopyImage(Some(png)) => {
                let image = gpui::Image::from_bytes(gpui::ImageFormat::Png, png);
                cx.write_to_clipboard(ClipboardItem::new_image(&image));
                self.show_toast("Copied Image", cx);
            }
            EngineEvent::CopyImage(None) => {
                self.show_toast_kind(ToastKind::Failed, "Couldn't copy the image", cx)
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
                url,
                blank_tab,
            } => self.download_started(id, tab_id, name, path, url, blank_tab, cx),
            EngineEvent::DownloadProgress {
                id,
                received,
                total,
            } => self.download_progress(id, received, total, cx),
            EngineEvent::DownloadFinished { id, end } => self.download_finished(id, end, cx),
            EngineEvent::PermissionRequest {
                id,
                tab_id,
                origin,
                permissions,
            } => self.permission_requested(id, tab_id, origin, permissions, window, cx),
            EngineEvent::PermissionDismissed { id } => self.permission_dismissed(id, cx),
            EngineEvent::NewDocument { tab_id } => {
                self.new_documents.insert(tab_id);
                self.page_replaced(tab_id, cx);
                self.page_document_changed(tab_id, window, cx);
            }
            EngineEvent::Status { tab_id, text } => self.show_link_status(tab_id, text, window, cx),
            EngineEvent::SignInRequest {
                id,
                tab_id,
                host,
                port,
                realm,
                proxy,
                secure,
                auth_scheme,
            } => self.sign_in_requested(
                id,
                tab_id,
                host,
                port,
                realm,
                proxy,
                secure,
                auth_scheme,
                window,
                cx,
            ),
            EngineEvent::CertificateError {
                id,
                tab_id,
                url,
                code,
            } => self.certificate_error(id, tab_id, url, code, window, cx),
            EngineEvent::LeavePage {
                id,
                tab_id,
                closing,
                reload,
            } => self.leave_requested(id, tab_id, closing, reload, window, cx),
            EngineEvent::RequestDismissed { id } => self.page_request_dismissed(id, window, cx),
            EngineEvent::Crashed { tab_id, reason } => {
                if let Some(page) = self.pages.get_mut(&tab_id) {
                    // The renderer took its loads, sound and camera along.
                    *page = PageStatus {
                        can_go_back: page.can_go_back,
                        can_go_forward: page.can_go_forward,
                        muted: page.muted,
                        ..PageStatus::default()
                    };
                }
                self.page_replaced(tab_id, cx);
                self.page_crashed(tab_id, reason, window, cx);
                if self.drawn(tab_id).address {
                    cx.notify();
                }
            }
            EngineEvent::Unresponsive { tab_id } => self.page_responsive(tab_id, false, window, cx),
            EngineEvent::Responsive { tab_id } => self.page_responsive(tab_id, true, window, cx),
            EngineEvent::PopupBlocked { tab_id, url } => self.popup_blocked(tab_id, url, cx),
            EngineEvent::Audible { tab_id, audible } => {
                let Some(page) = self.pages.get_mut(&tab_id) else {
                    return;
                };
                if std::mem::replace(&mut page.audible, audible) != audible
                    && self.drawn(tab_id).address
                {
                    cx.notify();
                }
            }
            EngineEvent::MediaAccess {
                tab_id,
                video,
                audio,
            } => {
                let Some(page) = self.pages.get_mut(&tab_id) else {
                    return;
                };
                let changed = (page.camera, page.microphone) != (video, audio);
                (page.camera, page.microphone) = (video, audio);
                if changed && self.drawn(tab_id).address {
                    cx.notify();
                }
            }
        }
    }

    /// A page asked for a tab: it opens in the opener's space, behind the
    /// current tab for ⌘-clicks.
    pub(crate) fn open_from_page(
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

    pub(crate) fn persist_later(&mut self, cx: &mut Context<Self>) {
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

    /// Keeps the menu bar's changing items (Show or Hide Sidebar, what ⌘D
    /// and ⌘W do to the tab, the spaces) in step with the window. Called on
    /// every frame; the menu bar is rebuilt only when they change, which is
    /// rare.
    pub(crate) fn sync_menu_bar(&mut self, cx: &mut Context<Self>) {
        // The words the sidebar button and the command bar use.
        let sidebar_hidden = self.sidebar_toggle_shows();
        let pin = self.state.active_tab().map_or(Pin::Today, |tab| tab.pin);
        let spaces = self.state.spaces.iter().take(9).map(|space| &space.name);
        let shown = &self.menu_bar;
        if shown.sidebar_hidden == sidebar_hidden
            && shown.pin == pin
            && shown.spaces.iter().eq(spaces.clone())
        {
            return;
        }
        let state = crate::MenuBarState {
            sidebar_hidden,
            pin,
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
                ..Tab::default()
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
