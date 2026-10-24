//! Chromium runs in native child NSViews. Pages keep their own compositor and
//! process lifetime; GPUI only draws the browser controls.

mod audio;
mod blocking;
mod commands;
mod context_menu;
mod downloads;
mod find;
mod modifiers;
mod page_keys;
mod page_requests;
mod permissions;
mod popups;
mod protocols;
mod site_data;

use super::{
    CrashReason, EditCommand, EngineEvent, PermissionDecision, certificate_continue_allowed,
    edit_command, merge_features, opens_in_tab,
    page::{LoadState, PageLoad, elide_middle, favicon_wait},
};
use crate::blocker::ContentBlocker;
use crate::native::CornerCaps;
use anyhow::{Context, Result, bail, ensure};
use async_channel::{Receiver, Sender};
use block2::RcBlock;
use cef::*;
use objc2::{
    ClassType, MainThreadMarker, MainThreadOnly, msg_send,
    rc::Retained,
    runtime::{AnyObject, Bool, NSObjectProtocol},
    sel,
};
use objc2_app_kit::{
    NSApplication, NSAutoresizingMaskOptions, NSEvent, NSEventMask, NSView, NSWindow,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use page_keys::KeyPress;
use page_requests::Answer;
use std::{
    collections::{HashMap, HashSet},
    ffi::c_void,
    mem::ManuallyDrop,
    path::{Path, PathBuf},
    ptr::NonNull,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

/// Browser-process switches from the energy research. Never add switches
/// that disable background throttling: pages in hidden spaces keep running,
/// throttled the way Chromium throttles hidden tabs.
const ENERGY_SWITCHES: &[&str] = &[
    // No Safe Browsing list updates, translate fetches or upgrade checks.
    "disable-background-networking",
];
const DISABLE_FEATURES: &[&str] = &[
    // Battery Saver must never freeze pages in hidden spaces.
    "CPUMeasurementInFreezingPolicy",
    "MemoryMeasurementInFreezingPolicy",
    // No Cast, DIAL or mDNS discovery; there is no Cast UI.
    "MediaRouter",
    // No periodic network time queries.
    "NetworkTimeServiceQuerying",
    // No per-form server queries; without an API key they fail anyway.
    "AutofillServerCommunication",
];
const ENABLE_FEATURES: &[&str] = &[
    // Aligns wake-ups to 32 ms, only while Battery Saver is active.
    "BatterySaverModeAlignWakeUps",
];
/// Browser-process switches the shell's own page UI needs.
const SHELL_SWITCHES: &[&str] = &[
    // Sign-ins (HTTP authentication) go to RequestHandler::GetAuthCredentials,
    // and the page card asks for them; Chrome's own login dialog has no
    // window to show in beside Alloy pages in native views.
    "disable-chrome-login-prompt",
];

/// Opaque white (0xAARRGGBB), Chromium's default page colour, shown until a
/// page paints so new and reloading views never flash another colour.
const PAGE_BACKGROUND: u32 = 0xFFFF_FFFF;
/// Favicons are drawn at 16 to 20 points: accept icons up to 32 points and
/// render them for 2x displays.
const FAVICON_SIZE: u32 = 32;
const FAVICON_SCALE: f32 = 2.;
/// Hovered links are sent this long at most: data: links can be megabytes,
/// and the status pill shows one line.
const STATUS_MAX_CHARS: usize = 1024;

/// Browser lifecycle state shared with CEF callbacks. A tab can be closed (or
/// unloaded) and created again before Chromium has finished closing its old
/// browser, so each browser has its own instance number.
#[derive(Default)]
struct PendingBrowsers {
    /// The instance representing each tab: requested, ready or live.
    current: HashMap<u64, u64>,
    /// URL each requested instance loads once it exists.
    requested: HashMap<u64, String>,
    ready: Vec<ReadyBrowser>,
    canceled: HashSet<u64>,
    shutting_down: bool,
    live: HashMap<u64, Browser>,
    started_navigation: HashSet<u64>,
    initialized_surface: HashSet<u64>,
    /// Popup and DevTools windows by browser identifier; see `popups`.
    popups: HashMap<i32, Browser>,
    /// Instances the user asked to close (`Engine::close_tab_asking`):
    /// their page may still ask to stay.
    asking: HashSet<u64>,
    /// Instances whose close went ahead: CEF called DoClose.
    closing: HashSet<u64>,
}

impl PendingBrowsers {
    /// Whether every browser has closed, so quitting can go on.
    fn all_closed(&self) -> bool {
        self.live.is_empty() && self.requested.is_empty() && self.popups.is_empty()
    }
}

struct ReadyBrowser {
    tab_id: u64,
    instance: u64,
    browser: Browser,
    url: String,
}

/// What the key monitor needs to route keys into web content.
#[derive(Default)]
struct KeyRouting {
    /// The web container; its window is the browser window.
    container: usize,
    /// The active tab's browser, which receives web editing commands.
    browser: Option<Browser>,
    /// The instance and browser in HTML fullscreen; Esc exits it.
    fullscreen: Option<(u64, Browser)>,
}

/// A favicon is fetched for a page host and an icon URL: the shell files
/// icons by host, and hosts can share one icon URL.
type FaviconKey = (Option<String>, String);

/// Display state of one browser, shared by its display, load and request
/// handlers. Only touched on the UI thread.
#[derive(Default)]
struct PageState {
    /// The latest icon the page named; downloads of any other are stale.
    favicon_requested: Option<FaviconKey>,
    /// The last icon sent, or known to be missing. Failed or cancelled
    /// downloads are not recorded, so the next page asks again.
    favicon_done: Option<FaviconKey>,
    /// The page host and start of the last icon download, which paces the
    /// next one (`favicon_wait`).
    favicon_fetched: Option<(Option<String>, Instant)>,
    /// A paced download is due; it fetches the latest icon by then.
    favicon_scheduled: bool,
    load: PageLoad,
    /// The hovered link's address as last sent; empty for none.
    status: String,
    /// The page's frames that report their sound, and which play it.
    audio: audio::PageAudio,
}

/// Identity and shared state the handlers of one browser report through.
#[derive(Clone)]
struct BrowserLink {
    tab_id: u64,
    instance: u64,
    events: Sender<EngineEvent>,
    pending: Arc<Mutex<PendingBrowsers>>,
    keys: Arc<Mutex<KeyRouting>>,
    live_browsers: Arc<AtomicUsize>,
    downloads: downloads::Downloads,
    permissions: permissions::PermissionRequests,
    requests: page_requests::PageRequests,
}

impl BrowserLink {
    fn send(&self, event: EngineEvent) {
        // Unbounded, so this never blocks; it fails only once the shell has
        // stopped listening while quitting.
        let _ = self.events.try_send(event);
    }

    fn pending(&self) -> MutexGuard<'_, PendingBrowsers> {
        self.pending.lock().expect("pending browser state")
    }

    fn keys(&self) -> MutexGuard<'_, KeyRouting> {
        self.keys.lock().expect("key routing state")
    }

    /// Callbacks from the internal empty document that precedes the first
    /// requested navigation are not reported.
    fn navigation_started(&self) -> bool {
        self.pending().started_navigation.contains(&self.instance)
    }
}

wrap_app! {
    struct BrowserApp;
    impl App {
        fn on_before_command_line_processing(&self, process_type: Option<&CefString>, command_line: Option<&mut CommandLine>) {
            // Browser process only; helpers inherit its feature state.
            if process_type.is_some_and(|kind| !kind.to_string().is_empty()) { return; }
            if let Some(command_line) = command_line.filter(|command_line| command_line.is_read_only() == 0) {
                apply_switches(command_line);
            }
        }
        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(PumpHandler::new())
        }
    }
}

fn apply_switches(command_line: &CommandLine) {
    for switch in ENERGY_SWITCHES.iter().chain(SHELL_SWITCHES) {
        let switch = CefString::from(*switch);
        if command_line.has_switch(Some(&switch)) == 0 {
            command_line.append_switch(Some(&switch));
        }
    }
    // CEF has already written its own --disable-features (GlicActorUi,
    // LensOverlay, ...) and Chromium keeps only a switch's last value.
    for (switch, features) in [
        ("disable-features", DISABLE_FEATURES),
        ("enable-features", ENABLE_FEATURES),
    ] {
        let switch = CefString::from(switch);
        let existing = CefString::from(&command_line.switch_value(Some(&switch))).to_string();
        let merged = merge_features(&existing, features);
        if merged != existing {
            command_line.remove_switch(Some(&switch));
            command_line
                .append_switch_with_value(Some(&switch), Some(&CefString::from(merged.as_str())));
        }
    }
}

wrap_browser_process_handler! {
    struct PumpHandler;
    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            if std::env::var_os("MAPBROWSER_TRACE_CEF").is_some() {
                eprintln!("CEF browser context initialized");
            }
        }
    }
}

/// A tab browser's handlers. The generated constructor takes one argument
/// per handler.
#[allow(clippy::too_many_arguments)]
mod client {
    use cef::*;

    wrap_client! {
        pub(super) struct TabClient {
            display: DisplayHandler,
            load: LoadHandler,
            life: LifeSpanHandler,
            menu: ContextMenuHandler,
            requests: RequestHandler,
            keys: KeyboardHandler,
            find: FindHandler,
            downloads: DownloadHandler,
            permissions: PermissionHandler,
            focus: FocusHandler,
            dialogs: JsdialogHandler,
        }
        impl Client {
            fn display_handler(&self) -> Option<DisplayHandler> {
                Some(self.display.clone())
            }
            fn load_handler(&self) -> Option<LoadHandler> {
                Some(self.load.clone())
            }
            fn life_span_handler(&self) -> Option<LifeSpanHandler> {
                Some(self.life.clone())
            }
            fn context_menu_handler(&self) -> Option<ContextMenuHandler> {
                Some(self.menu.clone())
            }
            fn request_handler(&self) -> Option<RequestHandler> {
                Some(self.requests.clone())
            }
            fn keyboard_handler(&self) -> Option<KeyboardHandler> {
                Some(self.keys.clone())
            }
            fn find_handler(&self) -> Option<FindHandler> {
                Some(self.find.clone())
            }
            fn download_handler(&self) -> Option<DownloadHandler> {
                Some(self.downloads.clone())
            }
            fn permission_handler(&self) -> Option<PermissionHandler> {
                Some(self.permissions.clone())
            }
            fn focus_handler(&self) -> Option<FocusHandler> {
                Some(self.focus.clone())
            }
            fn jsdialog_handler(&self) -> Option<JsdialogHandler> {
                Some(self.dialogs.clone())
            }
        }
    }
}

/// CEF asks for a handler on every callback, so each browser builds its
/// handlers once and hands out references.
fn tab_client(link: BrowserLink) -> Client {
    let page = Arc::<Mutex<PageState>>::default();
    client::TabClient::new(
        TabDisplay::new(link.clone(), page.clone()),
        TabLoad::new(link.clone(), page.clone()),
        TabLife::new(link.clone()),
        context_menu::TabMenu::new(link.clone()),
        TabRequests::new(link.clone(), page, false),
        page_keys::PageKeys::new(link.clone(), false),
        find::TabFind::new(link.clone()),
        downloads::TabDownloads::new(link.clone(), false),
        permissions::TabPermissions::new(link.clone()),
        TabFocus::new(),
        TabDialogs::new(link),
    )
}

wrap_display_handler! {
    struct TabDisplay { link: BrowserLink, page: Arc<Mutex<PageState>> }
    impl DisplayHandler {
        fn on_address_change(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, url: Option<&CefString>) {
            if !self.link.navigation_started() { return; }
            if frame.is_some_and(|frame| frame.is_main() != 0) {
                self.link.send(EngineEvent::Address { tab_id: self.link.tab_id, url: url.map(ToString::to_string).unwrap_or_default() });
            }
        }
        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
            if !self.link.navigation_started() { return; }
            self.link.send(EngineEvent::Title { tab_id: self.link.tab_id, title: title.map(ToString::to_string).unwrap_or_default() });
        }
        fn on_favicon_urlchange(&self, browser: Option<&mut Browser>, icon_urls: Option<&mut CefStringList>) {
            // Take the borrowed list: iterating a clone of it yields nothing.
            let Some(url) = icon_urls.map(std::mem::take).and_then(|urls| urls.into_iter().next()) else { return; };
            let Some(browser) = browser else { return; };
            // The new document has committed, so the main frame has its URL.
            let page_host = browser.main_frame().and_then(|frame| crate::favicons::host_of(&frame_url(Some(&frame))));
            let key = (page_host, url);
            let wait = {
                let mut page = self.page.lock().expect("page state");
                if page.favicon_done.as_ref() == Some(&key) { return; }
                page.favicon_requested = Some(key.clone());
                // The download due takes the latest icon.
                if page.favicon_scheduled { return; }
                let now = Instant::now();
                let last = page.favicon_fetched.as_ref().map(|(host, at)| (host.as_deref(), *at));
                let wait = favicon_wait(last, key.0.as_deref(), now);
                match wait {
                    Some(_) => page.favicon_scheduled = true,
                    None => page.favicon_fetched = Some((key.0.clone(), now)),
                }
                wait
            };
            let Some(wait) = wait else {
                return fetch_favicon(&self.link, &self.page, browser, key);
            };
            let mut task = FaviconDue::new(self.link.clone(), self.page.clone(), browser.clone());
            let delay = i64::try_from(wait.as_millis()).unwrap_or(i64::MAX);
            if post_delayed_task(ThreadId::UI, Some(&mut task), delay) == 0 {
                self.page.lock().expect("page state").favicon_scheduled = false;
            }
        }
        fn on_loading_progress_change(&self, _browser: Option<&mut Browser>, progress: f64) {
            if !self.link.navigation_started() { return; }
            if self.page.lock().expect("page state").load.progress(progress) {
                self.link.send(EngineEvent::Progress { tab_id: self.link.tab_id, progress });
            }
        }
        fn on_status_message(&self, _browser: Option<&mut Browser>, value: Option<&CefString>) {
            if !self.link.navigation_started() { return; }
            let text = elide_middle(&value.map(ToString::to_string).unwrap_or_default(), STATUS_MAX_CHARS);
            {
                let mut page = self.page.lock().expect("page state");
                if page.status == text { return; }
                page.status.clone_from(&text);
            }
            self.link.send(EngineEvent::Status { tab_id: self.link.tab_id, text });
        }
        fn on_fullscreen_mode_change(&self, browser: Option<&mut Browser>, fullscreen: i32) {
            let fullscreen = fullscreen != 0;
            {
                let mut keys = self.link.keys();
                if fullscreen {
                    keys.fullscreen = browser.map(|browser| (self.link.instance, browser.clone()));
                } else if keys.fullscreen.as_ref().is_some_and(|(instance, _)| *instance == self.link.instance) {
                    keys.fullscreen = None;
                }
            }
            self.link.send(EngineEvent::Fullscreen { tab_id: self.link.tab_id, fullscreen });
        }
        fn on_media_access_change(&self, _browser: Option<&mut Browser>, has_video_access: i32, has_audio_access: i32) {
            self.link.send(EngineEvent::MediaAccess { tab_id: self.link.tab_id, video: has_video_access != 0, audio: has_audio_access != 0 });
        }
        fn on_console_message(&self, _browser: Option<&mut Browser>, _level: LogSeverity, message: Option<&CefString>, _source: Option<&CefString>, _line: i32) -> i32 {
            // The audio script's reports; the page's own messages pass.
            let Some((token, audible)) = message.and_then(audio::report) else { return 0; };
            let changed = self.page.lock().expect("page state").audio.report(&token, audible);
            if changed {
                self.link.send(EngineEvent::Audible { tab_id: self.link.tab_id, audible });
            }
            1
        }
    }
}

/// A page's frames stopped playing: its document went, or its renderer.
/// Their scripts' tokens end with them.
fn page_silent(link: &BrowserLink, page: &Mutex<PageState>) {
    let was_audible = page.lock().expect("page state").audio.clear();
    if was_audible {
        link.send(EngineEvent::Audible {
            tab_id: link.tab_id,
            audible: false,
        });
    }
}

/// Watches the media of the document `frame` just loaded (`audio`).
fn watch_audio(link: &BrowserLink, page: &Mutex<PageState>, frame: &Frame) {
    let id = CefString::from(&frame.identifier()).to_string();
    let url = frame_url(Some(frame));
    let (script, went_quiet) = {
        let mut page = page.lock().expect("page state");
        let before = page.audio.is_audible();
        let script = page.audio.loaded(id, &url);
        (script, before && !page.audio.is_audible())
    };
    if went_quiet {
        link.send(EngineEvent::Audible {
            tab_id: link.tab_id,
            audible: false,
        });
    }
    if let Some(script) = script {
        audio::run(frame, &script);
    }
}

fn fetch_favicon(
    link: &BrowserLink,
    page: &Arc<Mutex<PageState>>,
    browser: &Browser,
    key: FaviconKey,
) {
    if let Some(host) = browser.host() {
        let url = CefString::from(key.1.as_str());
        let mut callback = FaviconDownload::new(link.clone(), page.clone(), key);
        host.download_image(Some(&url), 1, FAVICON_SIZE, 0, Some(&mut callback));
    }
}

// A download paced by `favicon_wait`, for the latest icon the page named.
wrap_task! {
    struct FaviconDue { link: BrowserLink, page: Arc<Mutex<PageState>>, browser: Browser }
    impl Task {
        fn execute(&self) {
            let key = {
                let mut page = self.page.lock().expect("page state");
                page.favicon_scheduled = false;
                let Some(key) = page.favicon_requested.clone().filter(|key| page.favicon_done.as_ref() != Some(key)) else { return; };
                page.favicon_fetched = Some((key.0.clone(), Instant::now()));
                key
            };
            if self.browser.is_valid() != 0 {
                fetch_favicon(&self.link, &self.page, &self.browser, key);
            }
        }
    }
}

wrap_download_image_callback! {
    struct FaviconDownload { link: BrowserLink, page: Arc<Mutex<PageState>>, key: FaviconKey }
    impl DownloadImageCallback {
        fn on_download_image_finished(&self, _image_url: Option<&CefString>, http_status_code: i32, image: Option<&mut Image>) {
            let image = image.filter(|image| image.is_empty() == 0).cloned();
            {
                let mut page = self.page.lock().expect("page state");
                // A newer icon supersedes this download.
                if page.favicon_requested.as_ref() != Some(&self.key) { return; }
                // Offline, cancelled or server errors: the next page retries.
                // A missing or undecodable icon is not fetched again.
                if image.is_none() && !matches!(http_status_code, 200..=299 | 404 | 410) { return; }
                page.favicon_done = Some(self.key.clone());
            }
            // Filed under the page it was fetched for, which the tab may
            // have left meanwhile; pages without a host get none. Encoding
            // takes one of CEF's file threads, off the main thread.
            if let (Some(image), Some(host)) = (image, self.key.0.clone()) {
                let mut task = EncodeFavicon::new(self.link.events.clone(), self.link.tab_id, host, image);
                post_task(ThreadId::FILE_USER_VISIBLE, Some(&mut task));
            }
        }
    }
}

// CefImage may be used on any browser process thread.
wrap_task! {
    struct EncodeFavicon { events: Sender<EngineEvent>, tab_id: u64, host: String, image: Image }
    impl Task {
        fn execute(&self) {
            let (mut width, mut height) = (0, 0);
            let png = self
                .image
                .as_png(FAVICON_SCALE, 1, Some(&mut width), Some(&mut height))
                .map(|png| binary_to_vec(&png))
                .filter(|png| !png.is_empty());
            if let Some(png) = png {
                let _ = self.events.try_send(EngineEvent::Favicon { tab_id: self.tab_id, host: self.host.clone(), png });
            }
        }
    }
}

/// `BinaryValue::data` copies `buffer.len()` bytes, so pre-size the buffer.
fn binary_to_vec(value: &BinaryValue) -> Vec<u8> {
    let mut bytes = vec![0; value.size()];
    if !bytes.is_empty() {
        let read = value.data(Some(&mut bytes), 0);
        bytes.truncate(read);
    }
    bytes
}

wrap_load_handler! {
    struct TabLoad { link: BrowserLink, page: Arc<Mutex<PageState>> }
    impl LoadHandler {
        fn on_loading_state_change(&self, _browser: Option<&mut Browser>, is_loading: i32, can_go_back: i32, can_go_forward: i32) {
            // History updates and iframe loads show no loading (`PageLoad`).
            let state = LoadState { loading: is_loading != 0, can_go_back: can_go_back != 0, can_go_forward: can_go_forward != 0 };
            let report = self.page.lock().expect("page state").load.loading_changed(state);
            if let Some(state) = report {
                self.link.send(state.event(self.link.tab_id));
            }
        }
        fn on_load_error(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, error_code: Errorcode, error_text: Option<&CefString>, failed_url: Option<&CefString>) {
            // ABORTED: downloads, 204s and superseded loads, which leave the page as it was.
            if error_code != Errorcode::ABORTED && frame.is_some_and(|frame| frame.is_main() != 0) {
                self.link.send(EngineEvent::LoadError { tab_id: self.link.tab_id, url: failed_url.map(ToString::to_string).unwrap_or_default(), code: error_code.get_raw(), message: error_text.map(ToString::to_string).unwrap_or_else(|| "Page could not be loaded".into()) });
            }
        }
        fn on_load_start(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, _transition_type: TransitionType) {
            if frame.is_some_and(|frame| frame.is_main() != 0) {
                self.link.permissions.drop_page(&self.link);
                self.link.requests.drop_page(&self.link, false);
                page_silent(&self.link, &self.page);
                // CEF calls this as a navigation to another document commits,
                // never for same-document ones, then reports the address.
                if self.link.navigation_started() {
                    self.link.send(EngineEvent::NewDocument { tab_id: self.link.tab_id });
                }
            }
        }
        fn on_load_end(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, http_status_code: i32) {
            // Every frame's media may play sound.
            if let Some(frame) = frame.as_deref() {
                watch_audio(&self.link, &self.page, frame);
            }
            // A web page back from the back-forward cache reports its main
            // frame's load end only, with no HTTP status; OnLoadStart ended
            // its frames' tokens, so each frame's script starts again.
            if http_status_code == 0
                && frame.as_deref().is_some_and(|frame| frame.is_main() != 0 && frame_url(Some(frame)).starts_with("http"))
                && let Some(browser) = browser.as_deref()
            {
                let mut frames = CefStringList::new();
                browser.frame_identifiers(Some(&mut frames));
                for id in frames {
                    if let Some(frame) = browser.frame_by_identifier(Some(&CefString::from(id.as_str()))).filter(|frame| frame.is_main() == 0) {
                        watch_audio(&self.link, &self.page, &frame);
                    }
                }
            }
            if !frame.is_some_and(|frame| frame.is_main() != 0 && CefString::from(&frame.url()).to_string() != "about:blank") { return; }
            if !self.link.pending().initialized_surface.insert(self.link.instance) { return; }
            if let Some(view) = browser.and_then(|browser| browser_view(browser)) {
                if std::env::var_os("MAPBROWSER_TRACE_CEF").is_some() {
                    trace_native_layout(self.link.tab_id, view);
                }
                // CEF 152 can leave its first native compositor surface stale
                // even when this late-created renderer has correct bounds.
                // Bootstrap it once, after the real document exists, through
                // Cocoa's size-change path. Both extents are applied in this
                // callback; no URL reload, timer, or ongoing repaint is used.
                let size = view.frame().size;
                view.setFrameSize(NSSize::new(size.width + 1., size.height));
                view.setFrameSize(size);
                if std::env::var_os("MAPBROWSER_TRACE_CEF").is_some() {
                    eprintln!("CEF initialized native surface tab {} at {}x{}", self.link.tab_id, size.width, size.height);
                }
            }
        }
    }
}

wrap_life_span_handler! {
    struct TabLife { link: BrowserLink }
    impl LifeSpanHandler {
        fn on_after_created(&self, browser: Option<&mut Browser>) {
            let Some(browser) = browser else { return; };
            let link = &self.link;
            link.live_browsers.fetch_add(1, Ordering::SeqCst);
            let mut pending = link.pending();
            pending.live.insert(link.instance, browser.clone());
            let url = pending.requested.remove(&link.instance).unwrap_or_default();
            if pending.shutting_down || pending.canceled.remove(&link.instance) {
                drop(pending);
                if let Some(host) = browser.host() { host.close_browser(1); }
            } else {
                if let Some(view) = browser_view(browser) { view.setHidden(true); }
                pending.ready.push(ReadyBrowser { tab_id: link.tab_id, instance: link.instance, browser: browser.clone(), url });
                drop(pending);
                link.send(EngineEvent::Created { tab_id: link.tab_id });
            }
        }
        fn on_before_popup(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, _popup_id: i32, target_url: Option<&CefString>, _target_frame_name: Option<&CefString>, target_disposition: WindowOpenDisposition, user_gesture: i32, popup_features: Option<&PopupFeatures>, window_info: Option<&mut WindowInfo>, client: Option<&mut Option<Client>>, _settings: Option<&mut BrowserSettings>, _extra_info: Option<&mut Option<DictionaryValue>>, _no_javascript_access: Option<&mut i32>) -> i32 {
            // target=_blank links and window.open: tabs, or popup windows
            // for sign-in. ⌘- and middle-clicks on them ask for a tab behind
            // this one; the same clicks on plain links arrive in
            // TabRequests::on_open_urlfrom_tab instead.
            let url = target_url.map(ToString::to_string).unwrap_or_default();
            i32::from(popups::before_popup(&self.link, frame.as_deref(), url, target_disposition, user_gesture != 0, popup_features, window_info, client))
        }
        fn do_close(&self, browser: Option<&mut Browser>) -> i32 {
            // The close goes ahead: the page let go (`close_tab_asking`).
            self.link.pending().closing.insert(self.link.instance);
            // Closing a tab must destroy its child NSView, never its GPUI window.
            // Defer destruction until CEF has returned from this callback.
            if let Some(host) = browser.and_then(|browser| browser.host()) {
                let raw = host.window_handle().cast::<NSView>();
                if !raw.is_null() {
                    // SAFETY: A valid browser's window handle is its live host
                    // NSView, and this callback runs on the main thread.
                    let retained = unsafe { Retained::retain(raw) }.expect("CEF view");
                    let address = Retained::into_raw(retained) as usize;
                    let mut task = CloseViewTask::new(address);
                    if post_task(ThreadId::UI, Some(&mut task)) == 0 {
                        // Scheduling only fails during teardown. Release the
                        // extra retain so view hierarchy destruction can finish.
                        // SAFETY: `address` came from `Retained::into_raw` above
                        // and the task that would have consumed it never runs.
                        unsafe { drop(Retained::from_raw(address as *mut NSView)); }
                    }
                }
            }
            1
        }
        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            let link = &self.link;
            link.live_browsers.fetch_sub(1, Ordering::SeqCst);
            link.permissions.drop_page(link);
            link.requests.drop_page(link, true);
            link.requests.set_unresponsive(link.instance, None);
            let (ready_to_quit, report) = {
                let mut pending = link.pending();
                pending.live.remove(&link.instance);
                pending.started_navigation.remove(&link.instance);
                pending.initialized_surface.remove(&link.instance);
                pending.asking.remove(&link.instance);
                pending.closing.remove(&link.instance);
                // A newer browser already represents this tab when it was
                // closed and created again before this one finished closing.
                let superseded = pending.current.get(&link.tab_id).is_some_and(|&current| current != link.instance);
                if !superseded {
                    pending.current.remove(&link.tab_id);
                }
                (pending.shutting_down && pending.all_closed(), !superseded)
            };
            let was_fullscreen = {
                let mut keys = link.keys();
                let was_fullscreen = keys.fullscreen.as_ref().is_some_and(|(instance, _)| *instance == link.instance);
                if was_fullscreen { keys.fullscreen = None; }
                was_fullscreen
            };
            if was_fullscreen {
                // Also when a newer browser represents the tab: keys.fullscreen
                // still named this one, so the shell last heard about
                // fullscreen from it, and nothing else will end it.
                link.send(EngineEvent::Fullscreen { tab_id: link.tab_id, fullscreen: false });
            }
            if report {
                link.send(EngineEvent::Closed { tab_id: link.tab_id });
            }
            if ready_to_quit { quit_message_loop(); }
        }
    }
}

fn frame_url(frame: Option<&Frame>) -> String {
    frame
        .map(|frame| CefString::from(&frame.url()).to_string())
        .unwrap_or_default()
}

/// Asks the shell for a tab showing `url` if a tab can show it; `opener` is
/// the frame that asked. See `opens_in_tab`. Mail and phone links open their
/// app instead; callers only get here after a click.
fn request_tab(link: &BrowserLink, opener: Option<&Frame>, url: String, background: bool) {
    if opens_in_tab(&url, &frame_url(opener)) {
        link.send(EngineEvent::OpenTab {
            opener_tab_id: link.tab_id,
            url,
            background,
        });
    } else {
        protocols::open_in_app(&url);
    }
}

// `popup`: a popup window's. Its sign-ins show on its opener tab's card;
// certificate errors keep Chromium's own interstitial in the popup, and
// crashes and hangs Chromium's defaults, as the tab's card isn't the
// popup's.
wrap_request_handler! {
    struct TabRequests { link: BrowserLink, page: Arc<Mutex<PageState>>, popup: bool }
    impl RequestHandler {
        fn on_open_urlfrom_tab(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, target_url: Option<&CefString>, target_disposition: WindowOpenDisposition, user_gesture: i32) -> i32 {
            // ⌘-, ⇧- and middle-clicks on links without a target arrive here,
            // not in on_before_popup. Alloy would load them in this tab.
            let background = target_disposition == WindowOpenDisposition::NEW_BACKGROUND_TAB;
            let new_tab = background
                || [WindowOpenDisposition::NEW_FOREGROUND_TAB, WindowOpenDisposition::NEW_WINDOW, WindowOpenDisposition::NEW_POPUP].contains(&target_disposition);
            if !new_tab { return 0; }
            // Never navigate this tab instead. Like popups, requests without
            // a user gesture (clicks dispatched by scripts) are dropped.
            let url = target_url.map(ToString::to_string).unwrap_or_default();
            if user_gesture != 0 {
                request_tab(&self.link, frame.as_deref(), url, background);
            } else {
                self.link.send(EngineEvent::PopupBlocked { tab_id: self.link.tab_id, url: popups::blocked_url(url, frame.as_deref()) });
            }
            1
        }
        fn on_before_browse(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, request: Option<&mut Request>, user_gesture: i32, _is_redirect: i32) -> i32 {
            let url = request.map(|request| CefString::from(&request.url()).to_string()).unwrap_or_default();
            if protocols::handled_outside(&url, user_gesture != 0) { return 1; }
            if frame.is_some_and(|frame| frame.is_main() != 0) {
                // A new navigation: the last one's sign-in or certificate
                // question is moot.
                self.link.requests.drop_page(&self.link, false);
                // Another document: it shows as loading, and the old one's
                // hovered link is gone.
                let (report, hovered) = {
                    let mut page = self.page.lock().expect("page state");
                    (page.load.main_frame_navigation(), !std::mem::take(&mut page.status).is_empty())
                };
                if let Some(state) = report {
                    self.link.send(state.event(self.link.tab_id));
                }
                if hovered {
                    self.link.send(EngineEvent::Status { tab_id: self.link.tab_id, text: String::new() });
                }
            }
            0
        }
        fn auth_credentials(&self, _browser: Option<&mut Browser>, origin_url: Option<&CefString>, is_proxy: i32, host: Option<&CefString>, port: i32, realm: Option<&CefString>, scheme: Option<&CefString>, callback: Option<&mut AuthCallback>) -> i32 {
            // On the IO thread: only the shared requests and the event
            // channel are used here. The card asks; Chromium keeps the
            // answer for the rest of the session.
            let Some(callback) = callback else { return 0; };
            let origin = origin_url.map(ToString::to_string).unwrap_or_default();
            let id = self.link.requests.add(&self.link, Answer::SignIn(callback.clone()));
            self.link.send(EngineEvent::SignInRequest {
                id,
                tab_id: self.link.tab_id,
                host: host.map(ToString::to_string).unwrap_or_default(),
                port: u16::try_from(port).unwrap_or(0),
                realm: realm.map(ToString::to_string).unwrap_or_default(),
                proxy: is_proxy != 0,
                secure: origin.starts_with("https:"),
                auth_scheme: scheme.map(ToString::to_string).unwrap_or_default().to_ascii_lowercase(),
            });
            1
        }
        fn on_certificate_error(&self, _browser: Option<&mut Browser>, cert_error: Errorcode, request_url: Option<&CefString>, _ssl_info: Option<&mut Sslinfo>, callback: Option<&mut Callback>) -> i32 {
            // Main-frame loads only; Chromium refuses subresources itself.
            let Some(callback) = callback.filter(|_| !self.popup) else { return 0; };
            let url = request_url.map(ToString::to_string).unwrap_or_default();
            let code = cert_error.get_raw();
            // CEF doesn't say whether HSTS or pinning makes the error fatal
            // for this host; only hosts that can have neither may go on.
            let may_continue = certificate_continue_allowed(code, &url);
            let id = self.link.requests.add(&self.link, Answer::Certificate { callback: callback.clone(), may_continue });
            self.link.send(EngineEvent::CertificateError { id, tab_id: self.link.tab_id, url, code });
            1
        }
        fn on_render_process_terminated(&self, _browser: Option<&mut Browser>, status: TerminationStatus, _error_code: i32, _error_string: Option<&CefString>) {
            // A browser closing or replaced for its tab has no card to show.
            if self.popup || self.link.pending().current.get(&self.link.tab_id) != Some(&self.link.instance) { return; }
            self.link.permissions.drop_page(&self.link);
            self.link.requests.drop_page(&self.link, false);
            self.link.requests.set_unresponsive(self.link.instance, None);
            page_silent(&self.link, &self.page);
            let hovered = !std::mem::take(&mut self.page.lock().expect("page state").status).is_empty();
            if hovered {
                self.link.send(EngineEvent::Status { tab_id: self.link.tab_id, text: String::new() });
            }
            let reason = if status == TerminationStatus::PROCESS_OOM { CrashReason::OutOfMemory } else { CrashReason::Crashed };
            self.link.send(EngineEvent::Crashed { tab_id: self.link.tab_id, reason });
        }
        fn on_render_process_unresponsive(&self, _browser: Option<&mut Browser>, callback: Option<&mut UnresponsiveProcessCallback>) -> i32 {
            // Alloy's default is to wait silently; the card offers Wait or Reload.
            let Some(callback) = callback.filter(|_| !self.popup) else { return 0; };
            self.link.requests.set_unresponsive(self.link.instance, Some(callback.clone()));
            self.link.send(EngineEvent::Unresponsive { tab_id: self.link.tab_id });
            1
        }
        fn on_render_process_responsive(&self, _browser: Option<&mut Browser>) {
            if self.link.requests.set_unresponsive(self.link.instance, None) {
                self.link.send(EngineEvent::Responsive { tab_id: self.link.tab_id });
            }
        }
    }
}

wrap_focus_handler! {
    struct TabFocus;
    impl FocusHandler {
        fn on_set_focus(&self, browser: Option<&mut Browser>, source: FocusSource) -> i32 {
            // Loads ask for keyboard focus for their page. A page that isn't
            // on screen (opened behind the current tab, in another space, or
            // hidden behind the card's notice) must not take it from the page
            // or field being typed in; selecting its tab focuses it.
            let hidden = browser
                .and_then(|browser| browser_view(browser))
                .is_none_or(|view| view.isHiddenOrHasHiddenAncestor());
            i32::from(source == FocusSource::NAVIGATION && hidden)
        }
    }
}

wrap_jsdialog_handler! {
    struct TabDialogs { link: BrowserLink }
    impl JsdialogHandler {
        fn on_before_unload_dialog(&self, _browser: Option<&mut Browser>, _message_text: Option<&CefString>, is_reload: i32, callback: Option<&mut JsdialogCallback>) -> i32 {
            let Some(callback) = callback else { return 0; };
            let (ask, closing) = {
                let pending = self.link.pending();
                let current = pending.current.get(&self.link.tab_id) == Some(&self.link.instance);
                // Once the page was let go (DoClose ran), it isn't asked again.
                let closing = pending.closing.contains(&self.link.instance);
                (current && !closing && !pending.shutting_down, pending.asking.contains(&self.link.instance))
            };
            if !ask {
                // Closing anyway (Clear, Unload Space, quitting): the page
                // goes without a question nobody could answer in time.
                callback.cont(1, None);
                return 1;
            }
            let id = self.link.requests.add(&self.link, Answer::Leave(callback.clone()));
            self.link.send(EngineEvent::LeavePage { id, tab_id: self.link.tab_id, closing, reload: is_reload != 0 });
            1
        }
        fn on_reset_dialog_state(&self, _browser: Option<&mut Browser>) {
            self.link.requests.drop_leave(&self.link);
        }
    }
}

wrap_task! {
    struct CloseViewTask { view_address: usize }
    impl Task {
        fn execute(&self) {
            // SAFETY: `do_close` produced the address with `Retained::into_raw`
            // and posts exactly one task per address, so it is released once.
            let view = unsafe { Retained::from_raw(self.view_address as *mut NSView) }.expect("retained CEF view");
            view.removeFromSuperview();
        }
    }
}

pub struct Engine {
    library: Option<ManuallyDrop<library_loader::LibraryLoader>>,
    contexts: HashMap<u64, RequestContext>,
    browsers: HashMap<u64, Browser>,
    pending: Arc<Mutex<PendingBrowsers>>,
    container: Option<Retained<NSView>>,
    caps: Option<CornerCaps>,
    parent: *mut NSView,
    monitors: Vec<Retained<AnyObject>>,
    keys: Arc<Mutex<KeyRouting>>,
    root_cache: PathBuf,
    events: Sender<EngineEvent>,
    live_browsers: Arc<AtomicUsize>,
    next_instance: u64,
    active_tab: Option<u64>,
    fullscreen_layout: bool,
    width: f64,
    height: f64,
    blocker: ContentBlocker,
    downloads: downloads::Downloads,
    permissions: permissions::PermissionRequests,
    requests: page_requests::PageRequests,
    /// Tabs whose page the card covers with a notice (`set_page_covered`).
    covered: HashSet<u64>,
}

impl Engine {
    pub fn initialize(data_dir: &Path) -> Result<(Self, Receiver<EngineEvent>)> {
        ensure!(
            MainThreadMarker::new().is_some(),
            "Chromium must start on the main thread"
        );
        let exe = std::env::current_exe()?;
        let framework = exe
            .parent()
            .context("executable path")?
            .join("../Frameworks/Chromium Embedded Framework.framework");
        ensure!(
            framework.exists(),
            "Run the bundled application. Build it with python3 scripts/bundle.py first."
        );
        let library = library_loader::LibraryLoader::new(&exe, false);
        ensure!(library.load(), "Could not load the Chromium framework");
        let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
        crate::native::install_cef_application_protocol()?;
        let root_cache = data_dir.join("profiles");
        std::fs::create_dir_all(&root_cache)?;
        let root_cache = root_cache.canonicalize()?;
        let args = args::Args::new();
        let mut app = BrowserApp::new();
        let debug_port = match std::env::var("MAPBROWSER_DEBUG_PORT") {
            Ok(value) => {
                let port: u16 = value
                    .parse()
                    .context("MAPBROWSER_DEBUG_PORT must be a port number")?;
                ensure!(
                    port >= 1024,
                    "MAPBROWSER_DEBUG_PORT must be between 1024 and 65535"
                );
                i32::from(port)
            }
            Err(std::env::VarError::NotPresent) => 0,
            Err(error) => return Err(error.into()),
        };
        let settings = Settings {
            no_sandbox: 0,
            // Chromium's native AppKit pump schedules its own deadlines. GPUI
            // shares that NSApplication loop through the native bridge.
            external_message_pump: 0,
            root_cache_path: CefString::from(root_cache.to_string_lossy().as_ref()),
            cache_path: CefString::from(root_cache.join("default").to_string_lossy().as_ref()),
            persist_session_cookies: 1,
            log_file: CefString::from(data_dir.join("chromium.log").to_string_lossy().as_ref()),
            log_severity: LogSeverity::WARNING,
            remote_debugging_port: debug_port,
            ..Default::default()
        };
        let initialized = initialize(
            Some(args.as_main_args()),
            Some(&settings),
            Some(&mut app),
            std::ptr::null_mut(),
        );
        if initialized != 1 {
            crate::native::disable_cef_message_loop();
            bail!("Chromium initialization failed");
        }
        crate::native::enable_cef_message_loop();
        let (events, receiver) = async_channel::unbounded();
        let keys = Arc::new(Mutex::new(KeyRouting::default()));
        let monitors = [
            install_key_monitor(events.clone(), keys.clone()),
            install_mouse_monitor(events.clone(), keys.clone()),
            modifiers::install_control_monitor(events.clone()),
        ]
        .into_iter()
        .flatten()
        .collect();
        let pending = Arc::new(Mutex::new(PendingBrowsers::default()));
        let quit_pending = pending.clone();
        let downloads = downloads::Downloads::default();
        let quit_downloads = downloads.clone();
        let requests = page_requests::PageRequests::default();
        let quit_requests = requests.clone();
        crate::native::set_quit_handler(move || {
            quit_downloads.cancel_all();
            let (browsers, can_quit) = {
                let mut pending = quit_pending.lock().expect("pending browser state");
                pending.shutting_down = true;
                // Popup and DevTools windows close with the tabs.
                let browsers = pending.live.values().chain(pending.popups.values());
                (browsers.cloned().collect::<Vec<_>>(), pending.all_closed())
            };
            // Pages asking "Leave site?" go: quitting never waits on them.
            quit_requests.leave_all();
            if can_quit {
                quit_message_loop();
                return;
            }
            // Keep the native loop running until OnBeforeClose acknowledges
            // every browser. Closing after NSApp.run returns is too late.
            for browser in browsers {
                if browser.is_valid() != 0
                    && let Some(host) = browser.host()
                {
                    host.close_browser(1);
                }
            }
        });
        Ok((
            Self {
                library: Some(ManuallyDrop::new(library)),
                contexts: HashMap::new(),
                browsers: HashMap::new(),
                pending,
                container: None,
                caps: None,
                parent: std::ptr::null_mut(),
                monitors,
                keys,
                root_cache,
                events,
                live_browsers: Arc::new(AtomicUsize::new(0)),
                next_instance: 0,
                active_tab: None,
                fullscreen_layout: false,
                width: 800.,
                height: 600.,
                blocker: ContentBlocker::default(),
                downloads,
                permissions: permissions::PermissionRequests::default(),
                requests,
                covered: HashSet::new(),
            },
            receiver,
        ))
    }

    /// `parent` is the NSView in GPUI's AppKit raw window handle.
    pub fn attach(&mut self, parent: *mut c_void) -> Result<()> {
        ensure!(!parent.is_null(), "Missing native parent view");
        if self.container.is_some() {
            return Ok(());
        }
        let mtm = MainThreadMarker::new().context("attach must run on the main thread")?;
        self.parent = parent.cast();
        let parent = self.parent_view().context("Missing native parent view")?;
        let container =
            NSView::initWithFrame(NSView::alloc(mtm), rect(0., 0., self.width, self.height));
        container.setWantsLayer(true);
        // Pages slide out of the card while spaces switch; they must never
        // draw over the sidebar. AppKit stopped clipping subviews by default
        // for apps built with the macOS 14 SDK (older systems still clip and
        // lack the property). A plain rectangle, unlike a rounded mask, needs
        // no offscreen pass; the corner caps do the rounding.
        if container.respondsToSelector(sel!(setClipsToBounds:)) {
            container.setClipsToBounds(true);
        }
        parent.addSubview(&container);
        container.setHidden(true);
        // Added after the container, so they draw above every web view.
        let caps = CornerCaps::new(parent, mtm);
        self.keys.lock().expect("key routing state").container =
            Retained::as_ptr(&container) as usize;
        self.container = Some(container);
        self.caps = Some(caps);
        Ok(())
    }

    /// Creates the tab's browser, or navigates the one it already has. Works
    /// for any tab at any time, including one closed or unloaded moments ago.
    pub fn create_tab(&mut self, tab_id: u64, workspace_id: u64, url: &str) -> Result<()> {
        if std::env::var_os("MAPBROWSER_TRACE_CEF").is_some() {
            eprintln!(
                "CEF create tab {tab_id}, space {workspace_id}, on_ui={}",
                currently_on(ThreadId::UI)
            );
        }
        if self.has_tab(tab_id) {
            self.navigate(tab_id, url);
            return Ok(());
        }
        let container = self
            .container
            .as_ref()
            .context("Attach the native view before creating a tab")?;
        let window_info = WindowInfo {
            runtime_style: RuntimeStyle::ALLOY,
            ..Default::default()
        }
        .set_as_child(
            Retained::as_ptr(container) as *mut _,
            &Rect {
                x: 0,
                y: 0,
                width: self.width as i32,
                height: self.height as i32,
            },
        );
        let mut context = self.space_context(workspace_id)?;
        self.next_instance += 1;
        let instance = self.next_instance;
        let mut client = tab_client(BrowserLink {
            tab_id,
            instance,
            events: self.events.clone(),
            pending: self.pending.clone(),
            keys: self.keys.clone(),
            live_browsers: self.live_browsers.clone(),
            downloads: self.downloads.clone(),
            permissions: self.permissions.clone(),
            requests: self.requests.clone(),
        });
        {
            let mut pending = self.pending.lock().expect("pending browser state");
            pending.current.insert(tab_id, instance);
            pending.requested.insert(instance, url.to_owned());
        }
        let settings = BrowserSettings {
            background_color: PAGE_BACKGROUND,
            ..Default::default()
        };
        // CEF asynchronously initializes persistent profiles. Its async browser
        // creation waits for that initialization; CreateBrowserSync does not.
        let accepted = browser_host_create_browser(
            Some(&window_info),
            Some(&mut client),
            // Start navigation only after register_pending has sized and
            // presented the native view. A fast initial document can otherwise
            // submit its first compositor frame while the container is hidden.
            None,
            Some(&settings),
            None,
            Some(&mut context),
        );
        if accepted == 0 {
            let mut pending = self.pending.lock().expect("pending browser state");
            pending.requested.remove(&instance);
            pending.current.remove(&tab_id);
            bail!("Chromium could not create the tab");
        }
        Ok(())
    }

    /// Each space is its own persistent Chromium profile: cookies, storage
    /// and cache. Every space's request context is created here.
    fn space_context(&mut self, space_id: u64) -> Result<RequestContext> {
        if let Some(context) = self.contexts.get(&space_id) {
            return Ok(context.clone());
        }
        let path = self.root_cache.join(format!("space-{space_id}"));
        std::fs::create_dir_all(&path)?;
        let settings = RequestContextSettings {
            cache_path: CefString::from(path.to_string_lossy().as_ref()),
            persist_session_cookies: 1,
            ..Default::default()
        };
        let mut blocking = blocking::request_context_handler(self.blocker.clone());
        let context = request_context_create_context(Some(&settings), Some(&mut blocking))
            .context("Could not create the space's Chromium profile")?;
        self.contexts.insert(space_id, context.clone());
        Ok(context)
    }

    /// Spaces opened before this call browse without blocking.
    pub fn set_blocker(&mut self, blocker: ContentBlocker) {
        self.blocker = blocker;
    }

    /// Takes page hosts as persisted; subdomains are covered too.
    pub fn set_blocking_exceptions(&self, hosts: HashSet<String>) {
        self.blocker.set_exceptions(hosts);
    }

    /// Lets go of a deleted space's profile once its browsers have closed.
    pub fn forget_space(&mut self, space_id: u64) {
        self.contexts.remove(&space_id);
    }

    pub fn register_pending(&mut self) {
        let ready = std::mem::take(&mut self.pending.lock().expect("pending browser state").ready);
        for ReadyBrowser {
            tab_id,
            instance,
            browser,
            url,
        } in ready
        {
            if browser.is_valid() == 0 {
                continue;
            }
            if self
                .pending
                .lock()
                .expect("pending browser state")
                .canceled
                .remove(&instance)
            {
                if let Some(host) = browser.host() {
                    host.close_browser(1);
                }
                continue;
            }
            let active = self.active_tab == Some(tab_id);
            let shown = active && !self.covered.contains(&tab_id);
            if shown {
                self.set_container_hidden(false);
            }
            if let Some(view) = browser_view(&browser) {
                view.setAutoresizingMask(NSAutoresizingMaskOptions::empty());
                view.setFrame(rect(0., 0., self.width, self.height));
                view.setHidden(!shown);
            }
            if !url.is_empty() {
                // Suppress address/title callbacks from the internal empty
                // document until the requested first navigation begins.
                self.pending
                    .lock()
                    .expect("pending browser state")
                    .started_navigation
                    .insert(instance);
                if let Some(frame) = browser.main_frame() {
                    frame.load_url(Some(&CefString::from(url.as_str())));
                }
            }
            self.browsers.insert(tab_id, browser);
            if active {
                self.update_key_routing();
            }
            if shown {
                self.set_container_hidden(false);
                self.focus_tab(tab_id);
            }
        }
    }

    pub fn activate_tab(&mut self, tab_id: u64) {
        self.active_tab = Some(tab_id);
        self.update_key_routing();
        let covered = self.covered.contains(&tab_id);
        for (&id, browser) in &self.browsers {
            if let Some(view) = browser_view(browser) {
                view.setFrameOrigin(NSPoint::new(0., 0.));
                if id == tab_id && !covered {
                    fit_view(view, self.width, self.height);
                }
                view.setHidden(id != tab_id || covered);
            }
        }
        // A page the card covers is hidden like an unloaded one, and the
        // shell keeps keyboard focus for the card.
        let live = self.live_browser(tab_id).is_some() && !covered;
        self.set_container_hidden(!live);
        if live {
            self.focus_tab(tab_id);
        } else {
            self.focus_shell();
        }
    }

    /// Input is top-left logical pixels, matching GPUI's coordinate system.
    pub fn set_bounds(&mut self, x: f64, y: f64, width: f64, height: f64) {
        let (Some(container), Some(parent)) = (&self.container, self.parent_view()) else {
            return;
        };
        let width = width.max(1.);
        let height = height.max(1.);
        let flipped = parent.isFlipped();
        let native_y = if flipped {
            y
        } else {
            parent.bounds().size.height - y - height
        };
        let frame = rect(x, native_y, width, height);
        if container.frame() != frame {
            container.setFrame(frame);
        }
        if let Some(caps) = &mut self.caps {
            caps.set_card(frame, flipped);
        }
        if self.width != width || self.height != height {
            self.width = width;
            self.height = height;
            // Every resize makes a page lay out again, so hidden pages (in
            // every space) are sized only when they are shown again.
            for browser in self.browsers.values() {
                if let Some(view) = browser_view(browser).filter(|view| !view.isHidden()) {
                    fit_view(view, width, height);
                }
            }
        }
    }

    /// Slide existing native views, without changing their viewport dimensions.
    /// A missing ID represents the GPUI new-tab page beneath the container.
    pub fn set_transition(&mut self, from_tab: u64, to_tab: u64, progress: f32, direction: i32) {
        let p = progress.clamp(0., 1.) as f64;
        let distance = self.width * if direction < 0 { -1. } else { 1. };
        let shows = |id: &u64| self.browsers.contains_key(id) && !self.covered.contains(id);
        let hidden = !shows(&from_tab) && !shows(&to_tab);
        self.set_container_hidden(hidden);
        for (&id, browser) in &self.browsers {
            if let Some(view) = browser_view(browser) {
                let visible = (id == from_tab || id == to_tab) && !self.covered.contains(&id);
                if visible {
                    fit_view(view, self.width, self.height);
                }
                view.setHidden(!visible);
                if visible {
                    view.setFrameOrigin(NSPoint::new(
                        if id == to_tab {
                            distance * (1. - p)
                        } else {
                            -distance * p
                        },
                        0.,
                    ));
                }
            }
        }
    }

    pub fn focus_tab(&self, tab_id: u64) {
        if let Some(host) = self.live_browser(tab_id).and_then(|b| b.host()) {
            host.set_focus(1);
        }
    }
    fn update_key_routing(&self) {
        self.keys.lock().expect("key routing state").browser = self
            .active_tab
            .and_then(|id| self.live_browser(id))
            .cloned();
    }
    pub fn focus_shell(&self) {
        if let Some(parent) = self.parent_view()
            && let Some(window) = parent.window()
        {
            window.makeFirstResponder(Some(parent));
        }
    }
    /// A mail, phone or messages address typed in the command bar opens in
    /// its app, as a click on such a link does.
    pub fn open_in_app(&self, url: &str) {
        protocols::open_in_app(url);
    }
    /// Edit > Undo and Redo chosen in the menu bar, for the active page if
    /// it has keyboard focus in the key window. ⌘Z and ⇧⌘Z go to the page
    /// first, and `PageKeys` undoes or redoes only if the page doesn't.
    pub fn undo_in_page(&self) {
        self.edit_page(EditCommand::Undo);
    }
    pub fn redo_in_page(&self) {
        self.edit_page(EditCommand::Redo);
    }
    fn edit_page(&self, command: EditCommand) {
        let (container, browser) = {
            let keys = self.keys.lock().expect("key routing state");
            (keys.container, keys.browser.clone())
        };
        if let Some(window) = self.parent_view().and_then(|parent| parent.window())
            && window.isKeyWindow()
            && page_has_focus(&window, container)
        {
            edit_frame(browser, command);
        }
    }
    pub fn navigate(&self, tab_id: u64, url: &str) {
        {
            let mut pending = self.pending.lock().expect("pending browser state");
            if let Some(&instance) = pending.current.get(&tab_id) {
                if let Some(requested) = pending.requested.get_mut(&instance) {
                    *requested = url.to_owned();
                }
                if let Some(ready) = pending
                    .ready
                    .iter_mut()
                    .find(|ready| ready.instance == instance)
                {
                    ready.url = url.to_owned();
                }
            }
        }
        if let Some(frame) = self.live_browser(tab_id).and_then(|b| b.main_frame()) {
            frame.load_url(Some(&CefString::from(url)));
        }
    }
    pub fn back(&self, tab_id: u64) {
        if let Some(browser) = self.live_browser(tab_id) {
            browser.go_back();
        }
    }
    pub fn forward(&self, tab_id: u64) {
        if let Some(browser) = self.live_browser(tab_id) {
            browser.go_forward();
        }
    }
    pub fn reload(&self, tab_id: u64) {
        if let Some(browser) = self.live_browser(tab_id) {
            browser.reload();
        }
    }
    pub fn stop(&self, tab_id: u64) {
        if let Some(browser) = self.live_browser(tab_id) {
            browser.stop_load();
        }
    }
    fn live_browser(&self, tab_id: u64) -> Option<&Browser> {
        self.browsers
            .get(&tab_id)
            .filter(|browser| browser.is_valid() != 0)
    }
    /// Whether a browser represents the tab: requested, being created or
    /// live. A `Closed` for a tab that has one again is stale.
    pub fn has_tab(&self, tab_id: u64) -> bool {
        self.pending
            .lock()
            .expect("pending browser state")
            .current
            .contains_key(&tab_id)
    }
    /// Drops the handle of a browser reported `Closed`. A browser created for
    /// the tab since then stays.
    pub fn forget_closed(&mut self, tab_id: u64) {
        if self
            .browsers
            .get(&tab_id)
            .is_some_and(|browser| browser.is_valid() == 0)
        {
            self.browsers.remove(&tab_id);
        }
    }
    /// Closes the tab's browser, for closing the tab or for unloading it
    /// (pinned tabs, "Unload Space") while the shell keeps its entry;
    /// `create_tab` loads it again. `Closed` follows once Chromium is done.
    pub fn close_tab(&mut self, tab_id: u64) {
        self.covered.remove(&tab_id);
        let instance = {
            let mut pending = self.pending.lock().expect("pending browser state");
            let instance = pending.current.remove(&tab_id);
            if let Some(instance) = instance
                && (pending.requested.contains_key(&instance)
                    || pending.ready.iter().any(|ready| ready.instance == instance))
            {
                pending.canceled.insert(instance);
            }
            instance
        };
        if let Some(browser) = self
            .browsers
            .remove(&tab_id)
            .filter(|browser| browser.is_valid() != 0)
        {
            // Chromium removes the view only after the page's unload
            // handlers ran; until then it would cover the next tab.
            if let Some(view) = browser_view(&browser) {
                view.setHidden(true);
            }
            if let Some(host) = browser.host() {
                host.close_browser(1);
            }
        }
        // A "Leave site?" still open lets the page go; it closes anyway.
        if let Some(instance) = instance {
            let link = self.link(tab_id, instance);
            self.requests.drop_page(&link, true);
        }
        self.update_key_routing();
    }

    /// Closes the tab's page the way a user closes it: a page with unsaved
    /// changes may ask to stay (`LeavePage`). True if the page closes now,
    /// as most do; `Closed` follows, and the shell treats the tab as
    /// closed. False while the page decides: `Closed` follows if it lets
    /// go, and nothing more if the user stays.
    pub fn close_tab_asking(&mut self, tab_id: u64) -> bool {
        let instance = self
            .pending
            .lock()
            .expect("pending browser state")
            .current
            .get(&tab_id)
            .copied();
        let (Some(instance), Some(host)) = (
            instance,
            self.live_browser(tab_id).and_then(|browser| browser.host()),
        ) else {
            // A browser still being created has no page to ask.
            self.close_tab(tab_id);
            return true;
        };
        self.pending
            .lock()
            .expect("pending browser state")
            .asking
            .insert(instance);
        // Without unload handlers Chromium closes at once (DoClose runs
        // within this call); otherwise the page's handlers run first.
        host.close_browser(0);
        let closing = {
            let mut pending = self.pending.lock().expect("pending browser state");
            let closing = pending.closing.contains(&instance);
            if closing {
                // The tab lets go of it, as `close_tab` would; asking
                // Chromium to close it again would run DoClose twice.
                pending.current.remove(&tab_id);
            }
            closing
        };
        if closing {
            self.covered.remove(&tab_id);
            if let Some(browser) = self.browsers.remove(&tab_id)
                && let Some(view) = browser_view(&browser)
            {
                view.setHidden(true);
            }
            self.update_key_routing();
        }
        closing
    }

    /// Identity for answering a browser instance's requests outside its
    /// handlers.
    fn link(&self, tab_id: u64, instance: u64) -> BrowserLink {
        BrowserLink {
            tab_id,
            instance,
            events: self.events.clone(),
            pending: self.pending.clone(),
            keys: self.keys.clone(),
            live_browsers: self.live_browsers.clone(),
            downloads: self.downloads.clone(),
            permissions: self.permissions.clone(),
            requests: self.requests.clone(),
        }
    }

    /// Answers a `SignInRequest`: signs in, or cancels without credentials.
    pub fn respond_sign_in(&self, id: u64, credentials: Option<(&str, &str)>) {
        self.requests.respond_sign_in(id, credentials);
    }

    /// Answers a `CertificateError`: the load goes on, or fails.
    pub fn respond_certificate(&self, id: u64, proceed: bool) {
        self.requests.respond_certificate(id, proceed);
    }

    /// Answers a `LeavePage`. Staying keeps a tab that was closing.
    pub fn respond_leave(&self, id: u64, leave: bool) {
        if let Some(instance) = self.requests.respond_leave(id, leave)
            && !leave
        {
            self.pending
                .lock()
                .expect("pending browser state")
                .asking
                .remove(&instance);
        }
    }

    /// Answers `Unresponsive`: wait for the page, or end it (`Crashed`
    /// follows, and a reload starts it again).
    pub fn respond_unresponsive(&self, tab_id: u64, wait: bool) {
        let instance = self
            .pending
            .lock()
            .expect("pending browser state")
            .current
            .get(&tab_id)
            .copied();
        if let Some(instance) = instance {
            self.requests.respond_unresponsive(instance, wait);
        }
    }

    /// Mutes or unmutes the tab's page.
    pub fn set_muted(&self, tab_id: u64, muted: bool) {
        if let Some(host) = self.live_browser(tab_id).and_then(|browser| browser.host()) {
            host.set_audio_muted(i32::from(muted));
        }
    }

    /// Hides the tab's page while the card shows something in its place (a
    /// crash, a sign-in, a failed load), or shows it again. GPUI can't draw
    /// over the page's native view.
    pub fn set_page_covered(&mut self, tab_id: u64, covered: bool) {
        let changed = if covered {
            self.covered.insert(tab_id)
        } else {
            self.covered.remove(&tab_id)
        };
        if changed && self.active_tab == Some(tab_id) {
            self.activate_tab(tab_id);
        }
    }

    /// Answers a `PermissionRequest`.
    pub fn respond_permission(&self, id: u64, decision: PermissionDecision) {
        self.permissions.respond(id, decision);
    }

    /// Run before leaving the application's main thread. No CEF work is allowed
    /// after this returns. Waiting is bounded and only occurs while quitting.
    pub fn shutdown(&mut self) -> Result<()> {
        if self.library.is_none() {
            return Ok(());
        }
        {
            let mut keys = self.keys.lock().expect("key routing state");
            keys.browser = None;
            keys.fullscreen = None;
        }
        self.permissions.clear();
        self.requests.leave_all();
        let popups: Vec<Browser> = {
            let mut pending = self.pending.lock().expect("pending browser state");
            pending.shutting_down = true;
            pending.popups.values().cloned().collect()
        };
        // Popup and DevTools windows normally closed with the quit request;
        // the wait below covers them too.
        for host in popups.iter().filter_map(|browser| browser.host()) {
            host.close_browser(1);
        }
        let ready = std::mem::take(&mut self.pending.lock().expect("pending browser state").ready);
        for ReadyBrowser { browser, .. } in ready {
            if let Some(view) = browser_view(&browser) {
                view.removeFromSuperview();
            }
        }
        for monitor in std::mem::take(&mut self.monitors) {
            // SAFETY: `monitor` is the token returned when the monitor was
            // added, and it is removed exactly once.
            unsafe {
                NSEvent::removeMonitor(&monitor);
            }
        }
        // AppKit application shutdown already commits to closing the window.
        // Tear down the child hierarchy synchronously, as NSWindow would, so
        // GPUI's app borrow is never re-entered through a nested NSRunLoop.
        for browser in self.browsers.values() {
            if let Some(view) = browser_view(browser) {
                view.removeFromSuperview();
            }
        }
        self.browsers.clear();
        let deadline = Instant::now() + Duration::from_secs(5);
        while (self.live_browsers.load(Ordering::SeqCst) != 0
            || !self
                .pending
                .lock()
                .expect("pending browser state")
                .requested
                .is_empty())
            && Instant::now() < deadline
        {
            do_message_loop_work();
            std::thread::sleep(Duration::from_millis(2));
        }
        if self.live_browsers.load(Ordering::SeqCst) != 0
            || !self
                .pending
                .lock()
                .expect("pending browser state")
                .requested
                .is_empty()
        {
            bail!("Chromium is still closing; skipping unsafe framework unload");
        }
        self.contexts.clear();
        if let Some(caps) = self.caps.take() {
            caps.remove();
        }
        if let Some(container) = self.container.take() {
            container.removeFromSuperview();
        }
        shutdown();
        crate::native::disable_cef_message_loop();
        if let Some(mut library) = self.library.take() {
            // SAFETY: Chromium has shut down and no CEF object is used after
            // this point, so the framework can be unloaded exactly once.
            unsafe {
                ManuallyDrop::drop(&mut library);
            }
        }
        Ok(())
    }

    /// GPUI's window view; null until `attach`.
    fn parent_view(&self) -> Option<&NSView> {
        // SAFETY: `attach` stores GPUI's content view, which lives as long as
        // the browser window. The engine is not `Send`, so this runs on the
        // main thread.
        unsafe { self.parent.as_ref() }
    }

    fn set_container_hidden(&mut self, hidden: bool) {
        if let Some(container) = &self.container {
            container.setHidden(hidden);
        }
        if let Some(caps) = &mut self.caps {
            caps.set_hidden(hidden || self.fullscreen_layout);
        }
    }
}

/// Window and card chrome.
impl Engine {
    /// Rounds the web card with `radius`, using the window colours
    /// (0xAARRGGBB) behind its top-left, top-right, bottom-left and
    /// bottom-right corners. Call again whenever they change, including on
    /// every frame of a theme transition; a zero radius leaves corners square.
    pub fn set_corner_caps(&mut self, radius: f64, colors: [u32; 4]) {
        if let Some(caps) = &mut self.caps {
            caps.set_style(radius, colors);
        }
    }

    /// In HTML fullscreen the card fills the window with square corners.
    pub fn set_fullscreen_layout(&mut self, fullscreen: bool) {
        self.fullscreen_layout = fullscreen;
        let hidden = self
            .container
            .as_ref()
            .is_none_or(|container| container.isHidden());
        self.set_container_hidden(hidden);
    }

    /// Leaves HTML fullscreen, for example when the window leaves fullscreen
    /// or another tab is selected. `Fullscreen { fullscreen: false }` follows.
    pub fn exit_fullscreen(&self, tab_id: u64) {
        if let Some(host) = self.live_browser(tab_id).and_then(|b| b.host())
            && host.is_fullscreen() != 0
        {
            host.exit_fullscreen(1);
        }
    }

    /// `child_view` is the NSView of a GPUI pop-up window (the command bar).
    /// It becomes a borderless child window directly above the browser
    /// window, made key when `focus` (it has a text field). Detach it before
    /// closing it.
    pub fn attach_child_window(&self, child_view: *mut c_void, focus: bool) {
        if let (Some(parent), Some(child)) = (self.parent_view(), child_view_ref(child_view)) {
            crate::native::attach_child_window(parent, child, focus);
        }
    }

    pub fn detach_child_window(&self, child_view: *mut c_void) {
        if let (Some(parent), Some(child)) = (self.parent_view(), child_view_ref(child_view)) {
            crate::native::detach_child_window(parent, child);
        }
    }

    /// Shows or hides a GPUI pop-up window (`child_view`) over the browser
    /// window that the pointer passes through, such as the link status.
    pub fn show_overlay_window(&self, child_view: *mut c_void, shown: bool) {
        if let (Some(parent), Some(child)) = (self.parent_view(), child_view_ref(child_view)) {
            crate::native::show_overlay_window(parent, child, shown);
        }
    }

    /// Moves an attached child window's top-left corner to `x`, `y` in the
    /// browser window (top-left logical pixels, as GPUI measures).
    pub fn place_child_window(&self, child_view: *mut c_void, x: f64, y: f64) {
        if let (Some(parent), Some(child)) = (self.parent_view(), child_view_ref(child_view)) {
            crate::native::place_child_window(parent, child, NSPoint::new(x, y));
        }
    }

    /// Hides the close, minimize and zoom buttons while the sidebar is hidden.
    pub fn set_traffic_lights_hidden(&self, hidden: bool) {
        if let Some(parent) = self.parent_view() {
            crate::native::set_traffic_lights_hidden(parent, hidden);
        }
    }
}

fn child_view_ref<'a>(view: *mut c_void) -> Option<&'a NSView> {
    // SAFETY: Callers pass the NSView from a live GPUI window's raw handle,
    // on the main thread (the engine is not `Send`), or null.
    unsafe { view.cast::<NSView>().as_ref() }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

/// Sizes a page's view to the card, before it is shown.
fn fit_view(view: &NSView, width: f64, height: f64) {
    let size = NSSize::new(width, height);
    if view.frame().size != size {
        view.setFrameSize(size);
    }
}

fn trace_native_layout(tab_id: u64, view: &NSView) {
    if let Some(window) = view.window() {
        eprintln!(
            "CEF initial layout tab {tab_id}: window visible={} key={} occlusion={:?} frame={:?}",
            window.isVisible(),
            window.isKeyWindow(),
            window.occlusionState(),
            window.frame()
        );
    }
    fn trace_view(view: &NSView, depth: usize) {
        eprintln!(
            "CEF view depth={depth} class={} frame={:?} bounds={:?} hidden={} hidden_ancestor={}",
            view.class().name().to_string_lossy(),
            view.frame(),
            view.bounds(),
            view.isHidden(),
            view.isHiddenOrHasHiddenAncestor()
        );
        if depth < 4 {
            for child in view.subviews() {
                trace_view(&child, depth + 1);
            }
        }
    }
    // SAFETY: Only reads the hierarchy on the main thread; the container and
    // GPUI's view are alive while a browser view is.
    if let Some(parent) = unsafe { view.superview().and_then(|container| container.superview()) } {
        trace_view(&parent, 0);
    } else {
        trace_view(view, 0);
    }
}

fn browser_view(browser: &Browser) -> Option<&NSView> {
    if browser.is_valid() == 0 {
        return None;
    }
    let raw = browser.host()?.window_handle().cast::<NSView>();
    // SAFETY: All callers execute on the UI thread; a valid browser's child
    // NSView is retained by its parent. No event loop runs between this check
    // and use.
    unsafe { raw.as_ref() }
}

fn install_key_monitor(
    events: Sender<EngineEvent>,
    keys: Arc<Mutex<KeyRouting>>,
) -> Option<Retained<AnyObject>> {
    let block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit passes a valid event that outlives this handler.
        let e = unsafe { event.as_ref() };
        let mtm = MainThreadMarker::new().expect("native event on main thread");
        // Only keys for the browser window itself. The command bar handles
        // its own (GPUI binds the same shortcuts there), popup windows have
        // PageKeys, and file panels, dialogs and other sheets keep theirs.
        let container = keys.lock().expect("key routing state").container;
        let Some(window) = e
            .window(mtm)
            .filter(|window| is_browser_window(window, container))
        else {
            return event.as_ptr();
        };
        if window.attachedSheet().is_some()
            || NSApplication::sharedApplication(mtm)
                .modalWindow()
                .is_some()
        {
            return event.as_ptr();
        }
        let Some(press) = KeyPress::from_ns_event(e) else {
            return event.as_ptr();
        };
        if press.is_escape() && exit_page_fullscreen(&keys) {
            return std::ptr::null_mut();
        }
        let shortcut = press.shortcut();
        // A focused page gets the editing keys and all but the reserved
        // shortcuts first. GPUI's view would claim them before the page's
        // view is asked, so the page's view gets them directly; PageKeys
        // runs those it ignores. A view that declines (it isn't the key
        // window's first responder) leaves the key to AppKit as usual.
        let page_first = edit_command(press.character, press.modifiers).is_some()
            || shortcut.is_some_and(|shortcut| !shortcut.reserved());
        if page_first && page_has_focus(&window, container) {
            return if page_keys::send_to_page(&window, e) {
                std::ptr::null_mut()
            } else {
                event.as_ptr()
            };
        }
        let Some(shortcut) = shortcut else {
            return event.as_ptr();
        };
        // ⌘← and ⌘→ move the caret in the shell's own text fields (a space
        // being renamed); GPUI's bindings give those fields the keys first.
        if press.moves_caret() {
            return event.as_ptr();
        }
        let _ = events.try_send(EngineEvent::Shortcut(shortcut));
        std::ptr::null_mut()
    });
    // SAFETY: The handler returns either the event or null to consume it, as
    // local monitors must; AppKit copies the block.
    unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &block) }
}

/// Whether `window` holds the web container, at address `container` (or 0
/// before `attach`).
fn is_browser_window(window: &NSWindow, container: usize) -> bool {
    // SAFETY: A non-zero `container` is the engine's web container, alive
    // until `shutdown`, which removes this monitor first. Main thread only.
    unsafe { (container as *const NSView).as_ref() }
        .and_then(|container| container.window())
        .is_some_and(|browser| std::ptr::eq(&*browser, window))
}

/// Esc leaves HTML fullscreen, wherever keyboard focus is. False when no
/// page is fullscreen.
fn exit_page_fullscreen(keys: &Mutex<KeyRouting>) -> bool {
    // Leaving fullscreen calls back into the display handler, which locks
    // the routing state, so release it first.
    let fullscreen = keys
        .lock()
        .expect("key routing state")
        .fullscreen
        .as_ref()
        .map(|(_, browser)| browser.clone());
    let Some(host) = fullscreen
        .filter(|browser| browser.is_valid() != 0)
        .and_then(|browser| browser.host())
    else {
        return false;
    };
    host.exit_fullscreen(1);
    true
}

/// Whether keyboard focus is in the web container at address `container`.
fn page_has_focus(window: &NSWindow, container: usize) -> bool {
    window.firstResponder().is_some_and(|responder| {
        // SAFETY: Both messages are NSObject/NSView queries with object
        // arguments; `container` is the live web container (checked
        // non-zero) and `responder` is retained for this call.
        unsafe {
            let is_view: Bool = msg_send![&responder, isKindOfClass: NSView::class()];
            if !is_view.as_bool() || container == 0 {
                return false;
            }
            let inside: Bool = msg_send![&responder, isDescendantOf: container as *mut NSView];
            inside.as_bool()
        }
    })
}

/// Runs `command` in the page's focused frame; false without one.
fn edit_frame(browser: Option<Browser>, command: EditCommand) -> bool {
    let Some(frame) = browser
        .filter(|browser| browser.is_valid() != 0)
        .and_then(|browser| browser.focused_frame())
    else {
        return false;
    };
    match command {
        EditCommand::SelectAll => frame.select_all(),
        EditCommand::Copy => frame.copy(),
        EditCommand::Cut => frame.cut(),
        EditCommand::Paste => frame.paste(),
        EditCommand::Undo => frame.undo(),
        EditCommand::Redo => frame.redo(),
    }
    true
}

/// Reports presses on the page (`PagePressed`). The page handles them as
/// usual; GPUI never sees them, since the page's views are native.
fn install_mouse_monitor(
    events: Sender<EngineEvent>,
    keys: Arc<Mutex<KeyRouting>>,
) -> Option<Retained<AnyObject>> {
    let block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit passes a valid event that outlives this handler.
        let e = unsafe { event.as_ref() };
        let mtm = MainThreadMarker::new().expect("native event on main thread");
        let container = keys.lock().expect("key routing state").container;
        if e.window(mtm)
            .is_some_and(|window| is_browser_window(&window, container))
            && hits_page(e, container)
        {
            let _ = events.try_send(EngineEvent::PagePressed);
        }
        event.as_ptr()
    });
    // SAFETY: The handler always returns the event, so AppKit delivers it
    // unchanged; AppKit copies the block.
    unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(
            NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown | NSEventMask::OtherMouseDown,
            &block,
        )
    }
}

/// Whether a mouse event in the browser window lands on a page view inside
/// the web container at address `container` (or 0 before `attach`).
fn hits_page(event: &NSEvent, container: usize) -> bool {
    // SAFETY: As in `is_browser_window`: the container is alive until
    // `shutdown` removes the monitors, and this runs on the main thread.
    let Some(container) = (unsafe { (container as *const NSView).as_ref() }) else {
        return false;
    };
    // SAFETY: A view's superview getter, on the main thread.
    let Some(parent) = (unsafe { container.superview() }) else {
        return false;
    };
    // hitTest: takes the point in the superview's coordinates and returns
    // nothing for points outside the container or while it is hidden.
    let point = parent.convertPoint_fromView(event.locationInWindow(), None);
    container.hitTest(point).is_some()
}
