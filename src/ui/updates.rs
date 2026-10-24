//! Updates as the browser shows them. Map Browser checks a minute after it
//! starts and every six hours after that, and downloads a newer version
//! without asking. Once one is ready, a card in the sidebar says so, with
//! Restart; Map Browser > Restart to Update and the command bar offer it
//! too, and quitting installs it anyway. Check for Updates… checks at once
//! and says what it found in toasts; checks of its own fail quietly. The
//! work is in `crate::updater`, on a thread of its own.

use super::{downloads::request_quit, elide::Cut, toast_window::ToastKind};
use crate::{app::BrowserApp, updater};
use gpui::{Context, SharedString, Task, Window};
use semver::Version;
use std::time::Duration;

const FIRST_CHECK: Duration = Duration::from_secs(60);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// What the menus and the command bar offer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum UpdateOffer {
    /// This copy doesn't update itself.
    #[default]
    None,
    Check,
    /// The version ready to install.
    Restart(String),
}

#[derive(Default)]
pub struct Updates {
    /// The version that installs when Map Browser quits.
    ready: Option<Version>,
    /// The card's Later: it stays away until a newer update is ready. The
    /// update still installs on quit.
    dismissed: bool,
    running: Option<Running>,
    /// Restart to Update asked for the quit: the app opens again after it.
    pub(crate) relaunch: bool,
    schedule: Option<Task<()>>,
}

/// A check, and the download of what it found.
struct Running {
    /// Check for Updates… asked for it: toasts say what it finds.
    manual: bool,
    /// The version downloading, once the check has found one.
    downloading: Option<Version>,
    _task: Task<()>,
}

impl Updates {
    pub fn offer(&self) -> UpdateOffer {
        match &self.ready {
            _ if !updater::enabled() => UpdateOffer::None,
            Some(version) => UpdateOffer::Restart(version.to_string()),
            None => UpdateOffer::Check,
        }
    }
}

impl BrowserApp {
    /// Checks a minute after launch, then every six hours.
    pub(crate) fn schedule_update_checks(&mut self, cx: &mut Context<Self>) {
        if !updater::enabled() {
            return;
        }
        self.updates.schedule = Some(cx.spawn(async move |this, cx| {
            let mut delay = FIRST_CHECK;
            loop {
                cx.background_executor().timer(delay).await;
                if this
                    .update(cx, |this, cx| this.check_for_updates(false, cx))
                    .is_err()
                {
                    break;
                }
                delay = CHECK_EVERY;
            }
        }));
    }

    /// Looks for a version newer than this one, or than the one ready, and
    /// downloads it. `manual`: Check for Updates…, which reports back.
    pub fn check_for_updates(&mut self, manual: bool, cx: &mut Context<Self>) {
        if !updater::enabled() {
            return;
        }
        if let Some(running) = &mut self.updates.running {
            if manual {
                running.manual = true;
                match running.downloading.clone() {
                    Some(version) => self.tell_downloading(&version, cx),
                    None => self.show_toast_kind(ToastKind::Info, "Checking for updates…", cx),
                }
            }
            return;
        }
        let newer_than = self.updates.ready.clone().unwrap_or_else(updater::current);
        let (sender, receiver) = async_channel::unbounded();
        updater::start(newer_than, sender);
        let task = cx.spawn(async move |this, cx| {
            while let Ok(progress) = receiver.recv().await {
                if this
                    .update(cx, |this, cx| this.update_progress(progress, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        self.updates.running = Some(Running {
            manual,
            downloading: None,
            _task: task,
        });
        if manual {
            self.show_toast_kind(ToastKind::Info, "Checking for updates…", cx);
        }
    }

    fn update_progress(&mut self, progress: updater::Progress, cx: &mut Context<Self>) {
        let Some(running) = &mut self.updates.running else {
            return;
        };
        let manual = running.manual;
        match progress {
            updater::Progress::Downloading(version) => {
                running.downloading = Some(version.clone());
                if manual {
                    self.tell_downloading(&version, cx);
                }
                return;
            }
            updater::Progress::UpToDate if manual => match self.updates.ready.clone() {
                // A card dismissed with Later comes back when asked.
                Some(version) => self.tell_ready(version, cx),
                None => self.show_toast("Map Browser is up to date", cx),
            },
            updater::Progress::UpToDate => {}
            updater::Progress::Ready(version) if manual => self.tell_ready(version, cx),
            updater::Progress::Ready(version) => {
                self.updates.ready = Some(version);
                self.updates.dismissed = false;
            }
            updater::Progress::Failed(failure) => {
                eprintln!("Couldn't update Map Browser: {failure}");
                match failure {
                    _ if !manual => {}
                    updater::Failure::Location => self.show_toast_about(
                        ToastKind::Info,
                        "To update, move it to ",
                        "Applications",
                        Cut::End,
                        cx,
                    ),
                    failure => self.show_toast_about(
                        ToastKind::Failed,
                        "Can't update: ",
                        &failure.to_string(),
                        Cut::End,
                        cx,
                    ),
                }
            }
        }
        self.updates.running = None;
        cx.notify();
    }

    fn tell_downloading(&mut self, version: &Version, cx: &mut Context<Self>) {
        let subject = format!("Map Browser {version}…");
        self.show_toast_about(ToastKind::Info, "Downloading ", &subject, Cut::End, cx);
    }

    /// The card says so, in the sidebar; with the sidebar hidden, a toast
    /// over the page does.
    fn tell_ready(&mut self, version: Version, cx: &mut Context<Self>) {
        if self.sidebar_width().is_some() {
            self.toast = None;
        } else {
            let subject = format!("Map Browser {version} · Restart to update");
            self.show_toast_about(ToastKind::Info, "Ready: ", &subject, Cut::Middle, cx);
        }
        self.updates.ready = Some(version);
        self.updates.dismissed = false;
    }

    /// The sidebar's card: shown from when an update is ready until Later.
    pub(crate) fn update_ready(&self) -> Option<SharedString> {
        let version = self.updates.ready.as_ref()?;
        (!self.updates.dismissed).then(|| version.to_string().into())
    }

    pub(crate) fn dismiss_update(&mut self, cx: &mut Context<Self>) {
        self.updates.dismissed = true;
        cx.notify();
    }

    /// Quits, installs the update and opens the new version. Quitting asks
    /// first while files download, as ⌘Q does; Cancel there keeps the app
    /// running and forgets the restart.
    pub fn restart_to_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.updates.ready.is_none() {
            return;
        }
        self.updates.relaunch = true;
        let Some(handle) = window.window_handle().downcast::<BrowserApp>() else {
            return;
        };
        cx.defer(move |cx| request_quit(handle, cx));
    }
}
