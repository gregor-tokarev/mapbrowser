//! This session's downloads (kept in memory only): the progress ring on the
//! library button, the library's Downloads section, the "Downloaded"
//! toast, and the question before quitting cancels unfinished ones. The
//! ring moves only when the engine reports progress.

use super::{
    icons::{Icon, icon},
    shadow,
    theme::Theme,
};
use crate::{
    app::BrowserApp,
    model::{Pin, TabId},
    native,
};
use gpui::{
    App, Bounds, Context, Div, Hsla, ParentElement, PathBuilder, Pixels, PromptLevel, SharedString,
    Styled, Window, WindowHandle, canvas, div, point, px,
};
use std::{f32::consts::TAU, path::PathBuf};

/// Downloads the library lists, newest first.
const LIBRARY_LIMIT: usize = 5;
/// The badge over the library button's top-right corner (arc-design.md,
/// image 20), in bottom bar coordinates.
const BADGE_SIZE: f32 = 18.;
const BADGE_LEFT: f32 = 24.5;
const BADGE_TOP: f32 = 6.;
const RING_WIDTH: f32 = 1.75;
/// Without a size, the ring shows a quarter arc that steps on by an
/// eighth of a turn for every this many bytes.
const UNKNOWN_SIZE_STEP: u64 = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadState {
    Running,
    Done,
    Failed,
}

pub struct Download {
    pub id: u64,
    pub tab_id: TabId,
    pub name: SharedString,
    pub path: PathBuf,
    pub received: u64,
    pub total: Option<u64>,
    pub state: DownloadState,
}

impl Download {
    /// What the library shows beside the name: progress, or a failure.
    pub fn status(&self) -> Option<String> {
        match self.state {
            DownloadState::Running => Some(match self.total {
                Some(total) => format!("{}%", (self.received * 100 / total.max(1)).min(100)),
                None => file_size(self.received),
            }),
            DownloadState::Done => None,
            DownloadState::Failed => Some("Failed".into()),
        }
    }
}

#[derive(Default)]
pub struct Downloads {
    /// Newest first.
    items: Vec<Download>,
    /// Whether the question before quitting is on screen.
    asking_to_quit: bool,
}

/// The ring: `start` and `length` in turns, clockwise from 12 o'clock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RingArc {
    pub start: f32,
    pub length: f32,
}

impl Downloads {
    pub fn start(&mut self, id: u64, tab_id: TabId, name: String, path: PathBuf) {
        self.items.insert(
            0,
            Download {
                id,
                tab_id,
                name: name.into(),
                path,
                received: 0,
                total: None,
                state: DownloadState::Running,
            },
        );
    }

    fn get_mut(&mut self, id: u64) -> Option<&mut Download> {
        self.items.iter_mut().find(|download| download.id == id)
    }

    pub fn progress(&mut self, id: u64, received: u64, total: Option<u64>) -> bool {
        let Some(download) = self.get_mut(id) else {
            return false;
        };
        download.received = received;
        download.total = total;
        true
    }

    pub fn finish(&mut self, id: u64, ok: bool) -> Option<&Download> {
        let download = self.get_mut(id)?;
        download.state = if ok {
            DownloadState::Done
        } else {
            DownloadState::Failed
        };
        Some(download)
    }

    pub fn get(&self, id: u64) -> Option<&Download> {
        self.items.iter().find(|download| download.id == id)
    }

    /// Whether a download started in the tab still runs: the tab's
    /// browser must stay open until it ends, or its progress goes unheard.
    pub fn running_in(&self, tab_id: TabId) -> bool {
        self.running().any(|download| download.tab_id == tab_id)
    }

    pub fn recent(&self) -> impl Iterator<Item = &Download> {
        self.items.iter().take(LIBRARY_LIMIT)
    }

    fn running(&self) -> impl Iterator<Item = &Download> {
        self.items
            .iter()
            .filter(|download| download.state == DownloadState::Running)
    }

    /// All running downloads together, or `None` when none runs.
    pub fn ring(&self) -> Option<RingArc> {
        let (mut received, mut total, mut sized) = (0, 0, true);
        let mut any = false;
        for download in self.running() {
            any = true;
            received += download.received;
            match download.total {
                Some(size) => total += size,
                None => sized = false,
            }
        }
        if !any {
            return None;
        }
        Some(if sized && total > 0 {
            RingArc {
                start: 0.,
                length: (received as f32 / total as f32).clamp(0.02, 1.),
            }
        } else {
            RingArc {
                start: (received / UNKNOWN_SIZE_STEP % 8) as f32 / 8.,
                length: 0.25,
            }
        })
    }
}

/// ⌘Q, Map > Quit Map and the window's close button: quits, but asks first
/// while files are downloading, as quitting cancels them. Quitting from
/// outside the app (the Dock's Quit, logging out) doesn't ask.
pub fn request_quit(window: WindowHandle<BrowserApp>, cx: &mut App) {
    let asking = window.update(cx, |app, window, cx| app.ask_before_quitting(window, cx));
    if !asking.unwrap_or(false) {
        cx.quit();
    }
}

/// Sizes as Finder shows them, in decimal units.
fn file_size(bytes: u64) -> String {
    match bytes {
        0..999_000 => format!("{} KB", bytes.div_ceil(1000)),
        999_000..999_950_000 => format!("{:.1} MB", bytes as f64 / 1e6),
        _ => format!("{:.1} GB", bytes as f64 / 1e9),
    }
}

impl BrowserApp {
    pub(crate) fn download_started(
        &mut self,
        id: u64,
        tab_id: TabId,
        name: String,
        path: PathBuf,
        blank_tab: bool,
        cx: &mut Context<Self>,
    ) {
        // Without the sidebar's ring, nothing would show it started.
        if self.sidebar_width().is_none() {
            self.show_toast_kind(
                super::toast_window::ToastKind::Info,
                format!("Downloading {name}"),
                cx,
            );
        }
        self.downloads.start(id, tab_id, name, path);
        // A link that opened a new tab for a file leaves it blank: close it
        // (it isn't worth reopening) and go back to the page with the link,
        // in its own space: the window stays in the space on screen. Its
        // browser stays until the download ends.
        if blank_tab
            && let Some(space) = self
                .state
                .space_of_tab(tab_id)
                .and_then(|id| self.state.space(id))
            && space
                .tabs
                .iter()
                .any(|tab| tab.id == tab_id && tab.pin == Pin::Today)
        {
            let (space_id, selected) = (space.id, space.selected_tab_id == Some(tab_id));
            if let Some(opener) = self.openers.remove(&tab_id)
                && selected
                && self.state.space_of_tab(opener) == Some(space_id)
            {
                self.state.select_tab_in_space(space_id, opener);
            }
            self.remove_tab(tab_id, cx);
            self.state.forget_closed(tab_id);
        }
        if self.downloads_shown() {
            cx.notify();
        }
    }

    /// Whether downloads are on screen: the ring, or the library.
    fn downloads_shown(&self) -> bool {
        self.sidebar_width().is_some() || self.menu.is_some()
    }

    pub(crate) fn download_progress(
        &mut self,
        id: u64,
        received: u64,
        total: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        if self.downloads.progress(id, received, total) && self.downloads_shown() {
            cx.notify();
        }
    }

    pub(crate) fn download_finished(&mut self, id: u64, ok: bool, cx: &mut Context<Self>) {
        let Some(download) = self.downloads.finish(id, ok) else {
            return;
        };
        let (name, tab_id) = (download.name.clone(), download.tab_id);
        // A tab closed or unloaded during the download kept its browser.
        if !self.downloads.running_in(tab_id)
            && !self.is_loaded(tab_id)
            && self.engine.borrow().has_tab(tab_id)
        {
            self.engine.borrow_mut().close_tab(tab_id);
        }
        if ok {
            self.show_toast(format!("Downloaded {name}"), cx);
        } else if self.downloads_shown() {
            cx.notify();
        }
    }

    /// Asks whether to quit while downloads run (a sheet on macOS), and
    /// quits if so. Returns false when there is nothing to ask.
    fn ask_before_quitting(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let running = self.downloads.running().count();
        if running == 0 {
            return false;
        }
        // Asked once: quitting again while it shows does nothing more.
        if std::mem::replace(&mut self.downloads.asking_to_quit, true) {
            return true;
        }
        let detail = if running == 1 {
            "Quitting cancels the download that hasn't finished.".to_owned()
        } else {
            format!("Quitting cancels the {running} downloads that haven't finished.")
        };
        window.activate_window();
        let answer = window.prompt(
            PromptLevel::Warning,
            "Downloads in progress — Quit anyway?",
            Some(&detail),
            &["Quit", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            let quit = answer.await == Ok(0);
            let _ = this.update(cx, |this, cx| {
                this.downloads.asking_to_quit = false;
                if quit {
                    cx.quit();
                }
            });
        })
        .detach();
        true
    }

    /// Shows a download in Finder, selected.
    pub fn reveal_download(&self, id: u64) {
        if let Some(download) = self.downloads.get(id) {
            native::reveal_in_finder(&download.path);
        }
    }

    /// The progress ring over the library button while files download.
    pub(super) fn render_download_badge(&self, theme: &Theme) -> Option<Div> {
        let arc = self.downloads.ring()?;
        let (track, fill) = (
            theme.ink(if theme.dark { 0.18 } else { 0.12 }),
            theme.focus(),
        );
        Some(
            div()
                .absolute()
                .left(px(BADGE_LEFT))
                .top(px(BADGE_TOP))
                .size(px(BADGE_SIZE))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(theme.surface())
                .shadow(vec![shadow(1., 3., 0.2)])
                .child(
                    canvas(
                        |_, _, _| {},
                        move |bounds, _, window, _| paint_ring(bounds, arc, track, fill, window),
                    )
                    .absolute()
                    .size_full(),
                )
                .child(icon(Icon::ArrowDown, 10., fill)),
        )
    }
}

/// Strokes the track and the arc of the ring inside `bounds`.
fn paint_ring(bounds: Bounds<Pixels>, arc: RingArc, track: Hsla, fill: Hsla, window: &mut Window) {
    let center = bounds.center();
    let radius = f32::from(bounds.size.width) / 2. - RING_WIDTH;
    let at = |turns: f32| {
        let angle = turns * TAU - TAU / 4.;
        point(
            center.x + px(radius * angle.cos()),
            center.y + px(radius * angle.sin()),
        )
    };
    let stroke = |start: f32, length: f32| {
        let mut path = PathBuilder::stroke(px(RING_WIDTH));
        path.move_to(at(start));
        // Arcs of half a turn at most, so full circles draw too.
        let halves = (length * 2.).ceil().max(1.);
        for step in 1..=halves as u32 {
            let end = start + length * step as f32 / halves;
            path.arc_to(point(px(radius), px(radius)), px(0.), false, true, at(end));
        }
        path.build().ok()
    };
    for (path, color) in [
        (stroke(0., 1.), track),
        (stroke(arc.start, arc.length), fill),
    ] {
        if let Some(path) = path {
            window.paint_path(path, color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn downloads(sizes: &[(u64, Option<u64>)]) -> Downloads {
        let mut downloads = Downloads::default();
        for (id, &(received, total)) in (1..).zip(sizes) {
            downloads.start(id, 7, format!("{id}.zip"), PathBuf::from("/tmp"));
            downloads.progress(id, received, total);
        }
        downloads
    }

    #[test]
    fn the_ring_adds_up_running_downloads() {
        assert_eq!(Downloads::default().ring(), None);
        let mut both = downloads(&[(30, Some(100)), (10, Some(100))]);
        assert_eq!(both.ring().unwrap().length, 0.2);
        both.finish(1, true);
        assert_eq!(both.ring().unwrap().length, 0.1);
        assert!(both.running_in(7));
        both.finish(2, false);
        assert_eq!(both.ring(), None);
        assert!(!both.running_in(7));
        // Just started: a sliver, not nothing.
        assert_eq!(downloads(&[(0, Some(100))]).ring().unwrap().length, 0.02);
    }

    #[test]
    fn without_a_size_the_ring_steps_with_received_bytes() {
        let ring = |received| {
            downloads(&[(received, None), (5, Some(10))])
                .ring()
                .unwrap()
        };
        assert_eq!(
            ring(0),
            RingArc {
                start: 0.,
                length: 0.25
            }
        );
        assert_eq!(ring(3 * UNKNOWN_SIZE_STEP).start, 3. / 8.);
        assert_eq!(ring(9 * UNKNOWN_SIZE_STEP).start, 1. / 8.);
    }

    #[test]
    fn statuses_show_progress_or_failure() {
        let mut list = downloads(&[(420, Some(1000)), (12_345_678, None), (5, Some(5))]);
        let status = |list: &Downloads, id| list.get(id).unwrap().status();
        assert_eq!(status(&list, 1).as_deref(), Some("42%"));
        assert_eq!(status(&list, 2).as_deref(), Some("12.3 MB"));
        list.finish(3, true);
        list.finish(2, false);
        assert_eq!(status(&list, 3), None);
        assert_eq!(status(&list, 2).as_deref(), Some("Failed"));
        // Newest first.
        let ids: Vec<u64> = list.recent().map(|download| download.id).collect();
        assert_eq!(ids, [3, 2, 1]);
    }

    #[test]
    fn sizes_read_like_finder() {
        assert_eq!(file_size(0), "0 KB");
        assert_eq!(file_size(1), "1 KB");
        assert_eq!(file_size(998_999), "999 KB");
        assert_eq!(file_size(999_000), "1.0 MB");
        assert_eq!(file_size(1_240_000), "1.2 MB");
        assert_eq!(file_size(999_960_000), "1.0 GB");
        assert_eq!(file_size(3_400_000_000), "3.4 GB");
    }
}
