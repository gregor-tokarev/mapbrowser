//! Demand-driven frame pacing for platforms whose vsync source can be paused.
//!
//! The macOS backend used to run a display link for every visible window and invoke the
//! frame callback on every refresh (60-120 Hz) even when nothing had changed. A
//! [`FramePacing`] value decides when a window's vsync source must run instead: GPUI calls
//! [`FramePacing::request`] whenever the window needs a frame, and the source stops after
//! `idle_limit` consecutive ticks that served no request.
//!
//! A vsync source that the platform believes is running but that delivers no ticks would
//! freeze the window, so the platform also runs a stall watchdog while the source should run:
//! see [`FramePacing::watchdog`].
//!
//! This file has no dependencies so that its tests run on any host:
//! `rustc --edition 2024 --test src/platform/frame_pacing.rs`.
//! It performs no I/O. Callers apply the returned [`LinkAction`]. All methods are called on
//! the main thread.

/// What the platform must do with its vsync source after a state change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub(crate) enum LinkAction {
    /// Leave the vsync source as it is.
    None,
    /// Start (or resume) the vsync source.
    Start,
    /// Stop (pause) the vsync source.
    Stop,
}

/// What the platform must do when its stall watchdog fires. See [`FramePacing::watchdog`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub(crate) enum Watchdog {
    /// Stop watching: the vsync source is stopped and no demand waits for it.
    Disarm,
    /// Apply the action (`Start` retries a failed start), then fire again after another
    /// watchdog period.
    Rearm(LinkAction),
    /// The vsync source should run but delivered no tick during a whole watchdog period.
    /// Replace it with a new one and start that, then fire again after another period.
    Recreate,
}

/// Frame pacing state for one window.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FramePacing {
    idle_limit: u32,
    idle_ticks: u32,
    visible: bool,
    closed: bool,
    /// Whether the vsync source has been told to run.
    running: bool,
    /// Whether a frame callback is in progress.
    in_frame: bool,
    /// Demand received since the current frame began, or since the last frame ended.
    pending: bool,
    /// Demand that the frame in progress is serving.
    serving: bool,
    /// Whether the platform has a watchdog timer pending.
    watchdog_armed: bool,
    /// Whether a tick began since the last watchdog check.
    ticked: bool,
}

impl FramePacing {
    /// Creates the state for a hidden window. `idle_limit` is the number of consecutive
    /// ticks without demand after which the vsync source stops (at least 1).
    pub(crate) const fn new(idle_limit: u32) -> Self {
        Self {
            idle_limit: if idle_limit == 0 { 1 } else { idle_limit },
            idle_ticks: 0,
            visible: false,
            closed: false,
            running: false,
            in_frame: false,
            pending: false,
            serving: false,
            watchdog_armed: false,
            ticked: false,
        }
    }

    /// Whether the vsync source should currently be running.
    #[allow(dead_code)]
    pub(crate) fn is_running(&self) -> bool {
        self.running
    }

    /// Whether demand is waiting for a frame.
    #[allow(dead_code)]
    pub(crate) fn has_pending_request(&self) -> bool {
        self.pending
    }

    /// Whether the window is visible, as last reported by [`Self::set_visible`].
    #[allow(dead_code)]
    pub(crate) fn is_visible(&self) -> bool {
        self.visible
    }

    /// Whether [`Self::close`] was called.
    #[allow(dead_code)]
    pub(crate) fn is_closed(&self) -> bool {
        self.closed
    }

    /// GPUI needs a frame: the window became dirty, queued a next-frame callback, or has a
    /// scene that was drawn but not presented. Safe to call at any time on the main thread,
    /// including from inside a frame callback. Demand received while the window is hidden is
    /// kept and served once it becomes visible.
    pub(crate) fn request(&mut self) -> LinkAction {
        if self.closed {
            return LinkAction::None;
        }
        self.pending = true;
        self.idle_ticks = 0;
        self.start_if_visible()
    }

    /// Called for every vsync tick. Returns whether the frame callback should run. When it
    /// returns `true`, call [`Self::end_tick`] after the callback, or [`Self::abort_frame`]
    /// if the callback could not run. Ticks that arrive after a stop, while hidden, or while
    /// another frame is in progress (a nested run loop) are ignored.
    pub(crate) fn begin_tick(&mut self) -> bool {
        if self.closed || !self.visible || !self.running || self.in_frame {
            return false;
        }
        self.ticked = true;
        self.begin_frame();
        true
    }

    /// Finishes a tick started by [`Self::begin_tick`].
    pub(crate) fn end_tick(&mut self) -> LinkAction {
        if self.end_frame() {
            self.idle_ticks = 0;
            return self.start_if_visible();
        }
        self.idle_ticks = self.idle_ticks.saturating_add(1);
        if self.running && self.idle_ticks >= self.idle_limit {
            self.running = false;
            LinkAction::Stop
        } else {
            LinkAction::None
        }
    }

    /// Starts a frame the platform runs synchronously, outside the vsync source (for
    /// example while AppKit asks the layer to display during a live resize). Returns `false`
    /// if a frame is already in progress or the window is closed.
    pub(crate) fn begin_sync_frame(&mut self) -> bool {
        if self.closed || self.in_frame {
            return false;
        }
        self.begin_frame();
        true
    }

    /// Finishes a frame started by [`Self::begin_sync_frame`]. Synchronous frames do not
    /// count towards the idle limit.
    pub(crate) fn end_sync_frame(&mut self) -> LinkAction {
        let _served = self.end_frame();
        if self.pending {
            self.idle_ticks = 0;
            self.start_if_visible()
        } else {
            LinkAction::None
        }
    }

    /// The frame callback could not run after [`Self::begin_tick`] or
    /// [`Self::begin_sync_frame`] returned `true`. Keeps its demand for the next frame and
    /// does not count the tick as idle.
    pub(crate) fn abort_frame(&mut self) {
        self.in_frame = false;
        self.pending |= self.serving;
        self.serving = false;
    }

    /// Reports the window's visibility (for macOS, its occlusion state). Hiding stops the
    /// vsync source. Showing counts as demand and starts the source until the window is
    /// idle: demand recorded while hidden is served, the platform can present the current
    /// scene again, and a failed start is retried by the watchdog.
    pub(crate) fn set_visible(&mut self, visible: bool) -> LinkAction {
        if self.closed || visible == self.visible {
            return LinkAction::None;
        }
        self.visible = visible;
        if visible {
            self.pending = true;
            self.idle_ticks = 0;
            self.start_if_visible()
        } else {
            self.stop()
        }
    }

    /// The platform could not start its vsync source. The next request or watchdog check
    /// retries.
    pub(crate) fn start_failed(&mut self) {
        self.running = false;
    }

    /// The platform started (or replaced and started) its vsync source.
    pub(crate) fn source_started(&mut self) {
        // A watchdog check that is already pending may fire before the new source's first
        // tick, so it must not count against the source. A check armed after this one comes
        // a whole period later and does.
        self.ticked = self.watchdog_armed;
    }

    /// Returns `true` if the platform must schedule a call to [`Self::watchdog`] one watchdog
    /// period from now, or `false` if one is already pending. The platform calls this after
    /// every attempt to start its vsync source, successful or not.
    pub(crate) fn arm_watchdog(&mut self) -> bool {
        !std::mem::replace(&mut self.watchdog_armed, true)
    }

    /// The watchdog period elapsed. While the vsync source should run, the platform keeps a
    /// watchdog armed. If no tick began during a whole period (and no frame is in progress,
    /// which would hold back ticks from a nested run loop), the source is stalled and must be
    /// replaced. If a start failed while demand is waiting, this retries it. Otherwise the
    /// watchdog stops, so an idle window has no timer.
    pub(crate) fn watchdog(&mut self) -> Watchdog {
        self.watchdog_armed = false;
        let ticked = std::mem::take(&mut self.ticked);
        if self.closed || !self.visible {
            Watchdog::Disarm
        } else if self.running {
            if ticked || self.in_frame {
                Watchdog::Rearm(LinkAction::None)
            } else {
                Watchdog::Recreate
            }
        } else if self.pending {
            Watchdog::Rearm(self.start_if_visible())
        } else {
            Watchdog::Disarm
        }
    }

    /// The window is closing. Stops the vsync source and ignores later requests.
    pub(crate) fn close(&mut self) -> LinkAction {
        self.closed = true;
        self.pending = false;
        self.serving = false;
        self.stop()
    }

    fn begin_frame(&mut self) {
        self.in_frame = true;
        self.serving = self.pending;
        self.pending = false;
    }

    /// Ends the frame in progress and reports whether it served or received demand.
    fn end_frame(&mut self) -> bool {
        self.in_frame = false;
        let busy = self.serving || self.pending;
        self.serving = false;
        busy
    }

    fn stop(&mut self) -> LinkAction {
        if self.running {
            self.running = false;
            LinkAction::Stop
        } else {
            LinkAction::None
        }
    }

    fn start_if_visible(&mut self) -> LinkAction {
        if self.visible && !self.running && !self.closed {
            self.running = true;
            LinkAction::Start
        } else {
            LinkAction::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FramePacing, LinkAction, Watchdog};

    const LIMIT: u32 = 4;

    /// A visible window whose link runs and has served the demand of becoming visible.
    fn visible() -> FramePacing {
        let mut pacing = FramePacing::new(LIMIT);
        assert_eq!(pacing.set_visible(true), LinkAction::Start);
        assert!(pacing.begin_tick());
        assert_eq!(pacing.end_tick(), LinkAction::None);
        pacing
    }

    /// Runs one tick whose frame callback does nothing.
    fn idle_tick(pacing: &mut FramePacing) -> LinkAction {
        assert!(pacing.begin_tick());
        pacing.end_tick()
    }

    #[test]
    fn hidden_window_does_not_start() {
        let mut pacing = FramePacing::new(LIMIT);
        assert_eq!(pacing.request(), LinkAction::None);
        assert!(!pacing.is_running());
        assert!(!pacing.begin_tick());
        // Demand recorded while hidden is served after showing.
        assert_eq!(pacing.set_visible(true), LinkAction::Start);
        assert!(pacing.begin_tick());
        assert_eq!(pacing.end_tick(), LinkAction::None);
    }

    #[test]
    fn stops_after_idle_limit_and_restarts_on_request() {
        let mut pacing = visible();
        for _ in 0..LIMIT - 1 {
            assert_eq!(idle_tick(&mut pacing), LinkAction::None);
        }
        assert_eq!(idle_tick(&mut pacing), LinkAction::Stop);
        assert!(!pacing.is_running());
        // A tick already in flight when the link stopped is ignored.
        assert!(!pacing.begin_tick());
        assert_eq!(pacing.request(), LinkAction::Start);
        // A second request while running does nothing.
        assert_eq!(pacing.request(), LinkAction::None);
    }

    #[test]
    fn tick_serving_a_request_is_not_idle() {
        let mut pacing = visible();
        for _ in 0..LIMIT - 1 {
            assert_eq!(idle_tick(&mut pacing), LinkAction::None);
        }
        assert_eq!(pacing.request(), LinkAction::None);
        // This tick draws; the idle count restarts after it.
        assert_eq!(idle_tick(&mut pacing), LinkAction::None);
        for _ in 0..LIMIT - 1 {
            assert_eq!(idle_tick(&mut pacing), LinkAction::None);
        }
        assert_eq!(idle_tick(&mut pacing), LinkAction::Stop);
    }

    #[test]
    fn request_inside_frame_keeps_link_running() {
        let mut pacing = visible();
        // An animation requests the next frame from inside every frame.
        for _ in 0..(LIMIT * 3) {
            assert!(pacing.begin_tick());
            assert_eq!(pacing.request(), LinkAction::None);
            assert_eq!(pacing.end_tick(), LinkAction::None);
        }
        assert!(pacing.is_running());
    }

    #[test]
    fn request_inside_frame_with_limit_one() {
        let mut pacing = FramePacing::new(1);
        assert_eq!(pacing.set_visible(true), LinkAction::Start);
        assert!(pacing.begin_tick());
        assert_eq!(pacing.request(), LinkAction::None);
        assert_eq!(pacing.end_tick(), LinkAction::None);
        assert!(pacing.begin_tick());
        assert_eq!(pacing.end_tick(), LinkAction::None);
        assert!(pacing.begin_tick());
        assert_eq!(pacing.end_tick(), LinkAction::Stop);
    }

    #[test]
    fn nested_tick_is_ignored_and_keeps_demand() {
        let mut pacing = visible();
        assert_eq!(pacing.request(), LinkAction::None);
        assert!(pacing.begin_tick());
        // A nested run loop delivers another tick while the frame callback runs.
        assert!(!pacing.begin_tick());
        assert_eq!(pacing.end_tick(), LinkAction::None);
    }

    #[test]
    fn aborted_frame_keeps_demand() {
        let mut pacing = visible();
        assert_eq!(pacing.request(), LinkAction::None);
        assert!(pacing.begin_tick());
        pacing.abort_frame();
        assert!(pacing.has_pending_request());
        assert!(pacing.begin_tick());
        assert_eq!(pacing.end_tick(), LinkAction::None);
        assert!(!pacing.has_pending_request());
    }

    #[test]
    fn sync_frame_while_paused() {
        let mut pacing = visible();
        for _ in 0..LIMIT {
            let _ = idle_tick(&mut pacing);
        }
        assert!(!pacing.is_running());
        // displayLayer: draws synchronously without the link.
        assert!(pacing.begin_sync_frame());
        assert_eq!(pacing.end_sync_frame(), LinkAction::None);
        assert!(!pacing.is_running());
        // If the synchronous frame leaves work behind, the link starts.
        assert!(pacing.begin_sync_frame());
        assert_eq!(pacing.request(), LinkAction::Start);
        assert_eq!(pacing.end_sync_frame(), LinkAction::None);
        assert!(pacing.is_running());
    }

    #[test]
    fn hiding_stops_and_showing_restarts() {
        let mut pacing = visible();
        assert_eq!(pacing.set_visible(false), LinkAction::Stop);
        assert_eq!(pacing.set_visible(false), LinkAction::None);
        assert_eq!(pacing.request(), LinkAction::None);
        assert_eq!(pacing.set_visible(true), LinkAction::Start);
        assert_eq!(pacing.set_visible(true), LinkAction::None);
    }

    #[test]
    fn hiding_inside_a_frame() {
        let mut pacing = visible();
        assert!(pacing.begin_tick());
        assert_eq!(pacing.set_visible(false), LinkAction::Stop);
        assert_eq!(pacing.request(), LinkAction::None);
        assert_eq!(pacing.end_tick(), LinkAction::None);
        assert!(!pacing.is_running());
        assert_eq!(pacing.set_visible(true), LinkAction::Start);
    }

    #[test]
    fn closed_window_ignores_everything() {
        let mut pacing = visible();
        assert_eq!(pacing.close(), LinkAction::Stop);
        assert_eq!(pacing.request(), LinkAction::None);
        assert_eq!(pacing.set_visible(true), LinkAction::None);
        assert!(!pacing.begin_tick());
        assert!(!pacing.begin_sync_frame());
    }

    #[test]
    fn failed_start_is_retried() {
        let mut pacing = FramePacing::new(LIMIT);
        assert_eq!(pacing.set_visible(true), LinkAction::Start);
        pacing.start_failed();
        assert_eq!(pacing.request(), LinkAction::Start);
    }

    #[test]
    fn showing_counts_as_demand() {
        let mut pacing = FramePacing::new(LIMIT);
        assert_eq!(pacing.set_visible(true), LinkAction::Start);
        assert!(pacing.has_pending_request());
        // The first tick serves it, so the link runs for a full idle period afterwards.
        assert!(pacing.begin_tick());
        assert_eq!(pacing.end_tick(), LinkAction::None);
        for _ in 0..LIMIT - 1 {
            assert_eq!(idle_tick(&mut pacing), LinkAction::None);
        }
        assert_eq!(idle_tick(&mut pacing), LinkAction::Stop);
    }

    #[test]
    fn watchdog_keeps_watching_a_ticking_source() {
        let mut pacing = visible();
        pacing.source_started();
        assert!(pacing.arm_watchdog());
        // Already armed: no second timer.
        assert!(!pacing.arm_watchdog());
        assert_eq!(idle_tick(&mut pacing), LinkAction::None);
        assert_eq!(pacing.watchdog(), Watchdog::Rearm(LinkAction::None));
        assert!(pacing.arm_watchdog());
        // The link stops once idle; the next check stops watching.
        while pacing.is_running() {
            let _ = idle_tick(&mut pacing);
        }
        assert_eq!(pacing.watchdog(), Watchdog::Disarm);
    }

    #[test]
    fn watchdog_recreates_a_stalled_source() {
        let mut pacing = FramePacing::new(LIMIT);
        assert_eq!(pacing.set_visible(true), LinkAction::Start);
        pacing.source_started();
        assert!(pacing.arm_watchdog());
        // The source never ticks. Requests do nothing while it is believed to run.
        assert_eq!(pacing.request(), LinkAction::None);
        assert_eq!(pacing.watchdog(), Watchdog::Recreate);
        // The replacement must tick within the next period.
        pacing.source_started();
        assert!(pacing.arm_watchdog());
        assert_eq!(pacing.watchdog(), Watchdog::Recreate);
        pacing.source_started();
        assert!(pacing.arm_watchdog());
        assert!(pacing.begin_tick());
        assert_eq!(pacing.end_tick(), LinkAction::None);
        assert_eq!(pacing.watchdog(), Watchdog::Rearm(LinkAction::None));
    }

    #[test]
    fn watchdog_gives_a_restarted_source_a_full_period() {
        let mut pacing = visible();
        pacing.source_started();
        assert!(pacing.arm_watchdog());
        while pacing.is_running() {
            let _ = idle_tick(&mut pacing);
        }
        // Restarted just before the pending check fires.
        assert_eq!(pacing.request(), LinkAction::Start);
        pacing.source_started();
        assert!(!pacing.arm_watchdog());
        assert_eq!(pacing.watchdog(), Watchdog::Rearm(LinkAction::None));
        assert!(pacing.arm_watchdog());
        // A whole period without a tick is a stall.
        assert_eq!(pacing.watchdog(), Watchdog::Recreate);
    }

    #[test]
    fn watchdog_ignores_a_frame_in_progress() {
        let mut pacing = visible();
        assert!(pacing.arm_watchdog());
        let _ = pacing.watchdog();
        assert!(pacing.begin_tick());
        // A nested run loop holds back ticks while the frame runs.
        assert_eq!(pacing.watchdog(), Watchdog::Rearm(LinkAction::None));
        assert_eq!(pacing.watchdog(), Watchdog::Rearm(LinkAction::None));
        assert_eq!(pacing.end_tick(), LinkAction::None);
    }

    #[test]
    fn watchdog_retries_a_failed_start_while_demand_waits() {
        let mut pacing = FramePacing::new(LIMIT);
        assert_eq!(pacing.set_visible(true), LinkAction::Start);
        pacing.start_failed();
        assert!(pacing.arm_watchdog());
        assert_eq!(pacing.watchdog(), Watchdog::Rearm(LinkAction::Start));
        pacing.start_failed();
        assert!(pacing.arm_watchdog());
        assert_eq!(pacing.watchdog(), Watchdog::Rearm(LinkAction::Start));
        pacing.source_started();
        assert!(pacing.arm_watchdog());
        assert!(pacing.begin_tick());
        assert_eq!(pacing.end_tick(), LinkAction::None);
        assert!(!pacing.has_pending_request());
    }

    #[test]
    fn watchdog_stops_while_hidden_or_closed() {
        let mut pacing = visible();
        assert!(pacing.arm_watchdog());
        assert_eq!(pacing.set_visible(false), LinkAction::Stop);
        assert_eq!(pacing.request(), LinkAction::None);
        assert_eq!(pacing.watchdog(), Watchdog::Disarm);

        let mut pacing = visible();
        assert!(pacing.arm_watchdog());
        assert_eq!(pacing.close(), LinkAction::Stop);
        assert_eq!(pacing.watchdog(), Watchdog::Disarm);
    }

    /// Simulates GPUI (dirty flag, next-frame callbacks, needs_present, animations) driving
    /// the pacing state through a fake display link and the platform's watchdog, with random
    /// events. The fake link can fail to start and can stall (keep "running" without ticks)
    /// until it is replaced, as the macOS glue replaces it: after a failed start, on becoming
    /// visible, and when the watchdog reports a stall. Checks that the link never runs while
    /// hidden, that a visible window with work always has a ticking link or a pending
    /// watchdog that will repair it, that the link stops and the watchdog disarms once GPUI
    /// is idle, and that no state is left undrawn while the link is stopped.
    #[test]
    fn randomized_simulation_has_no_missed_wakeups() {
        struct Sim {
            pacing: FramePacing,
            visible: bool,
            link_running: bool,
            /// The link object stopped delivering ticks; only a new link object recovers.
            link_stalled: bool,
            fail_starts: bool,
            watchdog_pending: bool,
            dirty: bool,
            callbacks: Vec<u32>,
            needs_present: bool,
            draws: u64,
            presents: u64,
        }

        impl Sim {
            // MacFramePacer::apply.
            fn apply(&mut self, action: LinkAction) {
                match action {
                    LinkAction::None => {}
                    LinkAction::Start => self.start_link(),
                    LinkAction::Stop => {
                        assert!(self.link_running, "stopped a stopped link");
                        self.link_running = false;
                    }
                }
                assert_eq!(self.link_running, self.pacing.is_running());
            }

            fn start_link(&mut self) {
                if self.fail_starts {
                    // The glue drops the link object, so the retry creates a new one.
                    self.link_stalled = false;
                    self.link_running = false;
                    self.pacing.start_failed();
                } else {
                    self.link_running = true;
                    self.pacing.source_started();
                }
                self.arm_watchdog();
            }

            fn arm_watchdog(&mut self) {
                if self.pacing.arm_watchdog() {
                    assert!(!self.watchdog_pending, "two watchdog timers");
                    self.watchdog_pending = true;
                }
            }

            fn fire_watchdog(&mut self) {
                if !self.watchdog_pending {
                    return;
                }
                self.watchdog_pending = false;
                match self.pacing.watchdog() {
                    Watchdog::Disarm => {}
                    Watchdog::Rearm(action) => {
                        self.apply(action);
                        self.arm_watchdog();
                    }
                    Watchdog::Recreate => {
                        assert!(self.link_running);
                        self.link_stalled = false;
                        self.start_link();
                        assert_eq!(self.link_running, self.pacing.is_running());
                        self.arm_watchdog();
                    }
                }
            }

            fn wake(&mut self) {
                let action = self.pacing.request();
                self.apply(action);
            }

            fn has_work(&self) -> bool {
                self.dirty || !self.callbacks.is_empty() || self.needs_present
            }

            // WindowInvalidator::set_dirty / invalidate_view: every call requests a frame.
            fn notify(&mut self) {
                self.dirty = true;
                self.wake();
            }

            // Window::on_next_frame.
            fn on_next_frame(&mut self, remaining_animation_frames: u32) {
                self.callbacks.push(remaining_animation_frames);
                self.wake();
            }

            // Window::draw outside a frame (dispatch_key_event).
            fn draw_outside_frame(&mut self) {
                if self.dirty {
                    self.dirty = false;
                    self.draws += 1;
                    self.needs_present = true;
                    self.wake();
                }
            }

            // The request_frame closure in Window::new.
            fn frame_callback(&mut self) {
                for remaining in std::mem::take(&mut self.callbacks) {
                    // request_animation_frame: notify, and keep animating while frames remain.
                    self.notify();
                    if remaining > 0 {
                        self.on_next_frame(remaining - 1);
                    }
                }
                if self.dirty {
                    self.dirty = false;
                    self.draws += 1;
                    self.needs_present = true;
                    self.wake();
                }
                if self.needs_present {
                    self.needs_present = false;
                    self.presents += 1;
                }
                // Safety net at the end of the frame.
                if self.has_work() {
                    self.wake();
                }
            }

            fn tick(&mut self) {
                if !self.link_running || self.link_stalled {
                    // A stray tick after a stop must be ignored.
                    if !self.link_running {
                        assert!(!self.pacing.begin_tick());
                    }
                    return;
                }
                if self.pacing.begin_tick() {
                    self.frame_callback();
                    let action = self.pacing.end_tick();
                    self.apply(action);
                }
            }

            fn sync_frame(&mut self) {
                if self.pacing.begin_sync_frame() {
                    self.frame_callback();
                    let action = self.pacing.end_sync_frame();
                    self.apply(action);
                }
            }

            // MacFramePacer::set_visible.
            fn set_visible(&mut self, visible: bool) {
                if visible && !self.pacing.is_visible() {
                    // The glue replaces the link object on every hidden-to-visible change.
                    self.link_stalled = false;
                }
                self.visible = visible;
                let action = self.pacing.set_visible(visible);
                self.apply(action);
            }

            fn check_invariants(&self) {
                if !self.visible {
                    assert!(!self.link_running, "link runs while hidden");
                }
                if self.link_running {
                    assert!(self.watchdog_pending, "link runs without a watchdog");
                }
                if self.visible && self.has_work() {
                    let ticking = self.link_running && !self.link_stalled;
                    assert!(
                        ticking || self.watchdog_pending,
                        "visible window has work but nothing will run a frame"
                    );
                    if !self.link_running {
                        assert!(
                            self.pacing.has_pending_request(),
                            "a failed start would not be retried"
                        );
                    }
                }
            }
        }

        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };

        for _run in 0..2_000 {
            let mut sim = Sim {
                pacing: FramePacing::new(LIMIT),
                visible: false,
                link_running: false,
                link_stalled: false,
                fail_starts: false,
                watchdog_pending: false,
                dirty: true,
                callbacks: Vec::new(),
                needs_present: false,
                draws: 0,
                presents: 0,
            };
            for _step in 0..300 {
                match next() % 16 {
                    0..=4 => sim.tick(),
                    5 | 6 => sim.notify(),
                    7 => sim.on_next_frame((next() % 5) as u32),
                    8 => sim.draw_outside_frame(),
                    9 => sim.sync_frame(),
                    10 => {
                        let visible = next() % 4 != 0;
                        sim.set_visible(visible);
                    }
                    11 => {
                        if sim.link_running && next() % 3 == 0 {
                            sim.link_stalled = true;
                        }
                    }
                    12 => sim.fail_starts = next() % 3 == 0,
                    13 => sim.fire_watchdog(),
                    _ => sim.tick(),
                }
                sim.check_invariants();
            }
            // Quiesce: no more GPUI events, only ticks and watchdog timers. GPUI must become
            // idle, the link stop and the watchdog disarm.
            sim.fail_starts = false;
            sim.set_visible(true);
            sim.check_invariants();
            let mut steps = 0;
            while sim.link_running || sim.watchdog_pending {
                for _ in 0..LIMIT * 2 {
                    sim.tick();
                    sim.check_invariants();
                }
                sim.fire_watchdog();
                sim.check_invariants();
                steps += 1;
                assert!(steps < 1_000, "link or watchdog never stopped");
            }
            assert!(!sim.has_work(), "link stopped with work left");
            assert!(!sim.pacing.has_pending_request());
            assert!(sim.presents <= sim.draws + 1);
        }
    }
}
