//! Downloads: the progress ring on the library button, the library's
//! Downloads section, the toasts when one ends, and the question before
//! quitting cancels unfinished ones. Running downloads can be cancelled and
//! stopped ones downloaded again, in the space they came from. The library
//! keeps the last few after a restart. The ring moves only when the engine
//! reports progress.

use super::{
    icons::{Icon, icon},
    menu::MenuKind,
    shadow,
    theme::Theme,
    toast_window::ToastKind,
};
use crate::{
    app::BrowserApp,
    engine::{DownloadEnd, page::elide_middle},
    model::{DOWNLOAD_LIMIT, Pin, SavedDownload, SpaceId, TabId, display_url},
    native,
};
use gpui::{
    App, Bounds, Context, Div, FontWeight, Hsla, ParentElement, PathBuilder, Pixels, PromptLevel,
    SharedString, Styled, Window, WindowHandle, canvas, div, point, px,
};
use std::{f32::consts::TAU, path::PathBuf};

pub use crate::model::DownloadState;

/// Downloads kept from the last session are numbered from here, far above
/// the engine's own numbers, which start at 1 at every launch.
const RESTORED_IDS: u64 = 1 << 48;
/// The badge over the library button's top-right corner (arc-design.md,
/// image 20), in bottom bar coordinates.
const BADGE_SIZE: f32 = 18.;
const BADGE_LEFT: f32 = 24.5;
const BADGE_TOP: f32 = 6.;
const RING_WIDTH: f32 = 1.75;
/// Without a size, the ring shows a quarter arc that steps on by an
/// eighth of a turn for every this many bytes.
const UNKNOWN_SIZE_STEP: u64 = 256 * 1024;
/// File names in questions and toasts keep this many characters at most,
/// their end included.
const NAME_CHARS: usize = 40;

pub struct Download {
    pub id: u64,
    /// The tab whose browser downloads it, this session.
    pub tab_id: Option<TabId>,
    /// The space whose profile it downloads with.
    pub space_id: Option<SpaceId>,
    pub name: SharedString,
    pub path: PathBuf,
    pub url: String,
    pub received: u64,
    pub total: Option<u64>,
    pub state: DownloadState,
    /// Why it failed, in a few words.
    pub error: Option<String>,
    /// Done, but no longer where it was saved (checked as the library
    /// opens).
    pub missing: bool,
}

impl Download {
    /// The line under the name in the library: progress, or how it ended.
    pub fn status(&self) -> String {
        match self.state {
            DownloadState::Running => match self.total {
                Some(total) => format!(
                    "{}% of {}",
                    (self.received * 100 / total.max(1)).min(100),
                    file_size(total)
                ),
                None => file_size(self.received),
            },
            DownloadState::Done if self.missing => "Moved or deleted".to_owned(),
            DownloadState::Done => match self.total {
                Some(size) => format!("Done — {}", file_size(size)),
                None => "Done".to_owned(),
            },
            DownloadState::Failed => match &self.error {
                Some(error) => format!("Failed — {error}"),
                None => "Failed".to_owned(),
            },
            DownloadState::Cancelled => "Cancelled".to_owned(),
        }
    }

    /// Whether it stopped before the end and can be downloaded again.
    pub fn can_retry(&self) -> bool {
        matches!(self.state, DownloadState::Failed | DownloadState::Cancelled)
            && !self.url.is_empty()
            && self.space_id.is_some()
    }

    fn saved(&self) -> SavedDownload {
        SavedDownload {
            name: self.name.to_string(),
            path: self.path.clone(),
            url: self.url.clone(),
            space_id: self.space_id,
            state: self.state,
            size: self.total.filter(|_| self.state == DownloadState::Done),
            error: self.error.clone(),
        }
    }
}

#[derive(Default)]
pub struct Downloads {
    /// Newest first.
    items: Vec<Download>,
    /// Whether the question before quitting is on screen.
    asking_to_quit: bool,
    /// A download failed since the library was last open: the badge on its
    /// button says so.
    failure_unseen: bool,
}

/// The ring: `start` and `length` in turns, clockwise from 12 o'clock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RingArc {
    pub start: f32,
    pub length: f32,
}

impl Downloads {
    /// The library as the last session saved it.
    pub fn restore(saved: &[SavedDownload]) -> Self {
        let items = (RESTORED_IDS..)
            .zip(saved)
            .map(|(id, saved)| Download {
                id,
                tab_id: None,
                space_id: saved.space_id,
                name: saved.name.clone().into(),
                path: saved.path.clone(),
                url: saved.url.clone(),
                received: saved.size.unwrap_or(0),
                total: saved.size,
                state: saved.state,
                error: saved.error.clone(),
                missing: saved.state == DownloadState::Done && !saved.path.exists(),
            })
            .collect();
        Self {
            items,
            ..Self::default()
        }
    }

    /// What to save of the library.
    pub fn saved(&self) -> Vec<SavedDownload> {
        self.items.iter().map(Download::saved).collect()
    }

    pub fn start(
        &mut self,
        id: u64,
        tab_id: TabId,
        space_id: Option<SpaceId>,
        name: String,
        path: PathBuf,
        url: String,
    ) {
        self.items.insert(
            0,
            Download {
                id,
                tab_id: Some(tab_id),
                space_id,
                name: name.into(),
                path,
                url,
                received: 0,
                total: None,
                state: DownloadState::Running,
                error: None,
                missing: false,
            },
        );
    }

    fn get_mut(&mut self, id: u64) -> Option<&mut Download> {
        self.items.iter_mut().find(|download| download.id == id)
    }

    /// Progress of a running download; false for any other.
    pub fn progress(&mut self, id: u64, received: u64, total: Option<u64>) -> bool {
        let Some(download) = self
            .get_mut(id)
            .filter(|download| download.state == DownloadState::Running)
        else {
            return false;
        };
        download.received = received;
        download.total = total;
        true
    }

    /// Ends a running download; `None` if it wasn't running (a cancel that
    /// crossed its last step).
    pub fn finish(&mut self, id: u64, end: &DownloadEnd) -> Option<&Download> {
        let download = self
            .get_mut(id)
            .filter(|download| download.state == DownloadState::Running)?;
        match *end {
            DownloadEnd::Complete { size } => {
                download.state = DownloadState::Done;
                download.received = size;
                download.total = Some(size);
            }
            DownloadEnd::Cancelled => download.state = DownloadState::Cancelled,
            DownloadEnd::Failed { reason } => {
                download.state = DownloadState::Failed;
                download.error = reason.map(str::to_owned);
            }
        }
        if download.state == DownloadState::Failed {
            self.failure_unseen = true;
        }
        self.trim();
        self.get(id)
    }

    /// Keeps every running download and the `DOWNLOAD_LIMIT` newest others.
    fn trim(&mut self) {
        let mut others = 0;
        self.items.retain(|download| {
            if download.state == DownloadState::Running {
                return true;
            }
            others += 1;
            others <= DOWNLOAD_LIMIT
        });
    }

    pub fn get(&self, id: u64) -> Option<&Download> {
        self.items.iter().find(|download| download.id == id)
    }

    fn remove(&mut self, id: u64) -> Option<Download> {
        let index = self.items.iter().position(|download| download.id == id)?;
        Some(self.items.remove(index))
    }

    /// Whether a download started in the tab still runs: the tab's
    /// browser must stay open until it ends, or its progress goes unheard.
    pub fn running_in(&self, tab_id: TabId) -> bool {
        self.running()
            .any(|download| download.tab_id == Some(tab_id))
    }

    /// The library's list: running downloads first, then the others,
    /// newest first.
    pub fn listed(&self) -> impl Iterator<Item = &Download> {
        self.running().chain(
            self.items
                .iter()
                .filter(|download| download.state != DownloadState::Running),
        )
    }

    fn running(&self) -> impl Iterator<Item = &Download> {
        self.items
            .iter()
            .filter(|download| download.state == DownloadState::Running)
    }

    /// The library opened: files moved or deleted since are noted, and a
    /// failure is no longer news.
    pub fn library_opened(&mut self) {
        self.failure_unseen = false;
        for download in &mut self.items {
            if download.state == DownloadState::Done {
                download.missing = !download.path.exists();
            }
        }
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

    /// The ring as one line, for showing over the page while the sidebar
    /// is hidden: "Downloading report.pdf — 35%".
    fn summary(&self) -> Option<String> {
        let mut running = self.running();
        let first = running.next()?;
        let count = 1 + running.count();
        let what = if count == 1 {
            elide_middle(&first.name, NAME_CHARS)
        } else {
            format!("{count} files")
        };
        let ring = self.ring()?;
        Some(if ring.start == 0. {
            format!("Downloading {what} — {}%", (ring.length * 100.).round())
        } else {
            format!("Downloading {what}…")
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
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn download_started(
        &mut self,
        id: u64,
        tab_id: TabId,
        name: String,
        path: PathBuf,
        url: String,
        blank_tab: bool,
        cx: &mut Context<Self>,
    ) {
        let space_id = self.state.space_of_tab(tab_id);
        self.downloads
            .start(id, tab_id, space_id, name, path, display_url(&url));
        // A link that opened a new tab for a file leaves it blank: close it
        // (it isn't worth reopening) and go back to the page with the link,
        // in its own space: the window stays in the space on screen. Its
        // browser stays until the download ends.
        if blank_tab
            && let Some(space) = space_id.and_then(|id| self.state.space(id))
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
        self.save_downloads(cx);
        self.downloads_changed(cx);
    }

    /// Redraws for a change to running downloads: the ring, the library,
    /// or with the sidebar hidden their line over the page. A page in
    /// fullscreen shows none of them.
    fn downloads_changed(&self, cx: &mut Context<Self>) {
        if !self.fullscreen || self.menu.is_some() {
            cx.notify();
        }
    }

    fn save_downloads(&mut self, cx: &mut Context<Self>) {
        if self.state.set_downloads(self.downloads.saved()) {
            self.persist_later(cx);
        }
    }

    pub(crate) fn download_progress(
        &mut self,
        id: u64,
        received: u64,
        total: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        if self.downloads.progress(id, received, total) {
            self.downloads_changed(cx);
        }
    }

    pub(crate) fn download_finished(&mut self, id: u64, end: DownloadEnd, cx: &mut Context<Self>) {
        let Some(download) = self.downloads.finish(id, &end) else {
            return;
        };
        let (name, tab_id) = (elide_middle(&download.name, NAME_CHARS), download.tab_id);
        // A tab closed or unloaded during the download kept its browser.
        if let Some(tab_id) = tab_id
            && !self.downloads.running_in(tab_id)
            && !self.is_loaded(tab_id)
            && self.engine.borrow().has_tab(tab_id)
        {
            self.engine.borrow_mut().close_tab(tab_id);
        }
        // The open library shows a failure as it happens: it is no news.
        if self
            .menu
            .as_ref()
            .is_some_and(|menu| menu.kind == MenuKind::Library)
        {
            self.downloads.failure_unseen = false;
        }
        self.save_downloads(cx);
        match end {
            DownloadEnd::Complete { .. } => self.show_toast(format!("Downloaded {name}"), cx),
            DownloadEnd::Failed { .. } => {
                self.show_toast_kind(ToastKind::Failed, format!("Couldn't download {name}"), cx)
            }
            // Cancelled from the library, which shows it.
            DownloadEnd::Cancelled => self.downloads_changed(cx),
        }
    }

    /// The library's ×: stops a running download. The engine reports the
    /// end, which the row then shows.
    pub fn cancel_download(&mut self, id: u64) {
        self.engine.borrow().cancel_download(id);
    }

    /// Downloads a failed or cancelled file again, with the profile of the
    /// space it came from: through a page of that space that is open, else
    /// through a tab of its own that closes once the download starts. The
    /// new download takes the old one's place at the top of the list.
    pub fn retry_download(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(download) = self
            .downloads
            .get(id)
            .filter(|download| download.can_retry())
        else {
            return;
        };
        let (url, origin_tab) = (download.url.clone(), download.tab_id);
        let Some(space_id) = download
            .space_id
            .filter(|&space_id| self.state.space(space_id).is_some())
        else {
            return self.show_toast_kind(ToastKind::Failed, "Its space was deleted", cx);
        };
        let open_tab = origin_tab
            .filter(|&tab| self.state.space_of_tab(tab) == Some(space_id))
            .into_iter()
            .chain(
                self.state
                    .space(space_id)
                    .into_iter()
                    .flat_map(|space| space.tabs.iter().map(|tab| tab.id)),
            )
            .find(|&tab| self.is_loaded(tab));
        let started = open_tab.is_some_and(|tab| self.engine.borrow().start_download(tab, &url));
        if !started {
            let Some(tab) = self.state.new_background_tab(space_id, &url) else {
                return;
            };
            self.ensure_loaded(tab);
            self.persist_later(cx);
        }
        self.downloads.remove(id);
        self.save_downloads(cx);
        cx.notify();
    }

    /// Asks whether to quit while downloads run (a sheet on macOS), and
    /// quits if so. Returns false when there is nothing to ask.
    fn ask_before_quitting(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let running: Vec<&Download> = self.downloads.running().collect();
        let Some(first) = running.first() else {
            return false;
        };
        let (title, detail) = if running.len() == 1 {
            (
                "A download is in progress — Quit anyway?",
                format!(
                    "Quitting cancels “{}”, which hasn't finished downloading.",
                    elide_middle(&first.name, NAME_CHARS)
                ),
            )
        } else {
            (
                "Downloads in progress — Quit anyway?",
                format!(
                    "Quitting cancels the {} downloads that haven't finished.",
                    running.len()
                ),
            )
        };
        // Asked once: quitting again while it shows does nothing more.
        if std::mem::replace(&mut self.downloads.asking_to_quit, true) {
            return true;
        }
        window.activate_window();
        let answer = window.prompt(
            PromptLevel::Warning,
            title,
            Some(&detail),
            &["Quit", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            let quit = answer.await == Ok(0);
            let _ = this.update(cx, |this, cx| {
                this.downloads.asking_to_quit = false;
                if quit {
                    // Saved as cancelled, as quitting cancels them.
                    for download in &mut this.downloads.items {
                        if download.state == DownloadState::Running {
                            download.state = DownloadState::Cancelled;
                        }
                    }
                    this.state.set_downloads(this.downloads.saved());
                    cx.quit();
                }
            });
        })
        .detach();
        true
    }

    /// Shows a finished download in Finder, selected.
    pub fn reveal_download(&self, id: u64) {
        if let Some(download) = self.downloads.get(id) {
            native::reveal_in_finder(&download.path);
        }
    }

    /// The library is opening.
    pub(crate) fn library_opened(&mut self) {
        self.downloads.library_opened();
    }

    /// Running downloads as a line over the page, while the hidden sidebar
    /// can't show their ring.
    pub(crate) fn downloads_over_card(&self) -> Option<SharedString> {
        self.downloads.summary().map(SharedString::from)
    }

    /// The progress ring over the library button while files download, or
    /// a red one with "!" after a download failed, until the library opens.
    pub(super) fn render_download_badge(&self, theme: &Theme) -> Option<Div> {
        let ring = self.downloads.ring();
        let failed = ring.is_none() && self.downloads.failure_unseen;
        let arc = ring.or(failed.then_some(RingArc {
            start: 0.,
            length: 1.,
        }))?;
        let (track, fill) = if failed {
            (theme.error().opacity(0.25), theme.error())
        } else {
            (
                theme.ink(if theme.dark { 0.18 } else { 0.12 }),
                theme.focus(),
            )
        };
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
                .child(if failed {
                    div()
                        .text_size(px(11.))
                        .line_height(px(12.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(fill)
                        .child("!")
                } else {
                    div().child(icon(Icon::ArrowDown, 10., fill))
                }),
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
            let name = format!("{id}.zip");
            downloads.start(id, 7, None, name, PathBuf::from("/tmp"), String::new());
            downloads.progress(id, received, total);
        }
        downloads
    }

    const DONE: DownloadEnd = DownloadEnd::Complete { size: 5 };
    const FAILED: DownloadEnd = DownloadEnd::Failed { reason: None };

    #[test]
    fn the_ring_adds_up_running_downloads() {
        assert_eq!(Downloads::default().ring(), None);
        let mut both = downloads(&[(30, Some(100)), (10, Some(100))]);
        assert_eq!(both.ring().unwrap().length, 0.2);
        both.finish(1, &DONE);
        assert_eq!(both.ring().unwrap().length, 0.1);
        assert!(both.running_in(7));
        both.finish(2, &FAILED);
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
        assert_eq!(status(&list, 1), "42% of 1 KB");
        assert_eq!(status(&list, 2), "12.3 MB");
        list.finish(3, &DONE);
        list.finish(2, &FAILED);
        assert_eq!(status(&list, 3), "Done — 1 KB");
        assert_eq!(status(&list, 2), "Failed");
        // Running first, then newest first.
        let ids: Vec<u64> = list.listed().map(|download| download.id).collect();
        assert_eq!(ids, [1, 3, 2]);
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
