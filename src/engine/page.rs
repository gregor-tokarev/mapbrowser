//! What a page's Chromium events become for the shell: which loads show as
//! loading, how often an animated favicon is fetched, the page menu's
//! labels, and the hovered link's text.

#[cfg(target_os = "macos")]
use super::EngineEvent;
#[cfg(any(target_os = "macos", test))]
use super::progress_worth_reporting;
#[cfg(any(target_os = "macos", test))]
use std::time::{Duration, Instant};

/// A tab's loading state as the shell shows it: the progress bar and Stop
/// while `loading`, and the back and forward buttons.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoadState {
    pub loading: bool,
    pub can_go_back: bool,
    pub can_go_forward: bool,
}

#[cfg(target_os = "macos")]
impl LoadState {
    pub fn event(self, tab_id: u64) -> EngineEvent {
        EngineEvent::Loading {
            tab_id,
            loading: self.loading,
            can_go_back: self.can_go_back,
            can_go_forward: self.can_go_forward,
        }
    }
}

/// Chromium reports loading, a start and a stop within milliseconds, for
/// every history update a page makes (pushState, replaceState, #hash: map
/// pans, scroll spies, chat apps switching channels) and for every load in
/// an iframe, as it does for loads of new documents. Only a load of a new
/// document in the main frame shows as loading, as in Chrome; the others
/// report changes to back and forward only, so they redraw nothing else.
/// Chromium's loading state turns on before the navigation behind it is
/// known (`OnBeforeBrowse`), so loading shows from there.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Default)]
pub struct PageLoad {
    /// Chromium's loading state, as it last reported it.
    chromium: LoadState,
    /// What the shell was told last.
    shown: LoadState,
    /// The progress reported last in the load shown.
    progress: Option<f64>,
}

#[cfg(any(target_os = "macos", test))]
impl PageLoad {
    /// Chromium's loading state changed (OnLoadingStateChange). Returns
    /// what to report, if that changed.
    pub fn loading_changed(&mut self, state: LoadState) -> Option<LoadState> {
        self.chromium = state;
        self.show(LoadState {
            loading: self.shown.loading && state.loading,
            ..state
        })
    }

    /// The main frame goes to another document: OnBeforeBrowse, for a
    /// navigation that goes ahead.
    pub fn main_frame_navigation(&mut self) -> Option<LoadState> {
        // A navigation always makes Chromium load first; one that somehow
        // didn't would show as loading until the next one stopped.
        if !self.chromium.loading {
            return None;
        }
        if !self.shown.loading {
            self.progress = None;
        }
        self.show(LoadState {
            loading: true,
            ..self.chromium
        })
    }

    /// Whether to report `progress` (OnLoadingProgressChange): only for
    /// the load shown, in steps (`progress_worth_reporting`).
    pub fn progress(&mut self, progress: f64) -> bool {
        if !self.shown.loading || !progress_worth_reporting(self.progress, progress) {
            return false;
        }
        self.progress = Some(progress);
        true
    }

    fn show(&mut self, state: LoadState) -> Option<LoadState> {
        (state != self.shown).then(|| {
            self.shown = state;
            state
        })
    }
}

/// Pages that animate their favicon (a canvas drawn into a data: URL every
/// second, favico.js badges) would have every frame fetched, encoded, stored
/// and drawn. A tab fetches its site's icon at most this often.
#[cfg(any(target_os = "macos", test))]
pub const FAVICON_INTERVAL: Duration = Duration::from_secs(2);

/// How long a tab waits before fetching the icon a page on `host` names,
/// given the `last` fetch (its page host and start). A new site's icon comes
/// at once.
#[cfg(any(target_os = "macos", test))]
pub fn favicon_wait(
    last: Option<(Option<&str>, Instant)>,
    host: Option<&str>,
    now: Instant,
) -> Option<Duration> {
    let (_, at) = last.filter(|(last_host, _)| *last_host == host)?;
    FAVICON_INTERVAL
        .checked_sub(now.saturating_duration_since(at))
        .filter(|wait| !wait.is_zero())
}

/// The page menu's "Search Google for “…”" for `selection`, or nothing for
/// a blank one. Long selections are cut short; `&` is doubled, as menu
/// labels read a single one as a mnemonic marker.
#[cfg(any(target_os = "macos", test))]
pub fn search_label(selection: &str) -> Option<String> {
    const MAX_CHARS: usize = 32;
    let words = selection.split_whitespace().collect::<Vec<_>>().join(" ");
    if words.is_empty() {
        return None;
    }
    let mut shown: String = words.chars().take(MAX_CHARS).collect();
    if shown.len() < words.len() {
        // After a whole word, as Chrome cuts it.
        if let Some(space) = shown.rfind(' ') {
            shown.truncate(space);
        }
        shown.push('…');
    }
    Some(format!("Search Google for “{}”", shown.replace('&', "&&")))
}

/// `text` with its middle replaced by "…" so that it keeps `max_chars`
/// characters at most: the start and the end of a long link stay readable.
pub fn elide_middle(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    let kept = max_chars.saturating_sub(1);
    let tail = kept / 2;
    let head = kept - tail;
    let start: String = text.chars().take(head).collect();
    let end: String = text.chars().skip(count - tail).collect();
    format!("{start}…{end}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(loading: bool, can_go_back: bool) -> LoadState {
        LoadState {
            loading,
            can_go_back,
            can_go_forward: false,
        }
    }

    #[test]
    fn history_updates_and_iframe_loads_show_no_loading() {
        let mut load = PageLoad::default();
        // pushState on a page that had no history yet, as Chromium 152
        // reports it: no OnBeforeBrowse, so nothing shows but Back.
        assert_eq!(load.loading_changed(state(true, false)), None);
        assert!(!load.progress(0.1));
        assert!(!load.progress(1.0));
        assert_eq!(
            load.loading_changed(state(false, true)),
            Some(state(false, true))
        );
        // replaceState and #hash change nothing on screen.
        for _ in 0..5 {
            assert_eq!(load.loading_changed(state(true, true)), None);
            assert!(!load.progress(0.1));
            assert_eq!(load.loading_changed(state(false, true)), None);
        }
        assert!(!load.shown.loading);
    }

    #[test]
    fn main_frame_loads_show_loading_and_progress() {
        let mut load = PageLoad::default();
        assert_eq!(load.loading_changed(state(true, false)), None);
        // Chromium reports 0.1 before the navigation is known.
        assert!(!load.progress(0.1));
        assert_eq!(load.main_frame_navigation(), Some(state(true, false)));
        // A redirect is the same load.
        assert_eq!(load.main_frame_navigation(), None);
        assert!(load.progress(0.5));
        assert!(!load.progress(0.55));
        assert!(load.progress(1.0));
        assert_eq!(
            load.loading_changed(state(false, true)),
            Some(state(false, true))
        );
        assert!(!load.progress(1.0));
        // The next load reports its end even if it jumps straight there.
        load.loading_changed(state(true, true));
        assert_eq!(load.main_frame_navigation(), Some(state(true, true)));
        assert!(load.progress(1.0));
    }

    #[test]
    fn navigations_without_loading_show_nothing() {
        let mut load = PageLoad::default();
        assert_eq!(load.main_frame_navigation(), None);
        assert!(!load.shown.loading);
    }

    #[test]
    fn animated_favicons_are_fetched_every_two_seconds() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let last = Some((Some("example.com"), start));
        assert_eq!(favicon_wait(None, Some("example.com"), start), None);
        assert_eq!(
            favicon_wait(last, Some("example.com"), at(500)),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(favicon_wait(last, Some("example.com"), at(2000)), None);
        // Another site's icon comes at once.
        assert_eq!(favicon_wait(last, Some("other.com"), at(500)), None);
        assert_eq!(favicon_wait(last, None, at(500)), None);
    }

    #[test]
    fn search_labels_are_short_and_escaped() {
        assert_eq!(
            search_label("  rust\n borrow   checker "),
            Some("Search Google for “rust borrow checker”".into())
        );
        assert_eq!(
            search_label("Tom & Jerry"),
            Some("Search Google for “Tom && Jerry”".into())
        );
        assert_eq!(
            search_label(&"word ".repeat(20)),
            Some("Search Google for “word word word word word word…”".into())
        );
        assert_eq!(search_label(" \n\t"), None);
    }

    #[test]
    fn long_text_loses_its_middle() {
        assert_eq!(elide_middle("example.com/a", 20), "example.com/a");
        assert_eq!(elide_middle("abcdefghij", 5), "ab…ij");
        assert_eq!(elide_middle("abcdefghij", 6), "abc…ij");
        assert_eq!(elide_middle("ääääää", 3), "ä…ä");
        assert_eq!(elide_middle("abc", 0), "…");
    }
}
