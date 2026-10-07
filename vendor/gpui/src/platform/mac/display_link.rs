use crate::{
    dispatch_get_main_queue,
    dispatch_sys::{
        _dispatch_source_type_data_add, dispatch_object_t, dispatch_resume, dispatch_set_context,
        dispatch_source_cancel, dispatch_source_create, dispatch_source_merge_data,
        dispatch_source_set_event_handler_f, dispatch_source_t,
    },
};
use anyhow::Result;
use core_graphics::display::CGDirectDisplayID;
use std::ffi::c_void;
use util::ResultExt;

unsafe extern "C" {
    // libdispatch (libSystem). Not in the generated bindings; same signature as the
    // generated `dispatch_resume`.
    fn dispatch_release(object: dispatch_object_t);
}

/// A CVDisplayLink that forwards each refresh to `callback` on the main queue.
///
/// The link can be started and stopped any number of times. The dispatch source that
/// carries refreshes to the main queue is activated once when the link is created and
/// cancelled when it is dropped. It is never suspended, so suspend and resume calls cannot
/// become unbalanced (over-resuming a dispatch object crashes). A refresh merged just before
/// `stop` may still be delivered once afterwards; callers must tolerate that.
///
/// Dropping a link that was started leaks the CVDisplayLink and the cancelled source (see
/// `Drop`), so callers should replace links only when needed.
pub struct DisplayLink {
    display_link: Option<sys::DisplayLink>,
    frame_requests: dispatch_source_t,
    display_id: CGDirectDisplayID,
}

impl DisplayLink {
    pub fn new(
        display_id: CGDirectDisplayID,
        data: *mut c_void,
        callback: unsafe extern "C" fn(*mut c_void),
    ) -> Result<DisplayLink> {
        unsafe extern "C" fn display_link_callback(
            _display_link_out: *mut sys::CVDisplayLink,
            _current_time: *const sys::CVTimeStamp,
            _output_time: *const sys::CVTimeStamp,
            _flags_in: i64,
            _flags_out: *mut i64,
            frame_requests: *mut c_void,
        ) -> i32 {
            unsafe {
                let frame_requests = frame_requests as dispatch_source_t;
                dispatch_source_merge_data(frame_requests, 1);
                0
            }
        }

        unsafe {
            // The CVDisplayLink comes first, so that its failures leave no dispatch source
            // behind. Until it is started no CoreVideo thread uses it, and dropping it here
            // releases it.
            let mut display_link = sys::DisplayLink::new(display_id)?;

            let frame_requests = dispatch_source_create(
                &_dispatch_source_type_data_add,
                0,
                0,
                dispatch_get_main_queue(),
            );
            anyhow::ensure!(
                !frame_requests.is_null(),
                "could not create dispatch source"
            );
            let source = dispatch_object_t {
                _ds: frame_requests,
            };
            if let Err(error) = display_link
                .set_output_callback(display_link_callback, frame_requests as *mut c_void)
            {
                // SAFETY: releasing an inactive source crashes, so activate it, cancel it, then
                // release it. It has no handler yet, and the link never ran, so nothing else
                // refers to the source.
                dispatch_resume(source);
                dispatch_source_cancel(frame_requests);
                dispatch_release(source);
                return Err(error);
            }
            dispatch_set_context(source, data);
            dispatch_source_set_event_handler_f(frame_requests, Some(callback));
            // Dispatch sources are created inactive. This is the only resume.
            dispatch_resume(source);

            Ok(Self {
                display_link: Some(display_link),
                frame_requests,
                display_id,
            })
        }
    }

    /// The display this link was created for.
    pub fn display_id(&self) -> CGDirectDisplayID {
        self.display_id
    }

    /// Starts the CVDisplayLink. Does nothing if it is already running.
    ///
    /// CoreVideo's own running state is the source of truth (as in Chromium), so a failed
    /// `stop` cannot leave a running link that every later `start` rejects.
    pub fn start(&mut self) -> Result<()> {
        unsafe {
            let display_link = self.display_link.as_mut().unwrap();
            if !display_link.is_running() {
                display_link.start()?;
            }
        }
        Ok(())
    }

    /// Stops the CVDisplayLink. Does nothing if it is already stopped.
    pub fn stop(&mut self) -> Result<()> {
        unsafe {
            let display_link = self.display_link.as_mut().unwrap();
            if display_link.is_running() {
                display_link.stop()?;
            }
        }
        Ok(())
    }
}

impl Drop for DisplayLink {
    fn drop(&mut self) {
        self.stop().log_err();
        // We see occasional segfaults on the CVDisplayLink thread.
        //
        // It seems possible that this happens because CVDisplayLinkRelease releases the CVDisplayLink
        // on the main thread immediately, but the background thread that CVDisplayLink uses for timers
        // is still accessing it.
        //
        // We might also want to upgrade to CADisplayLink, but that requires dropping old macOS support.
        //
        // Upstream later found (zed#32116) that `CVDisplayLinkStop` only asks that thread to
        // exit and returns at once. Releasing the link after a delay would not be safe either,
        // because nothing bounds when the thread exits: it may be waiting for a refresh, for
        // example until a sleeping display wakes. The cancelled source is not released for the
        // same reason, since that thread may still pass it to `dispatch_source_merge_data`.
        // Cancelled, it no longer wakes the main queue.
        std::mem::forget(self.display_link.take());
        unsafe {
            dispatch_source_cancel(self.frame_requests);
        }
    }
}

mod sys {
    //! Derived from display-link crate under the following license:
    //! <https://github.com/BrainiumLLC/display-link/blob/master/LICENSE-MIT>
    //! Apple docs: [CVDisplayLink](https://developer.apple.com/documentation/corevideo/cvdisplaylinkoutputcallback?language=objc)
    #![allow(dead_code, non_upper_case_globals)]

    use anyhow::Result;
    use core_graphics::display::CGDirectDisplayID;
    use foreign_types::{ForeignType, foreign_type};
    use std::{
        ffi::c_void,
        fmt::{self, Debug, Formatter},
    };

    #[derive(Debug)]
    pub enum CVDisplayLink {}

    foreign_type! {
        pub unsafe type DisplayLink {
            type CType = CVDisplayLink;
            fn drop = CVDisplayLinkRelease;
            fn clone = CVDisplayLinkRetain;
        }
    }

    impl Debug for DisplayLink {
        fn fmt(&self, formatter: &mut Formatter) -> fmt::Result {
            formatter
                .debug_tuple("DisplayLink")
                .field(&self.as_ptr())
                .finish()
        }
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(crate) struct CVTimeStamp {
        pub version: u32,
        pub video_time_scale: i32,
        pub video_time: i64,
        pub host_time: u64,
        pub rate_scalar: f64,
        pub video_refresh_period: i64,
        pub smpte_time: CVSMPTETime,
        pub flags: u64,
        pub reserved: u64,
    }

    pub type CVTimeStampFlags = u64;

    pub const kCVTimeStampVideoTimeValid: CVTimeStampFlags = 1 << 0;
    pub const kCVTimeStampHostTimeValid: CVTimeStampFlags = 1 << 1;
    pub const kCVTimeStampSMPTETimeValid: CVTimeStampFlags = 1 << 2;
    pub const kCVTimeStampVideoRefreshPeriodValid: CVTimeStampFlags = 1 << 3;
    pub const kCVTimeStampRateScalarValid: CVTimeStampFlags = 1 << 4;
    pub const kCVTimeStampTopField: CVTimeStampFlags = 1 << 16;
    pub const kCVTimeStampBottomField: CVTimeStampFlags = 1 << 17;
    pub const kCVTimeStampVideoHostTimeValid: CVTimeStampFlags =
        kCVTimeStampVideoTimeValid | kCVTimeStampHostTimeValid;
    pub const kCVTimeStampIsInterlaced: CVTimeStampFlags =
        kCVTimeStampTopField | kCVTimeStampBottomField;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub(crate) struct CVSMPTETime {
        pub subframes: i16,
        pub subframe_divisor: i16,
        pub counter: u32,
        pub time_type: u32,
        pub flags: u32,
        pub hours: i16,
        pub minutes: i16,
        pub seconds: i16,
        pub frames: i16,
    }

    pub type CVSMPTETimeType = u32;

    pub const kCVSMPTETimeType24: CVSMPTETimeType = 0;
    pub const kCVSMPTETimeType25: CVSMPTETimeType = 1;
    pub const kCVSMPTETimeType30Drop: CVSMPTETimeType = 2;
    pub const kCVSMPTETimeType30: CVSMPTETimeType = 3;
    pub const kCVSMPTETimeType2997: CVSMPTETimeType = 4;
    pub const kCVSMPTETimeType2997Drop: CVSMPTETimeType = 5;
    pub const kCVSMPTETimeType60: CVSMPTETimeType = 6;
    pub const kCVSMPTETimeType5994: CVSMPTETimeType = 7;

    pub type CVSMPTETimeFlags = u32;

    pub const kCVSMPTETimeValid: CVSMPTETimeFlags = 1 << 0;
    pub const kCVSMPTETimeRunning: CVSMPTETimeFlags = 1 << 1;

    pub type CVDisplayLinkOutputCallback = unsafe extern "C" fn(
        display_link_out: *mut CVDisplayLink,
        // A pointer to the current timestamp. This represents the timestamp when the callback is called.
        current_time: *const CVTimeStamp,
        // A pointer to the output timestamp. This represents the timestamp for when the frame will be displayed.
        output_time: *const CVTimeStamp,
        // Unused
        flags_in: i64,
        // Unused
        flags_out: *mut i64,
        // A pointer to app-defined data.
        display_link_context: *mut c_void,
    ) -> i32;

    #[link(name = "CoreFoundation", kind = "framework")]
    #[link(name = "CoreVideo", kind = "framework")]
    #[allow(improper_ctypes, unknown_lints, clippy::duplicated_attributes)]
    unsafe extern "C" {
        pub fn CVDisplayLinkCreateWithActiveCGDisplays(
            display_link_out: *mut *mut CVDisplayLink,
        ) -> i32;
        pub fn CVDisplayLinkSetCurrentCGDisplay(
            display_link: &mut DisplayLinkRef,
            display_id: u32,
        ) -> i32;
        pub fn CVDisplayLinkSetOutputCallback(
            display_link: &mut DisplayLinkRef,
            callback: CVDisplayLinkOutputCallback,
            user_info: *mut c_void,
        ) -> i32;
        pub fn CVDisplayLinkStart(display_link: &mut DisplayLinkRef) -> i32;
        pub fn CVDisplayLinkStop(display_link: &mut DisplayLinkRef) -> i32;
        pub fn CVDisplayLinkIsRunning(display_link: &DisplayLinkRef) -> u8;
        pub fn CVDisplayLinkRelease(display_link: *mut CVDisplayLink);
        pub fn CVDisplayLinkRetain(display_link: *mut CVDisplayLink) -> *mut CVDisplayLink;
    }

    impl DisplayLink {
        /// Apple docs: [CVDisplayLinkCreateWithCGDisplay](https://developer.apple.com/documentation/corevideo/1456981-cvdisplaylinkcreatewithcgdisplay?language=objc)
        pub unsafe fn new(display_id: CGDirectDisplayID) -> Result<Self> {
            unsafe {
                let mut display_link: *mut CVDisplayLink = 0 as _;

                let code = CVDisplayLinkCreateWithActiveCGDisplays(&mut display_link);
                anyhow::ensure!(code == 0, "could not create display link, code: {}", code);

                let mut display_link = DisplayLink::from_ptr(display_link);

                let code = CVDisplayLinkSetCurrentCGDisplay(&mut display_link, display_id);
                anyhow::ensure!(
                    code == 0,
                    "could not assign display to display link, code: {}",
                    code
                );

                Ok(display_link)
            }
        }
    }

    impl DisplayLinkRef {
        /// Apple docs: [CVDisplayLinkSetOutputCallback](https://developer.apple.com/documentation/corevideo/1457096-cvdisplaylinksetoutputcallback?language=objc)
        pub unsafe fn set_output_callback(
            &mut self,
            callback: CVDisplayLinkOutputCallback,
            user_info: *mut c_void,
        ) -> Result<()> {
            unsafe {
                let code = CVDisplayLinkSetOutputCallback(self, callback, user_info);
                anyhow::ensure!(code == 0, "could not set output callback, code: {}", code);
                Ok(())
            }
        }

        /// Apple docs: [CVDisplayLinkStart](https://developer.apple.com/documentation/corevideo/1457193-cvdisplaylinkstart?language=objc)
        pub unsafe fn start(&mut self) -> Result<()> {
            unsafe {
                let code = CVDisplayLinkStart(self);
                anyhow::ensure!(code == 0, "could not start display link, code: {}", code);
                Ok(())
            }
        }

        /// Apple docs: [CVDisplayLinkStop](https://developer.apple.com/documentation/corevideo/1457281-cvdisplaylinkstop?language=objc)
        pub unsafe fn stop(&mut self) -> Result<()> {
            unsafe {
                let code = CVDisplayLinkStop(self);
                anyhow::ensure!(code == 0, "could not stop display link, code: {}", code);
                Ok(())
            }
        }

        /// Whether CoreVideo reports the link as running (`CVDisplayLinkIsRunning`).
        pub unsafe fn is_running(&self) -> bool {
            unsafe { CVDisplayLinkIsRunning(self) != 0 }
        }
    }
}
