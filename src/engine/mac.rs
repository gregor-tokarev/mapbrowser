//! Chromium runs in native child NSViews. Pages keep their own compositor and
//! process lifetime; GPUI only draws the browser controls.

use super::{EngineEvent, Shortcut};
use anyhow::{Context, Result, bail, ensure};
use async_channel::{Receiver, Sender};
use block2::RcBlock;
use cef::*;
use objc2::{
    ClassType, MainThreadMarker, MainThreadOnly, msg_send,
    rc::Retained,
    runtime::{AnyObject, Bool},
};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSEvent, NSEventMask, NSEventModifierFlags, NSView,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::{
    collections::{HashMap, HashSet},
    ffi::c_void,
    mem::ManuallyDrop,
    path::{Path, PathBuf},
    ptr::NonNull,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Default)]
struct PendingBrowsers {
    requested_urls: HashMap<u64, (String, String)>,
    ready: Vec<(u64, Browser, String)>,
    canceled: HashSet<u64>,
    shutting_down: bool,
    live: HashMap<u64, Browser>,
    started_navigation: HashSet<u64>,
    initialized_surface: HashSet<u64>,
}

#[derive(Default)]
struct NativeEditing {
    container: usize,
    browser: Option<Browser>,
}

wrap_app! {
    struct BrowserApp;
    impl App {
        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(PumpHandler::new())
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

wrap_client! {
    struct TabClient {
        tab_id: u64,
        events: Sender<EngineEvent>,
        live_browsers: Arc<AtomicUsize>,
        pending: Arc<Mutex<PendingBrowsers>>,
    }
    impl Client {
        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(TabDisplay::new(self.tab_id, self.events.clone(), self.pending.clone()))
        }
        fn load_handler(&self) -> Option<LoadHandler> {
            Some(TabLoad::new(self.tab_id, self.events.clone(), self.pending.clone()))
        }
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(TabLife::new(self.tab_id, self.events.clone(), self.live_browsers.clone(), self.pending.clone()))
        }
    }
}

wrap_display_handler! {
    struct TabDisplay { tab_id: u64, events: Sender<EngineEvent>, pending: Arc<Mutex<PendingBrowsers>> }
    impl DisplayHandler {
        fn on_address_change(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, url: Option<&CefString>) {
            if !self.pending.lock().expect("pending browser state").started_navigation.contains(&self.tab_id) { return; }
            if frame.is_some_and(|frame| frame.is_main() != 0) {
                let _ = self.events.try_send(EngineEvent::Address { tab_id: self.tab_id, url: url.map(ToString::to_string).unwrap_or_default() });
            }
        }
        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
            if !self.pending.lock().expect("pending browser state").started_navigation.contains(&self.tab_id) { return; }
            let _ = self.events.try_send(EngineEvent::Title { tab_id: self.tab_id, title: title.map(ToString::to_string).unwrap_or_default() });
        }
    }
}

wrap_load_handler! {
    struct TabLoad { tab_id: u64, events: Sender<EngineEvent>, pending: Arc<Mutex<PendingBrowsers>> }
    impl LoadHandler {
        fn on_loading_state_change(&self, _browser: Option<&mut Browser>, is_loading: i32, can_go_back: i32, can_go_forward: i32) {
            let _ = self.events.try_send(EngineEvent::Loading { tab_id: self.tab_id, loading: is_loading != 0, can_go_back: can_go_back != 0, can_go_forward: can_go_forward != 0 });
        }
        fn on_load_error(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, error_code: Errorcode, error_text: Option<&CefString>, _failed_url: Option<&CefString>) {
            if error_code != Errorcode::ABORTED && frame.is_some_and(|frame| frame.is_main() != 0) {
                let _ = self.events.try_send(EngineEvent::LoadError { tab_id: self.tab_id, message: error_text.map(ToString::to_string).unwrap_or_else(|| "Page could not be loaded".into()) });
            }
        }
        fn on_load_end(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, _http_status_code: i32) {
            if !frame.is_some_and(|frame| frame.is_main() != 0 && CefString::from(&frame.url()).to_string() != "about:blank") { return; }
            if !self.pending.lock().expect("pending browser state").initialized_surface.insert(self.tab_id) { return; }
            if let Some(view) = browser.and_then(|browser| browser_view(browser)) {
                if std::env::var_os("MAPBROWSER_TRACE_CEF").is_some() {
                    trace_native_layout(self.tab_id, view);
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
                    eprintln!("CEF initialized native surface tab {} at {}x{}", self.tab_id, size.width, size.height);
                }
            }
        }
    }
}

wrap_life_span_handler! {
    struct TabLife { tab_id: u64, events: Sender<EngineEvent>, live_browsers: Arc<AtomicUsize>, pending: Arc<Mutex<PendingBrowsers>> }
    impl LifeSpanHandler {
        fn on_after_created(&self, browser: Option<&mut Browser>) {
            let Some(browser) = browser else { return; };
            self.live_browsers.fetch_add(1, Ordering::SeqCst);
            let mut pending = self.pending.lock().expect("pending browser state");
            pending.live.insert(self.tab_id, browser.clone());
            let url = pending.requested_urls.remove(&self.tab_id).filter(|(initial, current)| initial != current).map(|(_, current)| current).unwrap_or_default();
            if pending.shutting_down || pending.canceled.remove(&self.tab_id) {
                drop(pending);
                if let Some(host) = browser.host() { host.close_browser(1); }
            } else {
                if let Some(view) = browser_view(browser) { view.setHidden(true); }
                pending.ready.push((self.tab_id, browser.clone(), url));
                drop(pending);
                let _ = self.events.try_send(EngineEvent::Created { tab_id: self.tab_id });
            }
        }
        fn on_before_popup(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, _popup_id: i32, target_url: Option<&CefString>, _target_frame_name: Option<&CefString>, _target_disposition: WindowOpenDisposition, user_gesture: i32, _popup_features: Option<&PopupFeatures>, _window_info: Option<&mut WindowInfo>, _client: Option<&mut Option<Client>>, _settings: Option<&mut BrowserSettings>, _extra_info: Option<&mut Option<DictionaryValue>>, _no_javascript_access: Option<&mut i32>) -> i32 {
            if user_gesture != 0 {
                if let Some(url) = target_url.filter(|url| !url.to_string().is_empty()) {
                    let _ = self.events.try_send(EngineEvent::OpenTab { opener_tab_id: self.tab_id, url: url.to_string() });
                }
            }
            1
        }
        fn do_close(&self, browser: Option<&mut Browser>) -> i32 {
            // Closing a tab must destroy its child NSView, never its GPUI window.
            // Defer destruction until CEF has returned from this callback.
            if let Some(host) = browser.and_then(|browser| browser.host()) {
                let raw = host.window_handle().cast::<NSView>();
                if !raw.is_null() {
                    let retained = unsafe { Retained::retain(raw) }.expect("CEF view");
                    let address = Retained::into_raw(retained) as usize;
                    let mut task = CloseViewTask::new(address);
                    if post_task(ThreadId::UI, Some(&mut task)) == 0 {
                        // Scheduling only fails during teardown. Release the
                        // extra retain so view hierarchy destruction can finish.
                        unsafe { drop(Retained::from_raw(address as *mut NSView)); }
                    }
                }
            }
            1
        }
        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            self.live_browsers.fetch_sub(1, Ordering::SeqCst);
            let ready_to_quit = {
                let mut pending = self.pending.lock().expect("pending browser state");
                pending.live.remove(&self.tab_id);
                pending.started_navigation.remove(&self.tab_id);
                pending.initialized_surface.remove(&self.tab_id);
                pending.shutting_down && pending.live.is_empty() && pending.requested_urls.is_empty()
            };
            let _ = self.events.try_send(EngineEvent::Closed { tab_id: self.tab_id });
            if ready_to_quit { quit_message_loop(); }
        }
    }
}

wrap_task! {
    struct CloseViewTask { view_address: usize }
    impl Task {
        fn execute(&self) {
            unsafe {
                let view = Retained::from_raw(self.view_address as *mut NSView).expect("retained CEF view");
                view.removeFromSuperview();
            }
        }
    }
}

pub struct Engine {
    library: Option<ManuallyDrop<library_loader::LibraryLoader>>,
    contexts: HashMap<u64, RequestContext>,
    browsers: HashMap<u64, Browser>,
    pending: Arc<Mutex<PendingBrowsers>>,
    container: Option<Retained<NSView>>,
    parent: *mut NSView,
    monitor: Option<Retained<AnyObject>>,
    editing: Arc<Mutex<NativeEditing>>,
    root_cache: PathBuf,
    events: Sender<EngineEvent>,
    live_browsers: Arc<AtomicUsize>,
    active_tab: Option<u64>,
    width: f64,
    height: f64,
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
        let editing = Arc::new(Mutex::new(NativeEditing::default()));
        let monitor = install_shortcuts(events.clone(), editing.clone());
        let pending = Arc::new(Mutex::new(PendingBrowsers::default()));
        let quit_pending = pending.clone();
        crate::native::set_quit_handler(move || {
            let (browsers, can_quit) = {
                let mut pending = quit_pending.lock().expect("pending browser state");
                pending.shutting_down = true;
                (
                    pending.live.values().cloned().collect::<Vec<_>>(),
                    pending.live.is_empty() && pending.requested_urls.is_empty(),
                )
            };
            if can_quit {
                quit_message_loop();
                return;
            }
            // Keep the native loop running until OnBeforeClose acknowledges
            // every browser. Closing after NSApp.run returns is too late.
            for browser in browsers {
                if browser.is_valid() != 0 {
                    if let Some(host) = browser.host() {
                        host.close_browser(1);
                    }
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
                parent: std::ptr::null_mut(),
                monitor,
                editing,
                root_cache,
                events,
                live_browsers: Arc::new(AtomicUsize::new(0)),
                active_tab: None,
                width: 800.,
                height: 600.,
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
        self.parent = parent.cast();
        let mtm = MainThreadMarker::new().context("attach must run on the main thread")?;
        let container =
            NSView::initWithFrame(NSView::alloc(mtm), rect(0., 0., self.width, self.height));
        container.setWantsLayer(true);
        unsafe {
            let layer: *mut AnyObject = msg_send![&container, layer];
            let _: () = msg_send![layer, setMasksToBounds: Bool::YES];
            let _: () = msg_send![layer, setCornerRadius: 10.0f64];
            (&*self.parent).addSubview(&container);
        }
        container.setHidden(true);
        self.editing.lock().expect("native editing state").container =
            Retained::as_ptr(&container) as usize;
        self.container = Some(container);
        Ok(())
    }

    pub fn create_tab(&mut self, tab_id: u64, workspace_id: u64, url: &str) -> Result<()> {
        if std::env::var_os("MAPBROWSER_TRACE_CEF").is_some() {
            eprintln!(
                "CEF create tab {tab_id}, space {workspace_id}, on_ui={}",
                currently_on(ThreadId::UI)
            );
        }
        if self.has_tab(tab_id)
            || self
                .pending
                .lock()
                .expect("pending browser state")
                .requested_urls
                .contains_key(&tab_id)
        {
            self.navigate(tab_id, url);
            return Ok(());
        }
        let container = self
            .container
            .as_ref()
            .context("Attach the native view before creating a tab")?;
        if !self.contexts.contains_key(&workspace_id) {
            let path = self.root_cache.join(format!("space-{workspace_id}"));
            std::fs::create_dir_all(&path)?;
            let settings = RequestContextSettings {
                cache_path: CefString::from(path.to_string_lossy().as_ref()),
                persist_session_cookies: 1,
                ..Default::default()
            };
            let context = request_context_create_context(Some(&settings), None)
                .context("Could not create the space's Chromium profile")?;
            self.contexts.insert(workspace_id, context);
        }
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
        let mut client = TabClient::new(
            tab_id,
            self.events.clone(),
            self.live_browsers.clone(),
            self.pending.clone(),
        );
        self.pending
            .lock()
            .expect("pending browser state")
            .requested_urls
            .insert(tab_id, (String::new(), url.to_owned()));
        // CEF asynchronously initializes persistent profiles. Its async browser
        // creation waits for that initialization; CreateBrowserSync does not.
        let accepted = browser_host_create_browser(
            Some(&window_info),
            Some(&mut client),
            // Start navigation only after register_pending has sized and
            // presented the native view. A fast initial document can otherwise
            // submit its first compositor frame while the container is hidden.
            None,
            Some(&BrowserSettings::default()),
            None,
            self.contexts.get_mut(&workspace_id),
        );
        if accepted == 0 {
            self.pending
                .lock()
                .expect("pending browser state")
                .requested_urls
                .remove(&tab_id);
            bail!("Chromium could not create the tab");
        }
        Ok(())
    }

    pub fn register_pending(&mut self) {
        let ready = std::mem::take(&mut self.pending.lock().expect("pending browser state").ready);
        for (tab_id, browser, url) in ready {
            if browser.is_valid() == 0 {
                continue;
            }
            if self
                .pending
                .lock()
                .expect("pending browser state")
                .canceled
                .remove(&tab_id)
            {
                if let Some(host) = browser.host() {
                    host.close_browser(1);
                }
                continue;
            }
            let active = self.active_tab == Some(tab_id);
            if active {
                if let Some(container) = &self.container {
                    container.setHidden(false);
                }
            }
            if let Some(view) = browser_view(&browser) {
                view.setAutoresizingMask(NSAutoresizingMaskOptions::empty());
                view.setHidden(!active);
                view.setFrame(rect(0., 0., self.width, self.height));
            }
            if !url.is_empty() {
                // Suppress address/title callbacks from the internal empty
                // document until the requested first navigation begins.
                self.pending
                    .lock()
                    .expect("pending browser state")
                    .started_navigation
                    .insert(tab_id);
                if let Some(frame) = browser.main_frame() {
                    frame.load_url(Some(&CefString::from(url.as_str())));
                }
            }
            self.browsers.insert(tab_id, browser);
            if active {
                if let Some(container) = &self.container {
                    container.setHidden(false);
                }
                self.focus_tab(tab_id);
                self.update_native_editing();
            }
        }
    }

    pub fn activate_tab(&mut self, tab_id: u64) {
        self.active_tab = Some(tab_id);
        self.update_native_editing();
        for (&id, browser) in &self.browsers {
            if let Some(view) = browser_view(browser) {
                view.setFrameOrigin(NSPoint::new(0., 0.));
                view.setHidden(id != tab_id);
            }
        }
        if let Some(container) = &self.container {
            container.setHidden(!self.has_tab(tab_id));
        }
        if self.has_tab(tab_id) {
            self.focus_tab(tab_id);
        } else {
            self.focus_shell();
        }
    }

    /// Input is top-left logical pixels, matching GPUI's coordinate system.
    pub fn set_bounds(&mut self, x: f64, y: f64, width: f64, height: f64) {
        let Some(container) = &self.container else {
            return;
        };
        let width = width.max(1.);
        let height = height.max(1.);
        let parent = unsafe { &*self.parent };
        let native_y = if parent.isFlipped() {
            y
        } else {
            parent.bounds().size.height - y - height
        };
        let frame = rect(x, native_y, width, height);
        if container.frame() != frame {
            container.setFrame(frame);
        }
        if self.width != width || self.height != height {
            self.width = width;
            self.height = height;
            for browser in self.browsers.values() {
                if let Some(view) = browser_view(browser) {
                    view.setFrameSize(NSSize::new(width, height));
                }
            }
        }
    }

    /// Slide existing native views, without changing their viewport dimensions.
    /// A missing ID represents the GPUI new-tab page beneath the container.
    pub fn set_transition(&mut self, from_tab: u64, to_tab: u64, progress: f32, direction: i32) {
        let p = progress.clamp(0., 1.) as f64;
        let distance = self.width * if direction < 0 { -1. } else { 1. };
        if let Some(container) = &self.container {
            container.setHidden(
                !self.browsers.contains_key(&from_tab) && !self.browsers.contains_key(&to_tab),
            );
        }
        for (&id, browser) in &self.browsers {
            if let Some(view) = browser_view(browser) {
                let visible = id == from_tab || id == to_tab;
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
    fn update_native_editing(&self) {
        self.editing.lock().expect("native editing state").browser = self
            .active_tab
            .and_then(|id| self.live_browser(id))
            .cloned();
    }
    pub fn focus_shell(&self) {
        if let Some(parent) = unsafe { self.parent.as_ref() } {
            if let Some(window) = parent.window() {
                window.makeFirstResponder(Some(parent));
            }
        }
    }
    pub fn navigate(&self, tab_id: u64, url: &str) {
        {
            let mut pending = self.pending.lock().expect("pending browser state");
            if let Some(requested) = pending.requested_urls.get_mut(&tab_id) {
                requested.1 = url.to_owned();
            }
            if let Some((_, _, requested)) =
                pending.ready.iter_mut().find(|(id, _, _)| *id == tab_id)
            {
                *requested = url.to_owned();
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
    pub fn has_tab(&self, tab_id: u64) -> bool {
        self.live_browser(tab_id).is_some()
    }
    pub fn forget_closed(&mut self, tab_id: u64) {
        self.browsers.remove(&tab_id);
    }
    pub fn close_tab(&mut self, tab_id: u64) {
        {
            let mut pending = self.pending.lock().expect("pending browser state");
            if pending.requested_urls.contains_key(&tab_id)
                || pending.ready.iter().any(|(id, _, _)| *id == tab_id)
            {
                pending.canceled.insert(tab_id);
            }
        }
        if let Some(browser) = self
            .browsers
            .remove(&tab_id)
            .filter(|browser| browser.is_valid() != 0)
        {
            if let Some(host) = browser.host() {
                host.close_browser(1);
            }
        }
    }

    /// Run before leaving the application's main thread. No CEF work is allowed
    /// after this returns. Waiting is bounded and only occurs while quitting.
    pub fn shutdown(&mut self) -> Result<()> {
        if self.library.is_none() {
            return Ok(());
        }
        self.editing.lock().expect("native editing state").browser = None;
        self.pending
            .lock()
            .expect("pending browser state")
            .shutting_down = true;
        let ready = std::mem::take(&mut self.pending.lock().expect("pending browser state").ready);
        for (_, browser, _) in ready {
            if let Some(view) = browser_view(&browser) {
                view.removeFromSuperview();
            }
        }
        if let Some(monitor) = self.monitor.take() {
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
                .requested_urls
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
                .requested_urls
                .is_empty()
        {
            bail!("Chromium is still closing; skipping unsafe framework unload");
        }
        self.contexts.clear();
        if let Some(container) = self.container.take() {
            container.removeFromSuperview();
        }
        shutdown();
        crate::native::disable_cef_message_loop();
        if let Some(mut library) = self.library.take() {
            unsafe {
                ManuallyDrop::drop(&mut library);
            }
        }
        Ok(())
    }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
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
    // All callers execute on the UI thread; a valid browser's child NSView is
    // retained by its parent. No event loop runs between this check and use.
    unsafe { raw.as_ref() }
}

fn install_shortcuts(
    events: Sender<EngineEvent>,
    editing: Arc<Mutex<NativeEditing>>,
) -> Option<Retained<AnyObject>> {
    let block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        let e = unsafe { event.as_ref() };
        let flags = e.modifierFlags();
        let command = flags.contains(NSEventModifierFlags::Command);
        let control = flags.contains(NSEventModifierFlags::Control);
        let option = flags.contains(NSEventModifierFlags::Option);
        let shift = flags.contains(NSEventModifierFlags::Shift);
        let key = e.keyCode();
        if command && !control && !option && matches!(key, 0 | 6 | 7 | 8 | 9) {
            let (container, browser) = {
                let editing = editing.lock().expect("native editing state");
                (editing.container, editing.browser.clone())
            };
            let web_has_focus = e
                .window(MainThreadMarker::new().expect("native event on main thread"))
                .and_then(|window| window.firstResponder())
                .is_some_and(|responder| unsafe {
                    let is_view: Bool = msg_send![&responder, isKindOfClass: NSView::class()];
                    if !is_view.as_bool() || container == 0 {
                        return false;
                    }
                    let inside: Bool =
                        msg_send![&responder, isDescendantOf: container as *mut NSView];
                    inside.as_bool()
                });
            if web_has_focus {
                if let Some(frame) = browser
                    .filter(|browser| browser.is_valid() != 0)
                    .and_then(|browser| browser.focused_frame())
                {
                    let handled = match (key, shift) {
                        (0, false) => {
                            frame.select_all();
                            true
                        }
                        (8, false) => {
                            frame.copy();
                            true
                        }
                        (7, false) => {
                            frame.cut();
                            true
                        }
                        (9, false) => {
                            frame.paste();
                            true
                        }
                        (6, false) => {
                            frame.undo();
                            true
                        }
                        (6, true) => {
                            frame.redo();
                            true
                        }
                        _ => false,
                    };
                    if handled {
                        return std::ptr::null_mut();
                    }
                }
            }
        }
        let shortcut = match (command, control, option, shift, key) {
            (true, false, false, false, 37) => Some(Shortcut::Address),
            (true, false, false, false, 17) => Some(Shortcut::NewTab),
            (true, false, false, false, 13) => Some(Shortcut::CloseTab),
            (true, false, false, false, 15) => Some(Shortcut::Reload),
            (true, false, false, false, 33) => Some(Shortcut::Back),
            (true, false, false, false, 30) => Some(Shortcut::Forward),
            (false, true, false, false, 48) => Some(Shortcut::NextTab),
            (true, false, true, false, 123) => Some(Shortcut::PreviousSpace),
            (true, false, true, false, 124) => Some(Shortcut::NextSpace),
            _ => None,
        };
        if let Some(shortcut) = shortcut {
            let _ = events.try_send(EngineEvent::Shortcut(shortcut));
            std::ptr::null_mut()
        } else {
            event.as_ptr()
        }
    });
    unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &block) }
}
