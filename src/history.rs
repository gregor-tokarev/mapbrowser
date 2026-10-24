//! Browsing history: the pages each space has shown, for the command bar's
//! suggestions and its History view (⌘Y). It is kept apart from the session,
//! in `history.json` in the data directory, because it grows with every page
//! and is cleared on its own. A visit is a page's new document: changes of
//! address within it (fragments, the History API) and reloads are the same
//! visit. Addresses and titles are private: nothing here logs them.

use crate::model::{self, SpaceId, TabId, display_url};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet, hash_map::Entry},
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

const FILE: &str = "history.json";
/// Pages kept, in all spaces together; past this the least recently
/// visited go, a batch at a time (`TRIM_SLACK`) so trimming stays rare.
const MAX_PAGES: usize = 5000;
const TRIM_SLACK: usize = 100;
/// Visits older than this (90 days) are forgotten when the history is read.
const MAX_AGE_SECS: u64 = 90 * 24 * 60 * 60;
/// Longer addresses carry state rather than name a page worth finding again.
const MAX_URL_LEN: usize = 2048;
const MAX_TITLE_CHARS: usize = 300;
/// A visit takes the titles its page shows this long after it began (10 s),
/// then keeps the last: a page that keeps changing its title (a timer, an
/// unread count) would otherwise rewrite the file every few seconds, and
/// leave the visit named by whatever it said last. Chrome does the same.
const TITLE_SECS: u64 = 10;

/// A space's visits to one page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Visit {
    pub space_id: SpaceId,
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default = "one")]
    pub count: u32,
    /// The latest, in seconds since the Unix epoch.
    pub last: u64,
}

fn one() -> u32 {
    1
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    visits: Vec<Visit>,
}

/// A page as the command bar lists it: every space's visits together.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisitedPage {
    pub url: String,
    /// The latest title; empty if the page had none.
    pub title: String,
    pub count: u32,
    pub last: u64,
    /// The spaces it was visited in, the latest visit's first.
    pub spaces: Vec<SpaceId>,
}

pub struct History {
    path: PathBuf,
    visits: Vec<Visit>,
    /// The visit each tab's page is, by index in `visits`, so the title that
    /// arrives after the address goes to it.
    pages: HashMap<TabId, usize>,
    /// Changed since the last snapshot.
    changed: bool,
    /// The last snapshot's number. Quitting writes the visits again unless
    /// it was written: after a failed write (a full disk) they would be
    /// lost, or a cleared history would come back at the next launch.
    snapshot_seq: u64,
}

/// Seconds since the Unix epoch.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Web pages only: no blank, file, data or Chromium pages.
fn recordable(url: &str) -> bool {
    let scheme = url.split_once("://").map(|(scheme, _)| scheme);
    url.len() <= MAX_URL_LEN
        && scheme.is_some_and(|scheme| {
            scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
        })
}

impl History {
    /// Reads the saved history. A damaged file starts it empty; the next
    /// save replaces it.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(FILE);
        let visits = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Saved>(&bytes)
                .map(|saved| saved.visits)
                .unwrap_or_else(|_| {
                    // serde's message could quote an address.
                    eprintln!("Couldn't read the browsing history: the file is damaged");
                    Vec::new()
                }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                eprintln!("Couldn't read the browsing history: {error}");
                Vec::new()
            }
        };
        let oldest = now().saturating_sub(MAX_AGE_SECS);
        let mut history = Self {
            path,
            visits,
            pages: HashMap::new(),
            changed: false,
            snapshot_seq: 0,
        };
        let count = history.visits.len();
        history
            .visits
            .retain(|visit| visit.last >= oldest && recordable(&visit.url));
        history.changed = history.visits.len() != count;
        history.trim(MAX_PAGES);
        history
    }

    /// The tab, in `space_id`, shows a new document at `url`. A page the tab
    /// showed last (a reload, Back from an error page) isn't counted again,
    /// nor one `restored` from the last session, which keeps its place.
    /// Returns whether the history changed.
    pub fn visit(
        &mut self,
        tab_id: TabId,
        space_id: SpaceId,
        url: &str,
        restored: bool,
        now: u64,
    ) -> bool {
        let url = display_url(url);
        if !recordable(&url) {
            self.pages.remove(&tab_id);
            return false;
        }
        let existing = self
            .visits
            .iter()
            .position(|visit| visit.space_id == space_id && visit.url == url);
        let index = match existing {
            Some(index) if restored => {
                self.pages.insert(tab_id, index);
                return false;
            }
            Some(index) => {
                let again = self.pages.get(&tab_id) == Some(&index);
                let visit = &mut self.visits[index];
                if !again {
                    visit.count = visit.count.saturating_add(1);
                }
                visit.last = now;
                index
            }
            None => {
                self.visits.push(Visit {
                    space_id,
                    url,
                    title: String::new(),
                    count: 1,
                    last: now,
                });
                self.visits.len() - 1
            }
        };
        self.pages.insert(tab_id, index);
        self.changed = true;
        self.trim(MAX_PAGES + TRIM_SLACK);
        true
    }

    /// The title of the page the tab shows, at `now`: its first, or a later
    /// one early in the visit (`TITLE_SECS`). Returns whether it changed.
    pub fn set_title(&mut self, tab_id: TabId, title: &str, now: u64) -> bool {
        let title = title.trim();
        let Some(visit) = self
            .pages
            .get(&tab_id)
            .and_then(|&index| self.visits.get_mut(index))
        else {
            return false;
        };
        // Chromium reports the address as the title of pages without one.
        if title.is_empty() || title == visit.url {
            return false;
        }
        if !visit.title.is_empty() && now.saturating_sub(visit.last) > TITLE_SECS {
            return false;
        }
        let title: String = title.chars().take(MAX_TITLE_CHARS).collect();
        if visit.title == title {
            return false;
        }
        visit.title = title;
        self.changed = true;
        true
    }

    /// Drops a deleted space's visits. Returns whether there were any.
    pub fn forget_space(&mut self, space_id: SpaceId) -> bool {
        let count = self.visits.len();
        self.visits.retain(|visit| visit.space_id != space_id);
        self.forgot(count)
    }

    /// Drops the visits of spaces the session doesn't have: one deleted by
    /// an older version, which keeps no history, or just before a crash
    /// kept this file from being written. Returns whether there were any.
    pub fn retain_spaces(&mut self, spaces: &HashSet<SpaceId>) -> bool {
        let count = self.visits.len();
        self.visits.retain(|visit| spaces.contains(&visit.space_id));
        self.forgot(count)
    }

    /// Every visited address, for the favicons still worth keeping.
    pub fn urls(&self) -> impl Iterator<Item = &str> {
        self.visits.iter().map(|visit| visit.url.as_str())
    }

    /// Forgets every visit. Returns whether there were any.
    pub fn clear(&mut self) -> bool {
        let count = self.visits.len();
        self.visits.clear();
        self.forgot(count)
    }

    fn forgot(&mut self, count: usize) -> bool {
        if self.visits.len() == count {
            return false;
        }
        // Indices moved.
        self.pages.clear();
        self.changed = true;
        true
    }

    /// Keeps the `MAX_PAGES` most recently visited pages once there are more
    /// than `limit`.
    fn trim(&mut self, limit: usize) {
        if self.visits.len() <= limit {
            return;
        }
        self.visits.sort_by_key(|visit| Reverse(visit.last));
        self.visits.truncate(MAX_PAGES);
        // Indices moved; a title still to come for a page on screen is lost.
        self.pages.clear();
        self.changed = true;
    }

    /// Every visited page, each space's visits to it together, the most
    /// recently visited first.
    pub fn pages(&self) -> Vec<VisitedPage> {
        let mut pages: HashMap<&str, VisitedPage> = HashMap::new();
        for visit in &self.visits {
            match pages.entry(&visit.url) {
                Entry::Vacant(entry) => {
                    entry.insert(VisitedPage {
                        url: visit.url.clone(),
                        title: visit.title.clone(),
                        count: visit.count,
                        last: visit.last,
                        spaces: vec![visit.space_id],
                    });
                }
                Entry::Occupied(mut entry) => {
                    let page = entry.get_mut();
                    page.count = page.count.saturating_add(visit.count);
                    let newer = visit.last > page.last;
                    if newer {
                        page.last = visit.last;
                        page.spaces.insert(0, visit.space_id);
                    } else {
                        page.spaces.push(visit.space_id);
                    }
                    if !visit.title.is_empty() && (newer || page.title.is_empty()) {
                        page.title.clone_from(&visit.title);
                    }
                }
            }
        }
        let mut pages: Vec<VisitedPage> = pages.into_values().collect();
        pages.sort_by(|a, b| b.last.cmp(&a.last).then_with(|| a.url.cmp(&b.url)));
        pages
    }

    /// The visits to write, if they changed since the last snapshot. Taken
    /// on the main thread; writing can happen anywhere.
    pub fn snapshot(&mut self) -> Option<HistorySnapshot> {
        std::mem::take(&mut self.changed).then(|| self.take_snapshot())
    }

    fn take_snapshot(&mut self) -> HistorySnapshot {
        self.snapshot_seq = NEXT_SNAPSHOT.fetch_add(1, Ordering::Relaxed);
        HistorySnapshot {
            path: self.path.clone(),
            visits: self.visits.clone(),
            seq: self.snapshot_seq,
        }
    }

    /// Writes on the calling thread, for quitting: any change, and visits
    /// whose last write failed or is still running (it is waited for).
    pub fn save(&mut self) {
        let written = *WRITTEN.lock().unwrap_or_else(PoisonError::into_inner);
        if (std::mem::take(&mut self.changed) || written < self.snapshot_seq)
            && let Err(error) = self.take_snapshot().write()
        {
            eprintln!("Couldn't save the browsing history: {error}");
        }
    }
}

static NEXT_SNAPSHOT: AtomicU64 = AtomicU64::new(1);
/// The newest snapshot written. Holding the lock for the whole write
/// serializes writers, and an older snapshot never replaces a newer one.
static WRITTEN: Mutex<u64> = Mutex::new(0);

pub struct HistorySnapshot {
    path: PathBuf,
    visits: Vec<Visit>,
    seq: u64,
}

impl HistorySnapshot {
    pub fn write(&self) -> io::Result<()> {
        let mut written = WRITTEN.lock().unwrap_or_else(PoisonError::into_inner);
        if *written >= self.seq {
            return Ok(());
        }
        let bytes = serde_json::to_vec(&SavedRef {
            visits: &self.visits,
        })
        .map_err(io::Error::other)?;
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        model::write_file(&self.path, &bytes)?;
        *written = self.seq;
        Ok(())
    }
}

#[derive(Serialize)]
struct SavedRef<'a> {
    visits: &'a [Visit],
}
