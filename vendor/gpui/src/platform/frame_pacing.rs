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
//! see [`FramePacing::watchdog`]. The watchdog gives up after [`MAX_SOURCE_FAILURES`] attempts
//! in a row, so a source that cannot work (for example on a sleeping display) is not replaced
//! forever; [`FramePacing::retry`] and becoming visible start over.
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
    /// The vsync source still delivers no ticks after [`MAX_SOURCE_FAILURES`] replacements or
    /// retried starts in a row. Drop it (stopping it if it runs) and stop watching. Demand is
    /// kept, but only [`FramePacing::retry`] or becoming visible start a source again.
    GiveUp,
}

/// Watchdog checks in a row that may replace a stalled vsync source or retry a failed start
/// before the watchdog gives up. A tick, becoming visible, or [`FramePacing::retry`] after
/// giving up starts a new count.
pub(crate) const MAX_SOURCE_FAILURES: u32 = 3;

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
    /// Watchdog checks since the last tick that found the source stalled or retried a failed
    /// start.
    failures: u32,
    /// A start failed and the next watchdog check retries it. Until then requests only record
    /// demand, so a source that cannot start is not retried on every request.
    retry_pending: bool,
    /// The watchdog gave up on the source. Requests only record demand until [`Self::retry`]
    /// or the window becomes visible.
    gave_up: bool,
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
            failures: 0,
            retry_pending: false,
            gave_up: false,
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

    /// Watchdog checks in a row that found the source stalled or retried a failed start
    /// (at most [`MAX_SOURCE_FAILURES`]). The platform backs off the watchdog period by it.
    #[allow(dead_code)]
    pub(crate) fn failures(&self) -> u32 {
        self.failures
    }

    /// Whether the watchdog gave up on the vsync source. See [`Watchdog::GiveUp`].
    #[allow(dead_code)]
    pub(crate) fn has_given_up(&self) -> bool {
        self.gave_up
    }

    /// GPUI needs a frame: the window became dirty, queued a next-frame callback, or has a
    /// scene that was drawn but not presented. Safe to call at any time on the main thread,
    /// including from inside a frame callback. Demand received while the window is hidden,
    /// while a failed start waits for the watchdog, or after the watchdog gave up is kept and
    /// served once a source runs.
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
        self.failures = 0;
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

    /// Reports the window's visibility (for macOS, its occlusion state, and hidden while the
    /// displays sleep). Hiding stops the vsync source. Showing counts as demand and starts the
    /// source until the window is idle: demand recorded while hidden is served, the platform
    /// can present the current scene again, and a failed start is retried by the watchdog.
    /// Showing also forgets earlier failures; the platform starts a new source for it.
    pub(crate) fn set_visible(&mut self, visible: bool) -> LinkAction {
        if self.closed || visible == self.visible {
            return LinkAction::None;
        }
        self.visible = visible;
        if visible {
            self.pending = true;
            self.idle_ticks = 0;
            self.failures = 0;
            self.retry_pending = false;
            self.gave_up = false;
            self.start_if_visible()
        } else {
            self.stop()
        }
    }

    /// The platform could not start its vsync source. The next watchdog check retries;
    /// requests until then only record demand.
    pub(crate) fn start_failed(&mut self) {
        self.running = false;
        self.retry_pending = true;
    }

    /// Something happened that may have repaired a vsync source the watchdog gave up on: the
    /// displays woke, the display configuration changed, or the window moved to another
    /// display, became key or received input. Forgets the failures and starts a new source if
    /// demand waits. Does nothing unless the watchdog gave up.
    pub(crate) fn retry(&mut self) -> LinkAction {
        if !self.gave_up {
            return LinkAction::None;
        }
        self.gave_up = false;
        self.failures = 0;
        if self.pending {
            self.start_if_visible()
        } else {
            LinkAction::None
        }
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
    /// replaced. If a start failed while demand is waiting, this retries it. After
    /// [`MAX_SOURCE_FAILURES`] such checks without a tick it gives up instead. Otherwise the
    /// watchdog stops, so an idle window has no timer.
    pub(crate) fn watchdog(&mut self) -> Watchdog {
        self.watchdog_armed = false;
        self.retry_pending = false;
        let ticked = std::mem::take(&mut self.ticked);
        if self.closed || !self.visible {
            Watchdog::Disarm
        } else if self.running {
            if ticked || self.in_frame {
                Watchdog::Rearm(LinkAction::None)
            } else if self.count_failure() {
                Watchdog::Recreate
            } else {
                self.running = false;
                Watchdog::GiveUp
            }
        } else if self.pending {
            if self.count_failure() {
                Watchdog::Rearm(self.start_if_visible())
            } else {
                Watchdog::GiveUp
            }
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

    /// Counts a watchdog check that found the source broken. Returns `false`, and gives up,
    /// if the source already failed [`MAX_SOURCE_FAILURES`] checks in a row.
    fn count_failure(&mut self) -> bool {
        if self.failures < MAX_SOURCE_FAILURES {
            self.failures += 1;
            true
        } else {
            self.gave_up = true;
            false
        }
    }

    /// Starts the source if it should run and nothing holds back a start: a failed start
    /// waits for the watchdog, and after giving up for [`Self::retry`].
    fn start_if_visible(&mut self) -> LinkAction {
        if self.visible && !self.running && !self.closed && !self.retry_pending && !self.gave_up {
            self.running = true;
            LinkAction::Start
        } else {
            LinkAction::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FramePacing, LinkAction, MAX_SOURCE_FAILURES, Watchdog};

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
    fn failed_start_is_retried_by_the_watchdog_only() {
        let mut pacing = FramePacing::new(LIMIT);
        assert_eq!(pacing.set_visible(true), LinkAction::Start);
        pacing.start_failed();
        assert!(pacing.arm_watchdog());
        // Every invalidation requests a frame; none of them may build another link.
        for _ in 0..10 {
            assert_eq!(pacing.request(), LinkAction::None);
        }
        assert!(pacing.begin_sync_frame());
        assert_eq!(pacing.request(), LinkAction::None);
        assert_eq!(pacing.end_sync_frame(), LinkAction::None);
        assert_eq!(pacing.watchdog(), Watchdog::Rearm(LinkAction::Start));
        pacing.source_started();
        assert!(pacing.arm_watchdog());
        assert!(pacing.begin_tick());
        assert_eq!(pacing.end_tick(), LinkAction::None);
        assert_eq!(pacing.failures(), 0);
    }

    #[test]
    fn failed_start_without_demand_lets_the_next_request_start() {
        let mut pacing = FramePacing::new(LIMIT);
        assert_eq!(pacing.set_visible(true), LinkAction::Start);
        pacing.start_failed();
        assert!(pacing.arm_watchdog());
        // A synchronous frame (displayLayer:) serves the demand before the check.
        assert!(pacing.begin_sync_frame());
        assert_eq!(pacing.end_sync_frame(), LinkAction::None);
        assert_eq!(pacing.watchdog(), Watchdog::Disarm);
        assert_eq!(pacing.request(), LinkAction::Start);
    }

    #[test]
    fn watchdog_gives_up_on_a_source_that_never_ticks() {
        let mut pacing = FramePacing::new(LIMIT);
        assert_eq!(pacing.set_visible(true), LinkAction::Start);
        pacing.source_started();
        assert!(pacing.arm_watchdog());
        for failures in 1..=MAX_SOURCE_FAILURES {
            assert_eq!(pacing.request(), LinkAction::None);
            assert_eq!(pacing.watchdog(), Watchdog::Recreate);
            assert_eq!(pacing.failures(), failures);
            pacing.source_started();
            assert!(pacing.arm_watchdog());
        }
        assert_eq!(pacing.watchdog(), Watchdog::GiveUp);
        assert!(pacing.has_given_up());
        assert!(!pacing.is_running());
        assert!(pacing.has_pending_request());
        // Pages keep notifying while the display sleeps: no new link, no timer.
        for _ in 0..10 {
            assert_eq!(pacing.request(), LinkAction::None);
            assert!(pacing.begin_sync_frame());
            assert_eq!(pacing.end_sync_frame(), LinkAction::None);
        }
        assert_eq!(pacing.request(), LinkAction::None);
        // An event that may have repaired it starts over with a full budget.
        assert_eq!(pacing.retry(), LinkAction::Start);
        assert!(!pacing.has_given_up());
        assert_eq!(pacing.failures(), 0);
        pacing.source_started();
        assert!(pacing.arm_watchdog());
        assert_eq!(pacing.watchdog(), Watchdog::Recreate);
    }

    #[test]
    fn watchdog_gives_up_on_starts_that_keep_failing() {
        let mut pacing = FramePacing::new(LIMIT);
        assert_eq!(pacing.set_visible(true), LinkAction::Start);
        pacing.start_failed();
        assert!(pacing.arm_watchdog());
        for _ in 0..MAX_SOURCE_FAILURES {
            assert_eq!(pacing.watchdog(), Watchdog::Rearm(LinkAction::Start));
            pacing.start_failed();
            assert!(pacing.arm_watchdog());
            assert_eq!(pacing.request(), LinkAction::None);
        }
        assert_eq!(pacing.watchdog(), Watchdog::GiveUp);
        assert_eq!(pacing.request(), LinkAction::None);
        assert_eq!(pacing.retry(), LinkAction::Start);
    }

    #[test]
    fn showing_again_forgets_failures() {
        let mut pacing = FramePacing::new(LIMIT);
        assert_eq!(pacing.set_visible(true), LinkAction::Start);
        pacing.start_failed();
        assert!(pacing.arm_watchdog());
        // Hidden and shown before the watchdog fires: the start is retried right away.
        assert_eq!(pacing.set_visible(false), LinkAction::None);
        assert_eq!(pacing.set_visible(true), LinkAction::Start);
        pacing.source_started();
        // The new link never ticks either.
        let mut checks = 0;
        while pacing.watchdog() != Watchdog::GiveUp {
            pacing.source_started();
            assert!(pacing.arm_watchdog());
            checks += 1;
            assert!(
                checks <= MAX_SOURCE_FAILURES + 1,
                "the watchdog never gave up"
            );
        }
        assert!(pacing.has_given_up());
        assert_eq!(pacing.set_visible(false), LinkAction::None);
        assert_eq!(pacing.request(), LinkAction::None);
        assert_eq!(pacing.set_visible(true), LinkAction::Start);
        assert!(!pacing.has_given_up());
        assert_eq!(pacing.failures(), 0);
    }

    #[test]
    fn a_tick_resets_the_failure_count() {
        let mut pacing = FramePacing::new(LIMIT);
        assert_eq!(pacing.set_visible(true), LinkAction::Start);
        pacing.source_started();
        assert!(pacing.arm_watchdog());
        for _ in 0..MAX_SOURCE_FAILURES {
            assert_eq!(pacing.watchdog(), Watchdog::Recreate);
            pacing.source_started();
            assert!(pacing.arm_watchdog());
        }
        // The last replacement works.
        assert!(pacing.begin_tick());
        assert_eq!(pacing.failures(), 0);
        assert_eq!(pacing.end_tick(), LinkAction::None);
        assert_eq!(pacing.watchdog(), Watchdog::Rearm(LinkAction::None));
        assert!(pacing.arm_watchdog());
        // A later stall gets the full budget again.
        assert_eq!(pacing.watchdog(), Watchdog::Recreate);
        assert_eq!(pacing.failures(), 1);
    }

    #[test]
    fn retry_does_nothing_unless_the_watchdog_gave_up() {
        let mut pacing = visible();
        assert_eq!(pacing.retry(), LinkAction::None);
        while pacing.is_running() {
            let _ = idle_tick(&mut pacing);
        }
        assert_eq!(pacing.retry(), LinkAction::None);
        assert!(!pacing.is_running());

        // A failed start waits for the watchdog, not for the next input.
        let mut pacing = FramePacing::new(LIMIT);
        assert_eq!(pacing.set_visible(true), LinkAction::Start);
        pacing.start_failed();
        assert_eq!(pacing.retry(), LinkAction::None);
    }

    #[test]
    fn retry_without_demand_lets_the_next_request_start() {
        let mut pacing = FramePacing::new(LIMIT);
        assert_eq!(pacing.set_visible(true), LinkAction::Start);
        pacing.source_started();
        assert!(pacing.arm_watchdog());
        // The demand is served by synchronous frames while the link is stalled.
        assert!(pacing.begin_sync_frame());
        assert_eq!(pacing.end_sync_frame(), LinkAction::None);
        for _ in 0..MAX_SOURCE_FAILURES {
            assert_eq!(pacing.watchdog(), Watchdog::Recreate);
            pacing.source_started();
            assert!(pacing.arm_watchdog());
        }
        assert_eq!(pacing.watchdog(), Watchdog::GiveUp);
        assert!(!pacing.has_pending_request());
        assert_eq!(pacing.retry(), LinkAction::None);
        assert_eq!(pacing.request(), LinkAction::Start);
    }

    #[test]
    fn display_sleep_hides_and_wake_starts_a_new_link() {
        let mut pacing = visible();
        assert!(pacing.arm_watchdog());
        // The displays sleep while the link runs: the platform reports the window hidden.
        assert_eq!(pacing.set_visible(false), LinkAction::Stop);
        // Pages keep notifying: demand is recorded, nothing starts, the watchdog disarms.
        for _ in 0..10 {
            assert_eq!(pacing.request(), LinkAction::None);
        }
        assert_eq!(pacing.watchdog(), Watchdog::Disarm);
        assert_eq!(pacing.request(), LinkAction::None);
        assert!(!pacing.is_running());
        // Waking shows the window again and serves the demand.
        assert_eq!(pacing.set_visible(true), LinkAction::Start);
        pacing.source_started();
        assert!(pacing.arm_watchdog());
        assert!(pacing.begin_tick());
        assert_eq!(pacing.end_tick(), LinkAction::None);
        assert!(!pacing.has_pending_request());
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
    /// events. The fake link can fail to start, can stall (keep "running" without ticks) until
    /// it is replaced, and can be broken (every new link stalls, like a link on a display that
    /// sleeps unnoticed). It is replaced as the macOS glue replaces it: after a failed start,
    /// on becoming visible, when the window changes display, when the watchdog reports a
    /// stall, and on a retry after the watchdog gave up.
    ///
    /// Checks that the link never runs while hidden; that a visible window with work always
    /// has a ticking link, a pending watchdog that will repair it, or a watchdog that gave up
    /// and waits for a retry; that requests never rebuild a link while a failed start waits for
    /// the watchdog or after it gave up; that the watchdog builds at most
    /// [`MAX_SOURCE_FAILURES`] links in a row and then stops while pages keep notifying; that
    /// the link stops and the watchdog disarms once GPUI is idle; and that no state is left
    /// undrawn while the link is stopped.
    #[test]
    fn randomized_simulation_has_no_missed_wakeups() {
        struct Sim {
            pacing: FramePacing,
            visible: bool,
            /// The glue holds a link object (`MacFramePacer::display_link`).
            link_exists: bool,
            link_running: bool,
            /// The link object stopped delivering ticks; only a new link object recovers.
            link_stalled: bool,
            /// Every new link object stalls.
            broken: bool,
            fail_starts: bool,
            watchdog_pending: bool,
            /// A start failed and the watchdog has not fired since.
            start_failed: bool,
            /// Links the watchdog built since the last tick or the last reset of its budget.
            watchdog_links: u32,
            recreates: u64,
            retried_starts: u64,
            give_ups: u64,
            held_back_requests: u64,
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

            // MacFramePacer::start_link and the failure branch of apply.
            fn start_link(&mut self) {
                if !self.link_exists {
                    self.link_exists = true;
                    self.link_stalled = self.broken;
                }
                if self.fail_starts {
                    // The glue drops the link object, so the retry creates a new one.
                    self.drop_link();
                    self.start_failed = true;
                    self.pacing.start_failed();
                } else {
                    self.link_running = true;
                    self.pacing.source_started();
                }
                self.arm_watchdog();
            }

            // MacFramePacer::drop_link.
            fn drop_link(&mut self) {
                self.link_exists = false;
                self.link_running = false;
                self.link_stalled = false;
            }

            fn arm_watchdog(&mut self) {
                if self.pacing.arm_watchdog() {
                    assert!(!self.watchdog_pending, "two watchdog timers");
                    self.watchdog_pending = true;
                }
            }

            // MacFramePacer::watchdog.
            fn fire_watchdog(&mut self) {
                if !self.watchdog_pending {
                    return;
                }
                self.watchdog_pending = false;
                self.start_failed = false;
                match self.pacing.watchdog() {
                    Watchdog::Disarm => {}
                    Watchdog::Rearm(action) => {
                        if action == LinkAction::Start {
                            self.watchdog_links += 1;
                            self.retried_starts += 1;
                        }
                        self.apply(action);
                        self.arm_watchdog();
                    }
                    Watchdog::Recreate => {
                        assert!(self.link_running);
                        self.watchdog_links += 1;
                        self.recreates += 1;
                        self.drop_link();
                        self.apply(LinkAction::Start);
                        self.arm_watchdog();
                    }
                    Watchdog::GiveUp => {
                        self.drop_link();
                        assert!(!self.pacing.is_running());
                        self.give_ups += 1;
                    }
                }
                assert!(
                    self.watchdog_links <= MAX_SOURCE_FAILURES,
                    "the watchdog keeps replacing a link that does not work"
                );
            }

            // MacFramePacer::retry: the displays woke, the display configuration changed, or
            // the window became key or received input.
            fn retry(&mut self) {
                if self.pacing.has_given_up() {
                    self.watchdog_links = 0;
                }
                let action = self.pacing.retry();
                self.apply(action);
            }

            // MacFramePacer::display_changed.
            fn display_changed(&mut self) {
                self.drop_link();
                if self.pacing.is_running() {
                    self.apply(LinkAction::Start);
                }
                self.retry();
                self.wake();
            }

            fn held_back(&self) -> bool {
                self.start_failed || self.pacing.has_given_up()
            }

            fn wake(&mut self) {
                let held_back = self.held_back();
                let action = self.pacing.request();
                if held_back {
                    assert_eq!(action, LinkAction::None, "a request rebuilt a broken link");
                    self.held_back_requests += 1;
                }
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
                    self.watchdog_links = 0;
                    self.frame_callback();
                    let action = self.pacing.end_tick();
                    self.apply(action);
                }
            }

            fn sync_frame(&mut self) {
                if self.pacing.begin_sync_frame() {
                    self.frame_callback();
                    let held_back = self.held_back();
                    let action = self.pacing.end_sync_frame();
                    if held_back {
                        assert_eq!(action, LinkAction::None, "a frame rebuilt a broken link");
                    }
                    self.apply(action);
                }
            }

            // MacFramePacer::set_visible.
            fn set_visible(&mut self, visible: bool) {
                if visible && !self.pacing.is_visible() {
                    // The glue replaces the link object on every hidden-to-visible change.
                    assert!(!self.link_running);
                    self.drop_link();
                    self.start_failed = false;
                    self.watchdog_links = 0;
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
                if self.pacing.has_given_up() {
                    assert!(
                        !self.link_running && !self.watchdog_pending,
                        "gave up but still runs or watches the link"
                    );
                }
                if self.visible && self.has_work() {
                    let ticking = self.link_running && !self.link_stalled;
                    assert!(
                        ticking || self.watchdog_pending || self.pacing.has_given_up(),
                        "visible window has work but nothing will run a frame"
                    );
                    if !self.link_running {
                        assert!(self.pacing.has_pending_request(), "demand was lost");
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

        let (mut recreates, mut retried_starts, mut random_give_ups, mut held_back) = (0, 0, 0, 0);
        for _run in 0..2_000 {
            let mut sim = Sim {
                pacing: FramePacing::new(LIMIT),
                visible: false,
                link_exists: false,
                link_running: false,
                link_stalled: false,
                broken: false,
                fail_starts: false,
                watchdog_pending: false,
                start_failed: false,
                watchdog_links: 0,
                recreates: 0,
                retried_starts: 0,
                give_ups: 0,
                held_back_requests: 0,
                dirty: true,
                callbacks: Vec::new(),
                needs_present: false,
                draws: 0,
                presents: 0,
            };
            for _step in 0..300 {
                match next() % 20 {
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
                    13 | 14 => sim.fire_watchdog(),
                    15 => sim.broken = next() % 4 == 0,
                    16 => sim.retry(),
                    17 => sim.display_changed(),
                    _ => sim.tick(),
                }
                sim.check_invariants();
            }
            random_give_ups += sim.give_ups;

            // The displays sleep without the window noticing: the link stops ticking and every
            // new one stalls or fails to start, while pages keep notifying. The watchdog must
            // stop building links and stop firing.
            sim.broken = true;
            sim.link_stalled = sim.link_exists;
            sim.fail_starts = next() % 2 == 0;
            sim.set_visible(true);
            let mut steps = 0;
            loop {
                sim.notify();
                sim.tick();
                sim.fire_watchdog();
                sim.check_invariants();
                if !sim.link_running && !sim.watchdog_pending {
                    break;
                }
                steps += 1;
                assert!(steps < 100, "the watchdog never gave up");
            }
            assert!(sim.pacing.has_given_up());
            for _ in 0..10 {
                sim.notify();
                sim.sync_frame();
                sim.notify();
                sim.tick();
                sim.check_invariants();
                assert!(!sim.link_running && !sim.watchdog_pending);
            }

            // The displays wake: the platform retries with a new link that works.
            sim.broken = false;
            sim.fail_starts = false;
            sim.retry();
            assert!(sim.link_running);

            // Quiesce: no more GPUI events, only ticks and watchdog timers. GPUI must become
            // idle, the link stop and the watchdog disarm.
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
            recreates += sim.recreates;
            retried_starts += sim.retried_starts;
            held_back += sim.held_back_requests;
        }
        // The random phase exercised every repair path.
        assert!(recreates > 0 && retried_starts > 0 && random_give_ups > 0 && held_back > 0);
    }
}
