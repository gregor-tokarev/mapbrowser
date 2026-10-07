//! GPUI owns the application and views; Chromium drives the native AppKit loop.

use std::{
    cell::{Cell, RefCell},
    ffi::CString,
    rc::Rc,
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Instant,
};

use anyhow::{Context, Result, ensure};
use objc2::{
    Encode, MainThreadMarker, ffi, msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject, AnyProtocol, Bool, Imp, ProtocolBuilder, Sel},
    sel,
};
use objc2_foundation::NSNotification;

type SendEvent = unsafe extern "C-unwind" fn(&AnyObject, Sel, &AnyObject);
type RunApplication = unsafe extern "C-unwind" fn(&AnyObject, Sel);
type TerminateApplication = unsafe extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject);

static ORIGINAL_SEND_EVENT: OnceLock<SendEvent> = OnceLock::new();
static ORIGINAL_RUN: OnceLock<RunApplication> = OnceLock::new();
static ORIGINAL_TERMINATE: OnceLock<TerminateApplication> = OnceLock::new();
static PROTOCOL_INSTALLED: OnceLock<std::result::Result<(), String>> = OnceLock::new();
static NATIVE_LOOP_READY: AtomicBool = AtomicBool::new(false);
static NATIVE_LOOP_FINISHED: AtomicBool = AtomicBool::new(false);
static TERMINATION_REQUESTED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static HANDLING_SEND_EVENT: Cell<Bool> = const { Cell::new(Bool::NO) };
    static IN_CEF_RUN: Cell<bool> = const { Cell::new(false) };
    static QUIT_HANDLER: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
}

unsafe extern "C-unwind" fn is_handling_send_event(_app: &AnyObject, _cmd: Sel) -> Bool {
    HANDLING_SEND_EVENT.get()
}

unsafe extern "C-unwind" fn set_handling_send_event(_app: &AnyObject, _cmd: Sel, handling: Bool) {
    HANDLING_SEND_EVENT.set(handling);
}

struct SendingEventGuard(Bool);

impl Drop for SendingEventGuard {
    fn drop(&mut self) {
        HANDLING_SEND_EVENT.set(self.0);
    }
}

unsafe extern "C-unwind" fn send_event(app: &AnyObject, command: Sel, event: &AnyObject) {
    let _guard = SendingEventGuard(HANDLING_SEND_EVENT.replace(Bool::YES));
    if let Some(original) = ORIGINAL_SEND_EVENT.get() {
        // SAFETY: This is the original implementation of sendEvent: on the same
        // application class, called with the original receiver and arguments.
        unsafe { original(app, command, event) };
    }
}

unsafe extern "C-unwind" fn run_application(app: &AnyObject, command: Sel) {
    if !NATIVE_LOOP_READY.load(Ordering::Acquire)
        || NATIVE_LOOP_FINISHED.load(Ordering::Acquire)
        || IN_CEF_RUN.get()
    {
        // CEF's native message pump calls NSApp.run itself. This inner entry
        // must invoke AppKit, with Chromium's task sources now attached.
        if let Some(original) = ORIGINAL_RUN.get() {
            unsafe { original(app, command) };
        }
        return;
    }

    IN_CEF_RUN.set(true);
    trace_cef_loop(|| "native run begin".to_owned());
    cef::run_message_loop();
    trace_cef_loop(|| "native run end".to_owned());
    // Final browser teardown may call CefDoMessageLoopWork, which can enter
    // NSApp.run briefly. Those calls must not start another CefRunMessageLoop.
    NATIVE_LOOP_FINISHED.store(true, Ordering::Release);
    IN_CEF_RUN.set(false);

    // GPUI's ordinary terminate: path exits the process. Deliver its cleanup
    // notification after the Chromium loop has returned instead. Observers
    // may persist state and release windows; CefShutdown runs later in main.
    unsafe {
        let delegate: *mut AnyObject = msg_send![app, delegate];
        if !delegate.is_null() {
            let notification = NSNotification::notificationWithName_object(
                objc2_app_kit::NSApplicationWillTerminateNotification,
                Some(app),
            );
            let _: () = msg_send![delegate, applicationWillTerminate: &*notification];
        }
    }
}

unsafe extern "C-unwind" fn terminate_application(
    app: &AnyObject,
    command: Sel,
    sender: *mut AnyObject,
) {
    if !NATIVE_LOOP_READY.load(Ordering::Acquire) {
        if let Some(original) = ORIGINAL_TERMINATE.get() {
            unsafe { original(app, command, sender) };
        }
        return;
    }
    if !TERMINATION_REQUESTED.swap(true, Ordering::AcqRel) {
        trace_cef_loop(|| "native quit requested".to_owned());
        // Release the RefCell borrow before invoking application code. The
        // handler closes browsers while Chromium's native loop is still live,
        // then its last on_before_close callback requests the loop's exit.
        let handler = QUIT_HANDLER.with_borrow(Clone::clone);
        if let Some(handler) = handler {
            handler();
        } else {
            cef::quit_message_loop();
        }
    }
}

/// Handle the first native quit request on the main thread. The handler must
/// call CefQuitMessageLoop once all browsers have completed their close flow.
pub fn set_quit_handler(handler: impl Fn() + 'static) {
    assert!(MainThreadMarker::new().is_some());
    QUIT_HANDLER.with_borrow_mut(|current| *current = Some(Rc::new(handler)));
}

/// Route GPUI's next NSApplication.run through CEF's native message pump.
/// Call after successful CefInitialize with external_message_pump disabled.
pub fn enable_cef_message_loop() {
    assert!(MainThreadMarker::new().is_some());
    NATIVE_LOOP_READY.store(true, Ordering::Release);
}

/// Disable CEF routing after CefShutdown, or when CefInitialize fails.
/// Keep routing enabled during final browser teardown so terminate: remains
/// intercepted while CefDoMessageLoopWork drains the remaining work.
pub fn disable_cef_message_loop() {
    assert!(
        MainThreadMarker::new().is_some(),
        "CEF must stop on the main thread"
    );
    assert!(
        !IN_CEF_RUN.get(),
        "CEF's main loop must return before shutdown"
    );
    NATIVE_LOOP_READY.store(false, Ordering::Release);
    NATIVE_LOOP_FINISHED.store(true, Ordering::Release);
    QUIT_HANDLER.with_borrow_mut(|handler| *handler = None);
}

/// Extend GPUI's existing NSApplication subclass with CEF's required protocol.
///
/// Call on the main thread after loading the CEF framework, before initializing
/// CEF. Creating a separate NSApplication would lose GPUI's platform ivar.
pub fn install_cef_application_protocol() -> Result<()> {
    ensure!(
        MainThreadMarker::new().is_some(),
        "CEF must initialize on the main thread"
    );
    let result = PROTOCOL_INSTALLED
        .get_or_init(|| install_protocol_methods().map_err(|error| format!("{error:#}")));
    result
        .as_ref()
        .map_err(|error| anyhow::anyhow!(error.clone()))?;
    Ok(())
}

fn install_protocol_methods() -> Result<()> {
    // GPUI 0.2.2 registers this class in a process constructor, before main.
    let class = AnyClass::get(c"GPUIApplication")
        .context("GPUI did not register its GPUIApplication class")?;
    let original = class
        .instance_method(sel!(sendEvent:))
        .context("GPUIApplication has no sendEvent: implementation")?;
    // SAFETY: NSApplication's documented sendEvent: method takes one NSEvent
    // object and returns void. The pointer remains valid for the process life.
    let original = unsafe { std::mem::transmute::<Imp, SendEvent>(original.implementation()) };
    ORIGINAL_SEND_EVENT
        .set(original)
        .map_err(|_| anyhow::anyhow!("CEF application methods were already installed"))?;
    let original_run = class
        .instance_method(sel!(run))
        .context("NSApplication has no run method")?;
    let original_terminate = class
        .instance_method(sel!(terminate:))
        .context("NSApplication has no terminate method")?;
    // SAFETY: These are the documented NSApplication method signatures.
    unsafe {
        ORIGINAL_RUN
            .set(std::mem::transmute::<Imp, RunApplication>(
                original_run.implementation(),
            ))
            .map_err(|_| anyhow::anyhow!("NSApplication run bridge was already installed"))?;
        ORIGINAL_TERMINATE
            .set(std::mem::transmute::<Imp, TerminateApplication>(
                original_terminate.implementation(),
            ))
            .map_err(|_| anyhow::anyhow!("NSApplication terminate bridge was already installed"))?;
    }

    let get_encoding = CString::new(format!("{}@:", Bool::ENCODING))?;
    let set_encoding = CString::new(format!("v@:{}", Bool::ENCODING))?;
    let class_ptr = class as *const AnyClass as *mut AnyClass;

    // SAFETY: All methods use the Objective-C ABI and the selector's exact
    // argument/return types. Only this process's GPUI subclass is modified.
    unsafe {
        ensure!(
            ffi::class_addMethod(
                class_ptr,
                sel!(isHandlingSendEvent),
                std::mem::transmute::<unsafe extern "C-unwind" fn(&AnyObject, Sel) -> Bool, Imp>(
                    is_handling_send_event
                ),
                get_encoding.as_ptr(),
            )
            .as_bool(),
            "GPUIApplication already defines isHandlingSendEvent"
        );
        ensure!(
            ffi::class_addMethod(
                class_ptr,
                sel!(setHandlingSendEvent:),
                std::mem::transmute::<unsafe extern "C-unwind" fn(&AnyObject, Sel, Bool), Imp>(
                    set_handling_send_event
                ),
                set_encoding.as_ptr(),
            )
            .as_bool(),
            "GPUIApplication already defines setHandlingSendEvent:"
        );
        ensure!(
            ffi::class_addMethod(
                class_ptr,
                sel!(sendEvent:),
                std::mem::transmute::<SendEvent, Imp>(send_event),
                c"v@:@".as_ptr(),
            )
            .as_bool(),
            "GPUIApplication already overrides sendEvent:; its integration must be reviewed"
        );
        ensure!(
            ffi::class_addMethod(
                class_ptr,
                sel!(run),
                std::mem::transmute::<RunApplication, Imp>(run_application),
                c"v@:".as_ptr()
            )
            .as_bool(),
            "GPUIApplication already overrides run; its integration must be reviewed"
        );
        ensure!(
            ffi::class_addMethod(
                class_ptr,
                sel!(terminate:),
                std::mem::transmute::<TerminateApplication, Imp>(terminate_application),
                c"v@:@".as_ptr()
            )
            .as_bool(),
            "GPUIApplication already overrides terminate:; its integration must be reviewed"
        );

        let protocol = cef_application_protocol()?;
        ensure!(
            ffi::class_addProtocol(class_ptr, protocol).as_bool()
                || ffi::class_conformsToProtocol(class_ptr, protocol).as_bool(),
            "Could not add CefAppProtocol to GPUIApplication"
        );

        // GPUI normally creates its singleton only in Application::run. CEF
        // initializes earlier and also asks for sharedApplication, so create
        // GPUI's subclass first to prevent an ordinary NSApplication singleton.
        let app: Retained<AnyObject> = msg_send![class, sharedApplication];
        let is_gpui_application: Bool = msg_send![&app, isKindOfClass: class];
        ensure!(
            is_gpui_application.as_bool(),
            "NSApplication was created before GPUIApplication; initialize the native bridge earlier"
        );
    }
    Ok(())
}

fn cef_application_protocol() -> Result<&'static AnyProtocol> {
    // Some CEF builds declare these protocols in headers without registering
    // every protocol in the dynamic library. Match cef_application_mac.h.
    if let Some(protocol) = AnyProtocol::get(c"CefAppProtocol") {
        return Ok(protocol);
    }
    let app = match AnyProtocol::get(c"CrAppProtocol") {
        Some(protocol) => protocol,
        None => {
            let mut builder =
                ProtocolBuilder::new(c"CrAppProtocol").context("Could not create CrAppProtocol")?;
            builder.add_method_description::<(), Bool>(sel!(isHandlingSendEvent), true);
            builder.register()
        }
    };
    let control = match AnyProtocol::get(c"CrAppControlProtocol") {
        Some(protocol) => protocol,
        None => {
            let mut builder = ProtocolBuilder::new(c"CrAppControlProtocol")
                .context("Could not create CrAppControlProtocol")?;
            builder.add_protocol(app);
            builder.add_method_description::<(Bool,), ()>(sel!(setHandlingSendEvent:), true);
            builder.register()
        }
    };
    let mut builder =
        ProtocolBuilder::new(c"CefAppProtocol").context("Could not create CefAppProtocol")?;
    builder.add_protocol(control);
    Ok(builder.register())
}

fn trace_cef_loop(message: impl FnOnce() -> String) {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    static START: OnceLock<Instant> = OnceLock::new();
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    if *ENABLED.get_or_init(|| std::env::var_os("MAPBROWSER_TRACE_CEF").is_some())
        && COUNT.fetch_add(1, Ordering::Relaxed) < 256
    {
        eprintln!(
            "CEF loop +{:.3}s {}",
            START.get_or_init(Instant::now).elapsed().as_secs_f64(),
            message()
        );
    }
}

/// Respect the user's macOS accessibility preference for workspace movement.
pub fn reduce_motion() -> bool {
    let workspace = objc2_app_kit::NSWorkspace::sharedWorkspace();
    unsafe {
        let reduced: Bool = msg_send![&workspace, accessibilityDisplayShouldReduceMotion];
        reduced.as_bool()
    }
}
