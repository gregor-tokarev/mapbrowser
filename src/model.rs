//! Persistent spaces and tabs. Each space owns a distinct Chromium profile.

use crate::favicons::{host_and_parents, normalize_host};
use gpui::http_client::Url;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        LazyLock, Mutex, PoisonError,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use unicode_segmentation::UnicodeSegmentation;

pub type SpaceId = u64;
pub type TabId = u64;

const STATE_FILE: &str = "state.json";
const BACKUP_FILE: &str = "state.backup.json";
const PROFILES_DIR: &str = "profiles";
/// Chromium's process singleton lock in its root directory, `PROFILES_DIR`,
/// while an instance runs (and after one crashed).
const CHROMIUM_LOCK: &str = "SingletonLock";
const SCHEMA_VERSION: u32 = 2;
const BLANK_URL: &str = "about:blank";
/// Space themes are indices below this; the UI owns the palettes.
pub const THEME_COUNT: u8 = 8;
pub const DEFAULT_SIDEBAR_WIDTH: f32 = 228.;
pub const MIN_SIDEBAR_WIDTH: f32 = 180.;
pub const MAX_SIDEBAR_WIDTH: f32 = 420.;
/// Closes that ⇧⌘T and the library can undo, per space. Tabs closed
/// together (Clear, Close Other Tabs) count as one.
pub const REOPEN_LIMIT: usize = 20;
/// Finished downloads the library keeps, also after a restart. Running
/// ones are kept on top of these.
pub const DOWNLOAD_LIMIT: usize = 20;

/// The sidebar section a tab belongs to. Variants are declared in sidebar
/// order, so sorting by `Pin` groups favorites, pinned tabs, then Today.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Pin {
    Favorite,
    Pinned,
    #[default]
    Today,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tab {
    pub id: TabId,
    pub url: String,
    pub title: String,
    #[serde(default)]
    pub pin: Pin,
    /// A name the user gave the tab, shown instead of the page's title.
    #[serde(default)]
    pub custom_title: Option<String>,
    /// The address a pinned tab or favorite had when it was pinned, which
    /// "Go to Pinned Page" returns to. Today tabs have none.
    #[serde(default)]
    pub pinned_url: Option<String>,
}

impl Tab {
    /// What the sidebar calls the tab: the user's name for it, else the
    /// page's title.
    pub fn shown_title(&self) -> &str {
        self.custom_title.as_deref().unwrap_or(&self.title)
    }

    /// Moves the tab to `pin`'s section. Pinning it remembers its address;
    /// back in Today it has none.
    fn repin(&mut self, pin: Pin) {
        self.pinned_url = match pin {
            Pin::Today => None,
            _ => self.pinned_url.take().or_else(|| Some(self.url.clone())),
        };
        self.pin = pin;
    }
}

/// A tab closed with ⌘W, Clear or Close Tab, which ⇧⌘T and the library
/// bring back into its space.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClosedTab {
    pub space_id: SpaceId,
    pub tab: Tab,
    /// Its place in its section when it closed, where it reopens.
    #[serde(default)]
    pub index: Option<usize>,
    /// Tabs closed together (Clear, Close Other Tabs) share the first one's
    /// id here, and ⇧⌘T reopens them together.
    #[serde(default)]
    pub batch: Option<TabId>,
}

/// Where a download stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadState {
    Running,
    Done,
    Failed,
    Cancelled,
}

/// A download as the library lists it, saved so recent downloads are still
/// listed after a restart.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedDownload {
    pub name: String,
    pub path: PathBuf,
    /// Where it came from, without credentials, to download it again.
    #[serde(default)]
    pub url: String,
    /// The space whose profile downloads it again.
    #[serde(default)]
    pub space_id: Option<SpaceId>,
    pub state: DownloadState,
    /// Bytes saved, once done.
    #[serde(default)]
    pub size: Option<u64>,
    /// Why it failed, in a few words.
    #[serde(default)]
    pub error: Option<String>,
}

/// `tabs` stays grouped by section, so its order is the sidebar order.
/// A space may be empty; `selected_tab_id` is `None` exactly when it is.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Space {
    pub id: SpaceId,
    pub name: String,
    pub tabs: Vec<Tab>,
    #[serde(default)]
    pub selected_tab_id: Option<TabId>,
    /// Always set after loading; optional so version 1 files deserialize.
    #[serde(default)]
    pub theme: Option<u8>,
    /// One grapheme, usually an emoji, shown instead of the theme dot.
    #[serde(default)]
    pub icon: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrowserState {
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    pub spaces: Vec<Space>,
    pub selected_space_id: SpaceId,
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    pub sidebar_hidden: bool,
    #[serde(default = "default_sidebar_width")]
    sidebar_width: f32,
    /// Hosts, normalized like `favicons::host_of`, where content blocking is
    /// off, subdomains included.
    #[serde(default)]
    blocking_exceptions: BTreeSet<String>,
    /// The browser window's frame, to open it there again.
    #[serde(default)]
    pub window: Option<SavedWindow>,
    /// Recently closed tabs, the most recent last. Saved, so a Clear can
    /// still be undone after a restart.
    #[serde(default)]
    closed: Vec<ClosedTab>,
    /// Where typed searches go.
    #[serde(default)]
    search_engine: SearchEngine,
    /// Recent downloads, the newest first.
    #[serde(default)]
    downloads: Vec<SavedDownload>,
    #[serde(skip, default = "data_dir")]
    storage_dir: PathBuf,
    /// Whether this is the session saved last, read from state.json, rather
    /// than a backup or the defaults.
    #[serde(skip)]
    from_state_file: bool,
    /// How this launch made do without a readable state.json, to tell the
    /// user once.
    #[serde(skip)]
    pub recovery: Option<Recovery>,
}

/// state.json couldn't be read at launch: the session came from the backup
/// or, without a readable one, from the defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recovery {
    pub from_backup: bool,
    /// Where the damaged file was set aside, if there was one.
    pub kept: Option<PathBuf>,
}

/// Where the browser window was, in points.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedWindow {
    /// The frame, from its display's top-left corner. In fullscreen, the
    /// frame it leaves fullscreen for.
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// The display's UUID, which stays when displays reconnect.
    #[serde(default)]
    pub display: Option<String>,
    #[serde(default)]
    pub fullscreen: bool,
}

/// The search engine typed searches go to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SearchEngine {
    #[default]
    Google,
    DuckDuckGo,
    Bing,
    Brave,
    Kagi,
    Ecosia,
}

impl SearchEngine {
    pub const ALL: [Self; 6] = [
        Self::Google,
        Self::DuckDuckGo,
        Self::Bing,
        Self::Brave,
        Self::Kagi,
        Self::Ecosia,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Google => "Google",
            Self::DuckDuckGo => "DuckDuckGo",
            Self::Bing => "Bing",
            Self::Brave => "Brave Search",
            Self::Kagi => "Kagi",
            Self::Ecosia => "Ecosia",
        }
    }

    /// What a search with it is called: "Google Search", "Brave Search".
    pub fn search_label(self) -> String {
        let name = self.name();
        if name.ends_with(" Search") {
            name.to_owned()
        } else {
            format!("{name} Search")
        }
    }

    /// Its name in state.json.
    fn id(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::DuckDuckGo => "duckduckgo",
            Self::Bing => "bing",
            Self::Brave => "brave",
            Self::Kagi => "kagi",
            Self::Ecosia => "ecosia",
        }
    }

    /// The results page's address, up to the encoded query.
    fn query_url(self) -> &'static str {
        match self {
            Self::Google => "https://www.google.com/search?q=",
            Self::DuckDuckGo => "https://duckduckgo.com/?q=",
            Self::Bing => "https://www.bing.com/search?q=",
            Self::Brave => "https://search.brave.com/search?q=",
            Self::Kagi => "https://kagi.com/search?q=",
            Self::Ecosia => "https://www.ecosia.org/search?q=",
        }
    }
}

impl Serialize for SearchEngine {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.id())
    }
}

/// Lenient: an engine this version doesn't know (saved by a newer one) is
/// Google, rather than a damaged session.
impl<'de> Deserialize<'de> for SearchEngine {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(value
            .as_str()
            .and_then(|id| Self::ALL.into_iter().find(|engine| engine.id() == id))
            .unwrap_or_default())
    }
}

/// The session's search engine, for the whole process: the page menu's
/// "Search … for" runs in Chromium's callbacks, away from the session.
static SEARCH_ENGINE: AtomicU8 = AtomicU8::new(0);

/// The search engine `search_url` uses.
pub fn current_search_engine() -> SearchEngine {
    let index = usize::from(SEARCH_ENGINE.load(Ordering::Relaxed));
    SearchEngine::ALL.get(index).copied().unwrap_or_default()
}

fn use_search_engine(engine: SearchEngine) {
    let index = SearchEngine::ALL
        .iter()
        .position(|&known| known == engine)
        .unwrap_or(0);
    SEARCH_ENGINE.store(index as u8, Ordering::Relaxed);
}

fn schema_version() -> u32 {
    SCHEMA_VERSION
}

fn default_sidebar_width() -> f32 {
    DEFAULT_SIDEBAR_WIDTH
}

/// Override this directory for integration tests or independent installations.
pub fn data_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("MAPBROWSER_DATA_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(path);
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join("Library/Application Support/MapBrowser")
}

impl Default for BrowserState {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            spaces: vec![
                Space::new(1, "Personal".into(), 0),
                Space::new(2, "Work".into(), 1),
            ],
            selected_space_id: 1,
            next_id: 3,
            sidebar_hidden: false,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            blocking_exceptions: BTreeSet::new(),
            window: None,
            closed: Vec::new(),
            search_engine: SearchEngine::default(),
            downloads: Vec::new(),
            storage_dir: data_dir(),
            from_state_file: false,
            recovery: None,
        }
    }
}

impl Space {
    fn new(id: SpaceId, name: String, theme: u8) -> Self {
        Self {
            id,
            name,
            tabs: Vec::new(),
            selected_tab_id: None,
            theme: Some(theme),
            icon: None,
        }
    }

    pub fn selected_tab(&self) -> Option<&Tab> {
        let id = self.selected_tab_id?;
        self.tabs.iter().find(|tab| tab.id == id)
    }

    /// One sidebar section, top to bottom.
    pub fn section(&self, pin: Pin) -> impl Iterator<Item = &Tab> {
        self.tabs.iter().filter(move |tab| tab.pin == pin)
    }

    pub fn theme(&self) -> u8 {
        self.theme.unwrap_or(0) % THEME_COUNT
    }

    fn tab_index(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }

    fn section_start(&self, pin: Pin) -> usize {
        self.tabs.partition_point(|tab| tab.pin < pin)
    }

    fn section_end(&self, pin: Pin) -> usize {
        self.tabs.partition_point(|tab| tab.pin <= pin)
    }

    /// The tab that takes over the selection from `tab_id` when it is closed
    /// or unloaded: the next tab of the same section, else the previous one
    /// there, else the nearest tab in sidebar order.
    pub fn successor_of(&self, tab_id: TabId) -> Option<TabId> {
        self.successor(self.tab_index(tab_id)?)
    }

    fn successor(&self, index: usize) -> Option<TabId> {
        let pin = self.tabs[index].pin;
        let next = self.tabs.get(index + 1);
        let previous = index.checked_sub(1).and_then(|index| self.tabs.get(index));
        next.filter(|next| next.pin == pin)
            .or(previous.filter(|previous| previous.pin == pin))
            .or(next)
            .or(previous)
            .map(|tab| tab.id)
    }

    fn remove_at(&mut self, index: usize) -> Tab {
        let successor = self.successor(index);
        let tab = self.tabs.remove(index);
        if self.selected_tab_id == Some(tab.id) {
            self.selected_tab_id = successor;
        }
        tab
    }
}

/// What this process wrote to each state directory. Holding the lock for the
/// whole write serializes writers.
static WRITTEN: LazyLock<Mutex<HashMap<PathBuf, Written>>> = LazyLock::new(Default::default);
static NEXT_SNAPSHOT: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
struct Written {
    /// The newest snapshot written or found unchanged; older ones are stale.
    seq: u64,
    /// What state.json holds, if this process wrote it.
    bytes: Vec<u8>,
    /// Whether `bytes` are flushed to the drive, not just ordered.
    flushed: bool,
    /// Whether the backup holds the state this process found on disk.
    backed_up: bool,
}

/// How far a write must reach before it returns.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Durability {
    /// The new file is complete before it replaces the old one, so a crash
    /// or power cut leaves a whole state.json, if maybe not the latest.
    Ordered,
    /// Also on the drive itself.
    Flushed,
}

/// Serialized state, ordered by when it was taken. Serializing needs the
/// state (main thread); writing it, which waits on the drive, can happen
/// anywhere.
pub struct StateSnapshot {
    root: PathBuf,
    bytes: Vec<u8>,
    seq: u64,
}

impl StateSnapshot {
    /// Atomically replace the saved state, after keeping the state found on
    /// disk at launch as a backup. Returns `Ok(false)` without writing if a
    /// newer snapshot of the same directory has already been written, or if
    /// nothing changed since the last write.
    ///
    /// Drive flushes are the cost here: on macOS each one (F_FULLFSYNC)
    /// wakes the SSD and empties its whole cache, and titles and addresses
    /// change often. So these routine saves only order their data before the
    /// rename (F_BARRIERFSYNC): a power cut can lose the last few seconds,
    /// never damage the file. `BrowserState::save` flushes, once, on quit.
    pub fn write(&self) -> io::Result<bool> {
        self.write_with(Durability::Ordered)
    }

    fn write_with(&self, durability: Durability) -> io::Result<bool> {
        let mut written = WRITTEN.lock().unwrap_or_else(PoisonError::into_inner);
        let last = written.entry(self.root.clone()).or_default();
        if last.seq >= self.seq {
            return Ok(false);
        }
        let state_path = self.root.join(STATE_FILE);
        if last.bytes == self.bytes {
            if durability == Durability::Flushed && !last.flushed {
                flush_drive(&self.root)?;
                last.flushed = true;
            }
            last.seq = self.seq;
            return Ok(false);
        }
        fs::create_dir_all(&self.root)?;
        if !last.backed_up {
            match read_state(&state_path, &self.root) {
                Ok(previous) => {
                    let previous =
                        serde_json::to_vec_pretty(&previous).map_err(io::Error::other)?;
                    atomic_write(&self.root.join(BACKUP_FILE), &previous, Durability::Ordered)?;
                    last.backed_up = true;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                    quarantine(&state_path).map(drop)?
                }
                Err(error) => return Err(error),
            }
        }
        atomic_write(&state_path, &self.bytes, durability)?;
        *last = Written {
            seq: self.seq,
            bytes: self.bytes.clone(),
            flushed: durability == Durability::Flushed,
            backed_up: last.backed_up,
        };
        Ok(true)
    }
}

/// After loading, the files may not hold what this process wrote (a damaged
/// state.json was set aside, say): write the next snapshot in any case, and
/// back up what is on disk now. Snapshot ordering stays.
fn forget_written(root: &Path) {
    let mut written = WRITTEN.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(last) = written.get_mut(root) {
        *last = Written {
            seq: last.seq,
            ..Written::default()
        };
    }
}

impl BrowserState {
    pub fn load() -> io::Result<Self> {
        Self::load_from(data_dir())
    }

    /// Recover the previous valid save if the main state was interrupted or damaged.
    /// Invalid files are retained for diagnosis instead of being overwritten.
    pub fn load_from(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref();
        fs::create_dir_all(root)?;
        forget_written(root);
        let state_path = root.join(STATE_FILE);
        let backup_path = root.join(BACKUP_FILE);
        // A damaged state.json is set aside; `kept` says where.
        let (kept, missing) = match read_state(&state_path, root) {
            Ok(state) => {
                return Self::loaded(Self {
                    from_state_file: true,
                    ..state
                });
            }
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                (Some(quarantine(&state_path)?), false)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (None, true),
            Err(error) => return Err(error),
        };
        let (state, from_backup, kept) = match read_state(&backup_path, root) {
            Ok(state) => (state, true, kept),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                (Self::at_root(root), false, kept)
            }
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                let backup = quarantine(&backup_path)?;
                (Self::at_root(root), false, kept.or(Some(backup)))
            }
            Err(error) => return Err(error),
        };
        // A first launch has neither file, and nothing to tell.
        let first_launch = missing && !from_backup && kept.is_none();
        Self::loaded(Self {
            recovery: (!first_launch).then_some(Recovery { from_backup, kept }),
            ..state
        })
    }

    fn loaded(state: Self) -> io::Result<Self> {
        state.ensure_profile_dirs()?;
        use_search_engine(state.search_engine);
        Ok(state)
    }

    fn at_root(root: &Path) -> Self {
        Self {
            storage_dir: root.to_path_buf(),
            ..Self::default()
        }
    }

    pub fn root_dir(&self) -> &Path {
        &self.storage_dir
    }

    pub fn profile_dir(&self, space_id: SpaceId) -> PathBuf {
        self.storage_dir
            .join(PROFILES_DIR)
            .join(format!("space-{space_id}"))
    }

    pub fn cache_dir(&self, space_id: SpaceId) -> PathBuf {
        self.profile_dir(space_id).join("cache")
    }

    pub fn ensure_profile_dirs(&self) -> io::Result<()> {
        for space in &self.spaces {
            fs::create_dir_all(self.cache_dir(space.id))?;
        }
        Ok(())
    }

    /// Deletes the profiles (cookies, storage, cache) of deleted spaces.
    /// Chromium can hold a profile open until it quits, so a space's data
    /// goes at the next launch: call this before Chromium starts. Only the
    /// session read from state.json is trusted with it, as a backup or the
    /// defaults may lack spaces whose data is still wanted, and only while
    /// no other instance holds the profiles.
    pub fn remove_orphaned_profiles(&self) -> io::Result<()> {
        let profiles = self.storage_dir.join(PROFILES_DIR);
        if !self.from_state_file
            || !fs::symlink_metadata(&profiles).is_ok_and(|profiles| profiles.is_dir())
            || fs::symlink_metadata(profiles.join(CHROMIUM_LOCK)).is_ok()
        {
            return Ok(());
        }
        let spaces = self.spaces.iter().map(|space| space.id).collect();
        for orphan in orphaned_profiles(&profiles, &spaces)? {
            fs::remove_dir_all(orphan)?;
        }
        Ok(())
    }

    pub fn snapshot(&self) -> io::Result<StateSnapshot> {
        Ok(StateSnapshot {
            root: self.storage_dir.clone(),
            bytes: serde_json::to_vec_pretty(self).map_err(io::Error::other)?,
            seq: NEXT_SNAPSHOT.fetch_add(1, Ordering::Relaxed),
        })
    }

    /// Write on the calling thread and flush it to the drive, for quitting.
    /// Ordered after every earlier snapshot, so a pending background write
    /// can't replace it.
    pub fn save(&self) -> io::Result<()> {
        self.snapshot()?.write_with(Durability::Flushed).map(drop)
    }

    pub fn active_space(&self) -> &Space {
        self.spaces
            .iter()
            .find(|space| space.id == self.selected_space_id)
            .unwrap_or(&self.spaces[0])
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.active_space().selected_tab()
    }

    pub fn space(&self, id: SpaceId) -> Option<&Space> {
        self.spaces.iter().find(|space| space.id == id)
    }

    pub fn tab(&self, id: TabId) -> Option<&Tab> {
        self.spaces
            .iter()
            .flat_map(|space| &space.tabs)
            .find(|tab| tab.id == id)
    }

    pub fn space_of_tab(&self, tab_id: TabId) -> Option<SpaceId> {
        self.spaces
            .iter()
            .find(|space| space.tab_index(tab_id).is_some())
            .map(|space| space.id)
    }

    fn space_mut(&mut self, id: SpaceId) -> Option<&mut Space> {
        self.spaces.iter_mut().find(|space| space.id == id)
    }

    fn locate_tab_mut(&mut self, tab_id: TabId) -> Option<(&mut Space, usize)> {
        self.spaces
            .iter_mut()
            .find_map(|space| space.tab_index(tab_id).map(|index| (space, index)))
    }

    pub fn sidebar_width(&self) -> f32 {
        self.sidebar_width
    }

    pub fn set_sidebar_width(&mut self, width: f32) {
        self.sidebar_width = clamp_sidebar_width(width);
    }

    pub fn search_engine(&self) -> SearchEngine {
        self.search_engine
    }

    /// Returns whether the engine changed.
    pub fn set_search_engine(&mut self, engine: SearchEngine) -> bool {
        use_search_engine(engine);
        std::mem::replace(&mut self.search_engine, engine) != engine
    }

    /// Hosts where content blocking is off, as the engine takes them.
    pub fn blocking_exceptions(&self) -> HashSet<String> {
        self.blocking_exceptions.iter().cloned().collect()
    }

    /// Whether blocking is off on `host`: an exception names it or a domain
    /// above it, which is how the content blocker applies them.
    pub fn is_blocking_disabled(&self, host: &str) -> bool {
        normalize_host(host).is_some_and(|host| {
            host_and_parents(&host).any(|host| self.blocking_exceptions.contains(host))
        })
    }

    /// Turns blocking off for `host` and its subdomains, or back on for
    /// `host`. Turning it on lifts exceptions for the domains above `host`
    /// too: they would keep it off. Returns whether the setting changed.
    pub fn set_blocking_disabled(&mut self, host: &str, disabled: bool) -> bool {
        let Some(host) = normalize_host(host) else {
            return false;
        };
        if disabled {
            !self.is_blocking_disabled(&host) && self.blocking_exceptions.insert(host)
        } else {
            let before = self.blocking_exceptions.len();
            self.blocking_exceptions
                .retain(|exception| !host_and_parents(&host).any(|host| host == exception));
            self.blocking_exceptions.len() != before
        }
    }

    fn allocate_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("browser ID space exhausted");
        id
    }

    /// New spaces start empty and selected, with the least-used theme.
    pub fn create_space(&mut self, name: impl Into<String>) -> SpaceId {
        let id = self.allocate_id();
        let name = space_name(&name.into());
        let name = if name.is_empty() {
            self.unused_space_name()
        } else {
            name
        };
        let theme = self.least_used_theme();
        self.spaces.push(Space::new(id, name, theme));
        self.selected_space_id = id;
        id
    }

    /// "Space N" for a space without a name: N counts on from the number of
    /// spaces, past any name already taken, so two spaces never share one.
    fn unused_space_name(&self) -> String {
        let taken = |name: &str| {
            self.spaces
                .iter()
                .any(|space| space.name.eq_ignore_ascii_case(name))
        };
        let mut number = self.spaces.len() + 1;
        loop {
            let name = format!("Space {number}");
            if !taken(&name) {
                return name;
            }
            number += 1;
        }
    }

    fn least_used_theme(&self) -> u8 {
        let mut uses = [0usize; THEME_COUNT as usize];
        for space in &self.spaces {
            uses[usize::from(space.theme())] += 1;
        }
        (0..THEME_COUNT)
            .min_by_key(|&theme| uses[usize::from(theme)])
            .unwrap_or(0)
    }

    /// Names a space; text with nothing but blanks keeps the old name.
    pub fn rename_space(&mut self, id: SpaceId, name: impl AsRef<str>) -> bool {
        let name = space_name(name.as_ref());
        if name.is_empty() {
            return false;
        }
        let Some(space) = self.space_mut(id) else {
            return false;
        };
        space.name = name;
        true
    }

    /// Moves a space to `index` in the switcher, the order that ⌃1–9,
    /// ⌥⌘←/→ and swipes follow. Returns whether the order changed.
    pub fn move_space(&mut self, id: SpaceId, index: usize) -> bool {
        let Some(from) = self.spaces.iter().position(|space| space.id == id) else {
            return false;
        };
        let to = index.min(self.spaces.len() - 1);
        if from == to {
            return false;
        }
        let space = self.spaces.remove(from);
        self.spaces.insert(to, space);
        true
    }

    /// Removes a space and its tabs, unless it is the last one. A selected
    /// space hands the selection to the next space, else the previous one.
    pub fn remove_space(&mut self, id: SpaceId) -> Option<Space> {
        if self.spaces.len() < 2 {
            return None;
        }
        let index = self.spaces.iter().position(|space| space.id == id)?;
        let space = self.spaces.remove(index);
        if self.selected_space_id == id {
            self.selected_space_id = self.spaces[index.min(self.spaces.len() - 1)].id;
        }
        // Its closed tabs would reopen in another space's profile.
        self.closed.retain(|closed| closed.space_id != id);
        Some(space)
    }

    pub fn set_space_theme(&mut self, id: SpaceId, theme: u8) -> bool {
        if theme >= THEME_COUNT {
            return false;
        }
        let Some(space) = self.space_mut(id) else {
            return false;
        };
        space.theme = Some(theme);
        true
    }

    /// Keeps the first grapheme; blank text removes the icon.
    pub fn set_space_icon(&mut self, id: SpaceId, icon: Option<&str>) -> bool {
        let Some(space) = self.space_mut(id) else {
            return false;
        };
        space.icon = icon.and_then(sanitize_icon);
        true
    }

    pub fn select_space(&mut self, id: SpaceId) -> bool {
        if self.space(id).is_none() {
            return false;
        }
        self.selected_space_id = id;
        true
    }

    /// Opens a tab at the top of Today, like Arc, and selects it. `url` is
    /// stored as given: a URL from the engine or a saved tab, or address-bar
    /// text already passed through `normalize_address`.
    pub fn new_tab(&mut self, space_id: SpaceId, url: impl AsRef<str>) -> Option<TabId> {
        self.insert_tab(space_id, url.as_ref(), true)
    }

    /// Like `new_tab`, but keeps the current selection unless the space was empty.
    pub fn new_background_tab(&mut self, space_id: SpaceId, url: impl AsRef<str>) -> Option<TabId> {
        self.insert_tab(space_id, url.as_ref(), false)
    }

    fn insert_tab(&mut self, space_id: SpaceId, url: &str, select: bool) -> Option<TabId> {
        let index = self.spaces.iter().position(|space| space.id == space_id)?;
        let id = self.allocate_id();
        let url = url.trim();
        let url = display_url(if url.is_empty() { BLANK_URL } else { url });
        let title = fallback_title(&url);
        let space = &mut self.spaces[index];
        let at = space.section_start(Pin::Today);
        space.tabs.insert(
            at,
            Tab {
                id,
                url,
                title,
                ..Tab::default()
            },
        );
        if select || space.selected_tab_id.is_none() {
            space.selected_tab_id = Some(id);
        }
        Some(id)
    }

    /// Opens a copy of the tab right below it, in its section, under a new
    /// id. The selection stays.
    pub fn duplicate_tab(&mut self, tab_id: TabId) -> Option<TabId> {
        self.space_of_tab(tab_id)?;
        let id = self.allocate_id();
        let (space, index) = self.locate_tab_mut(tab_id)?;
        let original = &space.tabs[index];
        let copy = Tab {
            id,
            url: original.url.clone(),
            title: original.title.clone(),
            pin: original.pin,
            custom_title: None,
            pinned_url: original.pinned_url.clone(),
        };
        space.tabs.insert(index + 1, copy);
        Some(id)
    }

    /// The tab's place in its section, top to bottom.
    pub fn section_index(&self, tab_id: TabId) -> Option<usize> {
        let space = self.space(self.space_of_tab(tab_id)?)?;
        let pin = space.tabs[space.tab_index(tab_id)?].pin;
        space.section(pin).position(|tab| tab.id == tab_id)
    }

    /// Names the tab, or with no name (or a blank one, or the page's title)
    /// lets it follow its page's title again. Returns whether the name
    /// changed.
    pub fn rename_tab(&mut self, tab_id: TabId, name: Option<&str>) -> bool {
        let Some((space, index)) = self.locate_tab_mut(tab_id) else {
            return false;
        };
        let tab = &mut space.tabs[index];
        let name = name
            .map(str::trim)
            .filter(|name| !name.is_empty() && *name != tab.title)
            .map(str::to_owned);
        if tab.custom_title == name {
            return false;
        }
        tab.custom_title = name;
        true
    }

    /// Selects the tab and its space.
    pub fn select_tab(&mut self, space_id: SpaceId, tab_id: TabId) -> bool {
        let selected = self.select_tab_in_space(space_id, tab_id);
        if selected {
            self.selected_space_id = space_id;
        }
        selected
    }

    /// Selects the tab within its space only: the active space stays.
    pub fn select_tab_in_space(&mut self, space_id: SpaceId, tab_id: TabId) -> bool {
        let Some(space) = self.space_mut(space_id) else {
            return false;
        };
        if space.tab_index(tab_id).is_none() {
            return false;
        }
        space.selected_tab_id = Some(tab_id);
        true
    }

    /// Returns the removed tab. If it was selected, the next tab of its
    /// section is selected, else the previous one there, else the nearest
    /// tab in sidebar order, else nothing.
    pub fn close_tab(&mut self, space_id: SpaceId, tab_id: TabId) -> Option<Tab> {
        let space = self.space_mut(space_id)?;
        let index = space.tab_index(tab_id)?;
        Some(space.remove_at(index))
    }

    /// Moves the tab to the end of the target section, or to the top of
    /// Today, where new tabs go. Keeps its position if the section is unchanged.
    pub fn set_pin(&mut self, tab_id: TabId, pin: Pin) -> bool {
        let Some((space, index)) = self.locate_tab_mut(tab_id) else {
            return false;
        };
        if space.tabs[index].pin == pin {
            return true;
        }
        let mut tab = space.tabs.remove(index);
        tab.repin(pin);
        let at = if pin == Pin::Today {
            space.section_start(pin)
        } else {
            space.section_end(pin)
        };
        space.tabs.insert(at, tab);
        true
    }

    /// Drag and drop within a space: moves the tab into `pin`'s section,
    /// before `before` (a tab of that section) or at the section's end.
    pub fn move_tab(&mut self, tab_id: TabId, before: Option<TabId>, pin: Pin) -> bool {
        let Some((space, index)) = self.locate_tab_mut(tab_id) else {
            return false;
        };
        if let Some(before) = before {
            if before == tab_id {
                return space.tabs[index].pin == pin;
            }
            if !space.section(pin).any(|tab| tab.id == before) {
                return false;
            }
        }
        let mut tab = space.tabs.remove(index);
        tab.repin(pin);
        let at = before
            .and_then(|before| space.tab_index(before))
            .unwrap_or_else(|| space.section_end(pin));
        space.tabs.insert(at, tab);
        true
    }

    /// Moves a tab to the same section of another space under a new id, as
    /// its page opens again there, in that space's profile: Today tabs at
    /// the top of Today, pinned tabs and favorites at the end of theirs.
    /// Its own space hands the selection on as when the tab is closed; the
    /// other space keeps its selection unless it had no tabs.
    pub fn move_tab_to_space(&mut self, tab_id: TabId, target: SpaceId) -> Option<TabId> {
        let source = self.space_of_tab(tab_id)?;
        if source == target || self.space(target).is_none() {
            return None;
        }
        let id = self.allocate_id();
        let space = self.space_mut(source)?;
        let index = space.tab_index(tab_id)?;
        let mut tab = space.remove_at(index);
        tab.id = id;
        let space = self.space_mut(target)?;
        let at = match tab.pin {
            Pin::Today => space.section_start(Pin::Today),
            pin => space.section_end(pin),
        };
        space.tabs.insert(at, tab);
        space.selected_tab_id.get_or_insert(id);
        Some(id)
    }

    /// Removes every Today tab. A selected Today tab passes the selection to
    /// the first remaining tab, if any.
    pub fn clear_today(&mut self, space_id: SpaceId) -> Vec<Tab> {
        let Some(space) = self.space_mut(space_id) else {
            return Vec::new();
        };
        let start = space.section_start(Pin::Today);
        let removed: Vec<Tab> = space.tabs.drain(start..).collect();
        if removed
            .iter()
            .any(|tab| Some(tab.id) == space.selected_tab_id)
        {
            space.selected_tab_id = space.tabs.first().map(|tab| tab.id);
        }
        removed
    }

    /// Closes the space's Today tabs among `ids`, as `close_tab` would one
    /// by one. Returns them top first, each with its place in Today before
    /// any of them closed.
    pub fn close_today_tabs(&mut self, space_id: SpaceId, ids: &[TabId]) -> Vec<(Tab, usize)> {
        let Some(space) = self.space_mut(space_id) else {
            return Vec::new();
        };
        let places: Vec<(TabId, usize)> = space
            .section(Pin::Today)
            .enumerate()
            .filter(|(_, tab)| ids.contains(&tab.id))
            .map(|(place, tab)| (tab.id, place))
            .collect();
        places
            .into_iter()
            .filter_map(|(id, place)| Some((space.remove_at(space.tab_index(id)?), place)))
            .collect()
    }

    /// Browser callbacks can update hidden tabs without selecting their space.
    /// Returns whether the tab's URL or title changed, so unchanged reloads
    /// and repeated titles need no save.
    pub fn update_tab(&mut self, id: TabId, url: impl AsRef<str>, title: impl AsRef<str>) -> bool {
        let Some((space, index)) = self.locate_tab_mut(id) else {
            return false;
        };
        let tab = &mut space.tabs[index];
        let url = url.as_ref();
        let title = title.as_ref();
        let new_url = display_url(url);
        let new_title = if title.trim().is_empty() || title == url {
            fallback_title(&new_url)
        } else {
            title.to_owned()
        };
        let changed = tab.url != new_url || tab.title != new_title;
        tab.url = new_url;
        tab.title = new_title;
        changed
    }

    /// Recently closed tabs, the most recent last.
    pub fn closed_tabs(&self) -> &[ClosedTab] {
        &self.closed
    }

    /// Remembers a closed tab for reopening, with no known place.
    #[cfg(test)]
    pub fn remember_closed(&mut self, space_id: SpaceId, tab: Tab) {
        self.remember_closed_at(space_id, tab, None);
    }

    /// Remembers a closed tab for reopening at `index` in its section.
    pub fn remember_closed_at(&mut self, space_id: SpaceId, tab: Tab, index: Option<usize>) {
        self.closed.push(ClosedTab {
            space_id,
            tab,
            index,
            batch: None,
        });
        self.trim_closed();
    }

    /// Remembers tabs closed together, each with its place in its section,
    /// as one close that ⇧⌘T undoes at once. Pushed bottom first, so the
    /// library, newest first, lists them in sidebar order.
    pub fn remember_closed_together(&mut self, space_id: SpaceId, tabs: Vec<(Tab, usize)>) {
        let batch = tabs.first().map(|(tab, _)| tab.id);
        self.closed
            .extend(tabs.into_iter().rev().map(|(tab, index)| ClosedTab {
                space_id,
                tab,
                index: Some(index),
                batch,
            }));
        self.trim_closed();
    }

    /// Keeps each space's `REOPEN_LIMIT` most recent closes, so one space
    /// never pushes another's out. Tabs closed together count once, and
    /// stay or go together.
    fn trim_closed(&mut self) {
        let mut closes: HashMap<SpaceId, usize> = HashMap::new();
        let mut newer_batch = None;
        let mut keep = Vec::with_capacity(self.closed.len());
        for closed in self.closed.iter().rev() {
            let count = closes.entry(closed.space_id).or_default();
            if closed.batch.is_none() || closed.batch != newer_batch {
                *count += 1;
            }
            newer_batch = closed.batch;
            keep.push(*count <= REOPEN_LIMIT);
        }
        let mut keep = keep.into_iter().rev();
        self.closed.retain(|_| keep.next().unwrap_or(false));
    }

    /// Takes a closed tab, by default the most recent one, off the list.
    pub fn take_closed(&mut self, index: Option<usize>) -> Option<ClosedTab> {
        let index = index.or(self.closed.len().checked_sub(1))?;
        (index < self.closed.len()).then(|| self.closed.remove(index))
    }

    /// How many tabs the most recent close took: one, or all the tabs closed
    /// together with it.
    pub fn last_closed_count(&self) -> usize {
        let Some(last) = self.closed.last() else {
            return 0;
        };
        match last.batch {
            Some(batch) => self
                .closed
                .iter()
                .rev()
                .take_while(|closed| closed.batch == Some(batch))
                .count(),
            None => 1,
        }
    }

    /// Takes the most recent close off the list: one tab, or all the tabs
    /// closed together with it.
    pub fn take_last_closed(&mut self) -> Vec<ClosedTab> {
        let start = self.closed.len() - self.last_closed_count();
        self.closed.split_off(start)
    }

    /// Puts a closed tab back in its space under a new id: in its section,
    /// at its old place there (or as near as the section allows), else where
    /// new tabs of that section go. The selection stays unless the space
    /// was empty.
    pub fn reopen_closed(&mut self, closed: ClosedTab) -> Option<TabId> {
        self.space(closed.space_id)?;
        let id = self.allocate_id();
        let space = self.space_mut(closed.space_id)?;
        let pin = closed.tab.pin;
        let (start, end) = (space.section_start(pin), space.section_end(pin));
        let at = match (closed.index, pin) {
            (Some(index), _) => (start + index).min(end),
            (None, Pin::Today) => start,
            (None, _) => end,
        };
        let mut tab = Tab { id, ..closed.tab };
        tab.repin(pin);
        space.tabs.insert(at, tab);
        space.selected_tab_id.get_or_insert(id);
        Some(id)
    }

    /// Forgets a closed tab that isn't worth reopening.
    pub fn forget_closed(&mut self, tab_id: TabId) {
        self.closed.retain(|closed| closed.tab.id != tab_id);
    }

    /// Forgets every closed tab, with the browsing history. Returns whether
    /// there were any.
    pub fn clear_closed(&mut self) -> bool {
        let had_any = !self.closed.is_empty();
        self.closed.clear();
        had_any
    }

    /// Recent downloads as last saved, the newest first.
    pub fn downloads(&self) -> &[SavedDownload] {
        &self.downloads
    }

    /// Saves the library's downloads, the newest first: every running one
    /// and the `DOWNLOAD_LIMIT` newest others. True if anything changed.
    pub fn set_downloads(&mut self, mut downloads: Vec<SavedDownload>) -> bool {
        trim_downloads(&mut downloads);
        if self.downloads == downloads {
            return false;
        }
        self.downloads = downloads;
        true
    }

    fn validate_and_repair(&mut self) -> io::Result<()> {
        if self.schema_version > SCHEMA_VERSION {
            return Err(newer_schema());
        }
        let mut ids = HashSet::new();
        for id in self
            .spaces
            .iter()
            .flat_map(|space| std::iter::once(space.id).chain(space.tabs.iter().map(|tab| tab.id)))
        {
            if id == 0 || !ids.insert(id) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "duplicate or zero browser ID",
                ));
            }
        }
        let highest_id = ids.into_iter().max().unwrap_or(0);
        self.next_id = self
            .next_id
            .max(highest_id.checked_add(1).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "browser ID space exhausted")
            })?)
            .max(1);
        if self.next_id > u64::MAX - self.spaces.len() as u64 - 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "browser ID space exhausted",
            ));
        }
        self.schema_version = SCHEMA_VERSION;
        if self.spaces.is_empty() {
            self.create_space("Personal");
        }
        for (index, space) in self.spaces.iter_mut().enumerate() {
            space.tabs.sort_by_key(|tab| tab.pin);
            if space.selected_tab().is_none() {
                space.selected_tab_id = space.tabs.first().map(|tab| tab.id);
            }
            // Arc has no blank tabs; drop unnavigated Today tabs as if they
            // were closed. Pinned tabs and favorites are the user's own and
            // keep their place even if a page went to about:blank.
            while let Some(blank) = space
                .tabs
                .iter()
                .position(|tab| tab.pin == Pin::Today && tab.url == BLANK_URL)
            {
                space.remove_at(blank);
            }
            for tab in &mut space.tabs {
                let sanitized = display_url(&tab.url);
                if tab.title == tab.url {
                    tab.title = sanitized.clone();
                }
                tab.url = sanitized;
                // Tabs pinned before pinned addresses were kept come back
                // to where they are now.
                tab.pinned_url = tab.pinned_url.as_deref().map(display_url);
                tab.repin(tab.pin);
                tab.custom_title = tab
                    .custom_title
                    .take()
                    .filter(|name| !name.trim().is_empty());
            }
            let position_theme = (index % usize::from(THEME_COUNT)) as u8;
            space.theme = Some(space.theme.unwrap_or(position_theme) % THEME_COUNT);
            space.icon = space.icon.as_deref().and_then(sanitize_icon);
            // Older versions kept pasted tabs and line breaks.
            space.name = space_name(&space.name);
            if space.name.is_empty() {
                space.name = format!("Space {}", index + 1);
            }
        }
        if self.space(self.selected_space_id).is_none() {
            self.selected_space_id = self.spaces[0].id;
        }
        self.sidebar_width = clamp_sidebar_width(self.sidebar_width);
        // A closed tab of a space that is gone (in a backup, say) must not
        // reopen in another space's profile.
        let spaces: HashSet<SpaceId> = self.spaces.iter().map(|space| space.id).collect();
        self.closed
            .retain(|closed| spaces.contains(&closed.space_id));
        self.trim_closed();
        self.blocking_exceptions = std::mem::take(&mut self.blocking_exceptions)
            .iter()
            .filter_map(|host| normalize_host(host))
            .collect();
        // A download still running when the app last stopped (a crash, a
        // quit from the Dock) never finished.
        for download in &mut self.downloads {
            if download.state == DownloadState::Running {
                download.state = DownloadState::Failed;
                download.error = Some("Interrupted".into());
            }
            download.url = display_url(&download.url);
        }
        trim_downloads(&mut self.downloads);
        Ok(())
    }
}

/// Keeps every running download and the `DOWNLOAD_LIMIT` newest others.
fn trim_downloads(downloads: &mut Vec<SavedDownload>) {
    let mut others = 0;
    downloads.retain(|download| {
        if download.state == DownloadState::Running {
            return true;
        }
        others += 1;
        others <= DOWNLOAD_LIMIT
    });
}

fn clamp_sidebar_width(width: f32) -> f32 {
    if width.is_finite() {
        width.clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH)
    } else {
        DEFAULT_SIDEBAR_WIDTH
    }
}

fn fallback_title(url: &str) -> String {
    if url == BLANK_URL {
        "New Tab".into()
    } else {
        url.to_owned()
    }
}

/// The space whose profile `name` is, for names `profile_dir` gives only:
/// "space-007" and "space-+7" are someone else's.
fn profile_space_id(name: &str) -> Option<SpaceId> {
    let id = name.strip_prefix("space-")?.parse().ok()?;
    (name == format!("space-{id}")).then_some(id)
}

/// Profile directories in `profiles` of no space in `spaces`: directories
/// right inside it, never symlinks, named as `profile_dir` names them.
fn orphaned_profiles(profiles: &Path, spaces: &HashSet<SpaceId>) -> io::Result<Vec<PathBuf>> {
    let mut orphans = Vec::new();
    for entry in fs::read_dir(profiles)? {
        let entry = entry?;
        // Unlike `Path::is_dir`, this doesn't follow symlinks.
        if entry.file_type()?.is_dir()
            && entry
                .file_name()
                .to_str()
                .and_then(profile_space_id)
                .is_some_and(|id| !spaces.contains(&id))
        {
            orphans.push(entry.path());
        }
    }
    Ok(orphans)
}

/// A space's name on one line: tabs and line breaks from a paste, and runs
/// of blanks, become single spaces; other control characters go.
fn space_name(name: &str) -> String {
    name.split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|ch| !ch.is_control())
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn sanitize_icon(icon: &str) -> Option<String> {
    icon.trim()
        .graphemes(true)
        .next()
        .filter(|grapheme| !grapheme.chars().any(char::is_control))
        .map(str::to_owned)
}

fn read_state(path: &Path, root: &Path) -> io::Result<BrowserState> {
    /// Only the version: a newer schema may change any other field, and
    /// must be refused rather than treated as damaged and replaced.
    #[derive(Deserialize)]
    struct Header {
        #[serde(default = "schema_version")]
        schema_version: u32,
    }
    let bytes = fs::read(path)?;
    let invalid = |error| io::Error::new(io::ErrorKind::InvalidData, error);
    let header: Header = serde_json::from_slice(&bytes).map_err(invalid)?;
    if header.schema_version > SCHEMA_VERSION {
        return Err(newer_schema());
    }
    let mut state: BrowserState = serde_json::from_slice(&bytes).map_err(invalid)?;
    state.storage_dir = root.to_path_buf();
    state.validate_and_repair()?;
    Ok(state)
}

fn newer_schema() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "this session was created by a newer version of Map Browser",
    )
}

/// Unique within this process even when two threads ask in the same
/// nanosecond, and across processes through the pid.
fn unique_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{timestamp}-{count}", std::process::id())
}

/// Sets a damaged file aside under a new name, which it returns.
fn quarantine(path: &Path) -> io::Result<PathBuf> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let kept = path.with_file_name(format!("{name}.corrupt-{}", unique_suffix()));
    fs::rename(path, &kept)?;
    Ok(kept)
}

/// Replaces a file of the data directory the way routine session saves do:
/// complete or not at all, without flushing the drive.
pub(crate) fn write_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_write(path, bytes, Durability::Ordered)
}

fn atomic_write(path: &Path, bytes: &[u8], durability: Durability) -> io::Result<()> {
    let temporary = path.with_extension(format!("tmp-{}", unique_suffix()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        write_barrier(&file)?;
        fs::rename(&temporary, path)?;
        if durability == Durability::Flushed
            && let Some(parent) = path.parent()
        {
            flush_drive(parent)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Gets `file`'s data to the drive ahead of any later write, the rename
/// included, without waiting for the drive to empty its cache.
#[cfg(target_os = "macos")]
fn write_barrier(file: &File) -> io::Result<()> {
    use std::{ffi::c_int, os::fd::AsRawFd};
    unsafe extern "C" {
        fn fcntl(fd: c_int, command: c_int, ...) -> c_int;
    }
    /// From <sys/fcntl.h>.
    const F_BARRIERFSYNC: c_int = 85;
    // SAFETY: F_BARRIERFSYNC takes no argument, and `file` is open.
    if unsafe { fcntl(file.as_raw_fd(), F_BARRIERFSYNC) } == -1 {
        // File systems without barriers (network, FAT) get a full flush.
        return file.sync_all();
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn write_barrier(file: &File) -> io::Result<()> {
    file.sync_data()
}

/// Flushes the directory's entries and, on macOS (F_FULLFSYNC), everything
/// written so far from the drive's cache to permanent storage.
fn flush_drive(directory: &Path) -> io::Result<()> {
    File::open(directory)?.sync_all()
}

/// Remove authority credentials from the URL kept in tabs, the address bar and disk.
/// The original URL can still be sent directly to Chromium for authentication.
pub fn display_url(url: &str) -> String {
    let authority_start = if let Some(index) = url
        .find(':')
        .filter(|index| url[index + 1..].starts_with("//"))
    {
        index + 3
    } else if url.starts_with("//") {
        2
    } else {
        return url.to_owned();
    };
    let authority_end = url[authority_start..]
        .find(['/', '?', '#'])
        .map(|index| authority_start + index)
        .unwrap_or(url.len());
    let Some(userinfo_end) = url[authority_start..authority_end].rfind('@') else {
        return url.to_owned();
    };
    format!(
        "{}{}",
        &url[..authority_start],
        &url[authority_start + userinfo_end + 1..]
    )
}

/// Convert address-bar text to a navigable URL, using HTTP for local development.
pub fn normalize_address(input: &str) -> String {
    let input = input.trim();
    if input.is_empty() {
        return "about:blank".into();
    }
    if let Some(query) = forced_search(input) {
        return search_url(query);
    }
    web_address(input).unwrap_or_else(|| search_url(input))
}

/// Whether address-bar text is searched for rather than opened as a URL.
pub fn is_search_query(input: &str) -> bool {
    let input = input.trim();
    !input.is_empty() && (forced_search(input).is_some() || web_address(input).is_none())
}

/// What a leading "?" asks to search for, even if it looks like an
/// address, as in Chrome: "?example.com" searches for "example.com".
pub fn forced_search(input: &str) -> Option<&str> {
    input
        .trim()
        .strip_prefix('?')
        .map(str::trim)
        .filter(|query| !query.is_empty())
}

/// The words a search for address-bar text looks for.
pub fn search_text(input: &str) -> &str {
    forced_search(input).unwrap_or(input.trim())
}

/// Host names that only mean something on the local network, as with
/// mDNS (.local) and home routers (.lan, .home.arpa). Pages there rarely
/// have certificates, so they open over HTTP, as Chrome and Safari do.
const LOCAL_SUFFIXES: &[&str] = &[
    ".local",
    ".lan",
    ".home",
    ".home.arpa",
    ".internal",
    ".intranet",
    ".corp",
    ".localdomain",
];

/// The URL that trimmed, non-empty text names, or `None` for a search query.
fn web_address(input: &str) -> Option<String> {
    let lower = input.to_ascii_lowercase();
    // Typed web addresses must be complete: "https://" alone, a space in
    // the host or a port that isn't a number are searched for instead.
    if lower.starts_with("http://") || lower.starts_with("https://") {
        let url = Url::parse(input).ok()?;
        return url
            .host_str()
            .is_some_and(|host| !host.is_empty())
            .then(|| input.to_owned());
    }
    if ["file://", "about:", "data:", "chrome:", "devtools:"]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
    {
        return Some(input.to_owned());
    }
    // These are commands or external protocols, not schemeless HTTP credentials.
    let non_web_scheme = [
        "javascript:",
        "vbscript:",
        "mailto:",
        "tel:",
        "sms:",
        "intent:",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix));
    if non_web_scheme || input.chars().any(char::is_whitespace) {
        return None;
    }
    let schemeless = input.strip_prefix("//").unwrap_or(input);
    let authority = schemeless
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(schemeless);
    // A colon in user:password must not make a URL into a search query.
    let server = authority.rsplit('@').next().unwrap_or(authority);
    let (host, port) = split_port(server);
    // "example.com:abc" is no address.
    if port.is_some_and(|port| port.parse::<u16>().is_err()) {
        return None;
    }
    let host = host.to_ascii_lowercase();
    let has_path = schemeless[authority.len()..].starts_with('/');
    let has_port = port.is_some();
    // IP addresses (routers, NAS boxes, dev servers) and local names are
    // reached over HTTP; Chrome doesn't upgrade them to HTTPS either.
    let ip = host.parse::<std::net::Ipv4Addr>().is_ok()
        || host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .is_some_and(|host| host.parse::<std::net::Ipv6Addr>().is_ok());
    let name = host.trim_end_matches('.');
    if ip
        || name == "localhost"
        || name.ends_with(".localhost")
        || LOCAL_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
    {
        return Some(format!("http://{schemeless}"));
    }
    // "me@example.com" is an email address to search for; a login has a
    // password or comes with a path.
    if authority
        .rsplit_once('@')
        .is_some_and(|(userinfo, _)| !userinfo.contains(':'))
        && !has_path
    {
        return None;
    }
    if name.is_empty() || name.starts_with('[') {
        return None;
    }
    // A real domain suffix opens over HTTPS. Other names are intranet hosts
    // ("router/", "nas:5000", "wiki.corp/page") when they come with a path
    // or port, else words ("node.js", "e.g.", "café") to search for.
    let known_suffix =
        name.contains('.') && psl::suffix(name.as_bytes()).is_some_and(|suffix| suffix.is_known());
    if known_suffix {
        Some(format!("https://{schemeless}"))
    } else if has_path || has_port {
        Some(format!("http://{schemeless}"))
    } else {
        None
    }
}

/// Splits "host:port" and "[ipv6]:port"; the port is `None` without a colon.
fn split_port(server: &str) -> (&str, Option<&str>) {
    if server.starts_with('[') {
        return match server.find(']') {
            Some(end) => {
                let (host, rest) = server.split_at(end + 1);
                (host, rest.strip_prefix(':'))
            }
            None => (server, None),
        };
    }
    match server.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (server, None),
    }
}

/// A search for `input` with the chosen search engine.
pub fn search_url(input: &str) -> String {
    let mut query = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                query.push(byte as char);
            }
            b' ' => query.push('+'),
            _ => {
                use std::fmt::Write;
                let _ = write!(&mut query, "%{byte:02X}");
            }
        }
    }
    format!("{}{query}", current_search_engine().query_url())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("mapbrowser-model-{}", unique_suffix()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn ids<'a>(tabs: impl IntoIterator<Item = &'a Tab>) -> Vec<TabId> {
        tabs.into_iter().map(|tab| tab.id).collect()
    }

    fn sidebar(state: &BrowserState, space: SpaceId) -> Vec<TabId> {
        ids(&state.space(space).unwrap().tabs)
    }

    fn selected(state: &BrowserState, space: SpaceId) -> Option<TabId> {
        state.space(space).unwrap().selected_tab_id
    }

    fn active_id(state: &BrowserState) -> Option<TabId> {
        state.active_tab().map(|tab| tab.id)
    }

    #[test]
    fn spaces_preserve_independent_tab_selection_and_profile_paths() {
        let root = TestDir::new();
        let mut state = BrowserState::load_from(&root.0).unwrap();
        let personal = state.spaces[0].id;
        let work = state.spaces[1].id;
        let personal_tab = state.new_tab(personal, "example.com").unwrap();
        let work_tab = state.new_tab(work, "localhost:8080").unwrap();
        state.select_space(work);
        assert_eq!(active_id(&state), Some(work_tab));
        state.select_space(personal);
        assert_eq!(active_id(&state), Some(personal_tab));
        assert_eq!(state.space_of_tab(work_tab), Some(work));
        assert_eq!(state.space_of_tab(12345), None);
        assert_ne!(state.profile_dir(personal), state.profile_dir(work));
        assert_ne!(state.cache_dir(personal), state.cache_dir(work));
        assert!(state.cache_dir(personal).is_dir());
        let original_path = state.profile_dir(personal);
        state.rename_space(personal, "Renamed");
        assert_eq!(state.profile_dir(personal), original_path);
    }

    #[test]
    fn spaces_start_empty_and_new_tabs_open_at_the_top_of_today() {
        let mut state = BrowserState::default();
        let space = state.selected_space_id;
        assert!(state.active_space().tabs.is_empty());
        assert_eq!(state.active_tab(), None);
        let first = state.new_tab(space, "example.com").unwrap();
        state.set_pin(first, Pin::Pinned);
        let second = state.new_tab(space, "example.org").unwrap();
        let third = state.new_tab(space, "example.net").unwrap();
        assert_eq!(sidebar(&state, space), [first, third, second]);
        assert_eq!(active_id(&state), Some(third));
        let background = state.new_background_tab(space, "rust-lang.org").unwrap();
        assert_eq!(sidebar(&state, space), [first, background, third, second]);
        assert_eq!(active_id(&state), Some(third));
        let work = state.spaces[1].id;
        let only = state.new_background_tab(work, "example.com").unwrap();
        assert_eq!(selected(&state, work), Some(only));
        assert_eq!(state.selected_space_id, space);
        assert_eq!(state.new_tab(12345, "example.com"), None);
    }

    #[test]
    fn closing_the_selected_tab_prefers_its_own_section() {
        let mut state = BrowserState::default();
        let space = state.selected_space_id;
        let favorite = state.new_tab(space, "a.com").unwrap();
        let pinned_a = state.new_tab(space, "b.com").unwrap();
        let pinned_b = state.new_tab(space, "c.com").unwrap();
        let today_old = state.new_tab(space, "d.com").unwrap();
        let today_new = state.new_tab(space, "e.com").unwrap();
        state.set_pin(favorite, Pin::Favorite);
        state.set_pin(pinned_a, Pin::Pinned);
        state.set_pin(pinned_b, Pin::Pinned);
        assert_eq!(
            sidebar(&state, space),
            [favorite, pinned_a, pinned_b, today_new, today_old]
        );

        // Next tab in the same section.
        state.select_tab(space, today_new);
        assert_eq!(state.close_tab(space, today_new).unwrap().id, today_new);
        assert_eq!(active_id(&state), Some(today_old));
        // Previous tab in the same section when it was the last one there.
        state.select_tab(space, pinned_b);
        state.close_tab(space, pinned_b);
        assert_eq!(active_id(&state), Some(pinned_a));
        // Closing a tab that isn't selected keeps the selection.
        state.close_tab(space, favorite);
        assert_eq!(active_id(&state), Some(pinned_a));
        // An emptied section hands over to the nearest tab in sidebar order.
        state.close_tab(space, pinned_a);
        assert_eq!(active_id(&state), Some(today_old));
        state.set_pin(today_old, Pin::Pinned);
        let today = state.new_tab(space, "f.com").unwrap();
        state.close_tab(space, today);
        assert_eq!(active_id(&state), Some(today_old));
        // The last tab leaves an empty space with nothing selected.
        state.close_tab(space, today_old);
        assert_eq!(active_id(&state), None);
        assert!(state.active_space().tabs.is_empty());
        assert_eq!(state.close_tab(space, today_old), None);
        assert_eq!(state.close_tab(12345, today_old), None);
    }

    #[test]
    fn successor_follows_the_close_selection_rules_without_removing() {
        let mut state = BrowserState::default();
        let space = state.selected_space_id;
        let pinned = state.new_tab(space, "a.com").unwrap();
        state.set_pin(pinned, Pin::Pinned);
        let older = state.new_tab(space, "b.com").unwrap();
        let newer = state.new_tab(space, "c.com").unwrap();
        let active = state.active_space();
        assert_eq!(active.successor_of(newer), Some(older));
        assert_eq!(active.successor_of(older), Some(newer));
        assert_eq!(active.successor_of(pinned), Some(newer));
        assert_eq!(active.successor_of(12345), None);
        assert_eq!(sidebar(&state, space), [pinned, newer, older]);
        assert_eq!(active_id(&state), Some(newer));
    }

    #[test]
    fn sections_follow_pins_and_set_pin_moves_to_the_section_end() {
        let mut state = BrowserState::default();
        let space = state.selected_space_id;
        let a = state.new_tab(space, "a.com").unwrap();
        let b = state.new_tab(space, "b.com").unwrap();
        let c = state.new_tab(space, "c.com").unwrap();
        let d = state.new_tab(space, "d.com").unwrap();
        assert!(state.set_pin(a, Pin::Pinned));
        assert!(state.set_pin(c, Pin::Pinned));
        assert!(state.set_pin(b, Pin::Favorite));
        assert_eq!(sidebar(&state, space), [b, a, c, d]);
        let active = state.active_space();
        assert_eq!(ids(active.section(Pin::Favorite)), [b]);
        assert_eq!(ids(active.section(Pin::Pinned)), [a, c]);
        assert_eq!(ids(active.section(Pin::Today)), [d]);
        // Unpinning returns a tab to the top of Today.
        assert!(state.set_pin(a, Pin::Today));
        assert_eq!(sidebar(&state, space), [b, c, a, d]);
        // Re-applying the current section keeps the position.
        assert!(state.set_pin(d, Pin::Today));
        assert_eq!(sidebar(&state, space), [b, c, a, d]);
        assert_eq!(active_id(&state), Some(d));
        assert!(!state.set_pin(12345, Pin::Pinned));
    }

    #[test]
    fn move_tab_reorders_within_and_between_sections_of_one_space() {
        let mut state = BrowserState::default();
        let space = state.selected_space_id;
        let a = state.new_tab(space, "a.com").unwrap();
        let b = state.new_tab(space, "b.com").unwrap();
        let c = state.new_tab(space, "c.com").unwrap();
        assert_eq!(sidebar(&state, space), [c, b, a]);
        assert!(state.move_tab(c, None, Pin::Today));
        assert_eq!(sidebar(&state, space), [b, a, c]);
        assert!(state.move_tab(c, Some(b), Pin::Today));
        assert_eq!(sidebar(&state, space), [c, b, a]);
        assert!(state.move_tab(a, None, Pin::Pinned));
        assert!(state.move_tab(b, Some(a), Pin::Pinned));
        assert_eq!(sidebar(&state, space), [b, a, c]);
        assert_eq!(ids(state.active_space().section(Pin::Pinned)), [b, a]);
        assert!(state.move_tab(c, None, Pin::Favorite));
        assert_eq!(sidebar(&state, space), [c, b, a]);
        assert!(state.move_tab(a, Some(a), Pin::Pinned));
        // `before` must be another tab in the target section of the same space.
        assert!(!state.move_tab(a, Some(c), Pin::Pinned));
        assert!(!state.move_tab(a, Some(a), Pin::Today));
        let work = state.spaces[1].id;
        let other = state.new_tab(work, "other.com").unwrap();
        assert!(!state.move_tab(a, Some(other), Pin::Today));
        assert_eq!(sidebar(&state, space), [c, b, a]);
        assert_eq!(sidebar(&state, work), [other]);
        assert!(!state.move_tab(12345, None, Pin::Today));
    }

    #[test]
    fn clear_today_keeps_pinned_tabs_and_moves_the_selection() {
        let mut state = BrowserState::default();
        let space = state.selected_space_id;
        let pinned = state.new_tab(space, "a.com").unwrap();
        state.set_pin(pinned, Pin::Pinned);
        let today_a = state.new_tab(space, "b.com").unwrap();
        let today_b = state.new_tab(space, "c.com").unwrap();
        let removed = state.clear_today(space);
        assert_eq!(ids(&removed), [today_b, today_a]);
        assert_eq!(sidebar(&state, space), [pinned]);
        assert_eq!(active_id(&state), Some(pinned));

        let today = state.new_tab(space, "d.com").unwrap();
        state.select_tab(space, pinned);
        assert_eq!(ids(&state.clear_today(space)), [today]);
        assert_eq!(active_id(&state), Some(pinned));

        state.set_pin(pinned, Pin::Today);
        state.clear_today(space);
        assert_eq!(active_id(&state), None);
        assert!(state.clear_today(space).is_empty());
        assert!(state.clear_today(12345).is_empty());
    }

    #[test]
    fn new_spaces_are_empty_selected_and_use_the_least_used_theme() {
        let mut state = BrowserState::default();
        assert_eq!(state.spaces[0].theme(), 0);
        assert_eq!(state.spaces[1].theme(), 1);
        let research = state.create_space("  Research ");
        let space = state.space(research).unwrap();
        assert_eq!(space.name, "Research");
        assert!(space.tabs.is_empty());
        assert_eq!(space.selected_tab_id, None);
        assert_eq!(space.theme(), 2);
        assert_eq!(state.selected_space_id, research);
        assert_eq!(state.active_tab(), None);

        assert!(state.set_space_theme(state.spaces[0].id, 3));
        assert!(!state.set_space_theme(state.spaces[0].id, THEME_COUNT));
        assert!(!state.set_space_theme(12345, 0));
        // Theme 0 is unused again.
        let next = state.create_space("");
        assert_eq!(state.space(next).unwrap().theme(), 0);
        assert_eq!(state.space(next).unwrap().name, "Space 4");
        for _ in 0..4 {
            state.create_space("More");
        }
        let themes = state
            .spaces
            .iter()
            .map(Space::theme)
            .collect::<HashSet<_>>();
        assert_eq!(themes.len(), usize::from(THEME_COUNT));
        // With every theme used once, the lowest index wins.
        let ninth = state.create_space("Ninth");
        assert_eq!(state.space(ninth).unwrap().theme(), 0);
    }

    #[test]
    fn space_icons_keep_one_grapheme() {
        let mut state = BrowserState::default();
        let space = state.selected_space_id;
        assert!(state.set_space_icon(space, Some(" 👩‍💻 extra")));
        assert_eq!(state.active_space().icon.as_deref(), Some("👩‍💻"));
        assert!(state.set_space_icon(space, Some("   ")));
        assert_eq!(state.active_space().icon, None);
        assert!(state.set_space_icon(space, Some("W")));
        assert!(state.set_space_icon(space, None));
        assert_eq!(state.active_space().icon, None);
        assert!(!state.set_space_icon(12345, Some("W")));
    }

    #[test]
    fn invalid_selection_does_not_change_current_space_or_tab() {
        let mut state = BrowserState::default();
        let active_space = state.selected_space_id;
        let active_tab = state.new_tab(active_space, "example.com").unwrap();
        let other_space = state.spaces[1].id;
        assert!(!state.select_space(12345));
        assert!(!state.select_tab(other_space, active_tab));
        assert!(!state.select_tab(12345, active_tab));
        assert!(!state.rename_space(active_space, "  "));
        assert_eq!(state.selected_space_id, active_space);
        assert_eq!(active_id(&state), Some(active_tab));
    }

    #[test]
    fn hidden_tab_updates_do_not_switch_spaces() {
        let mut state = BrowserState::default();
        let selected_space = state.selected_space_id;
        let hidden_tab = state.new_tab(state.spaces[1].id, "example.com").unwrap();
        state.update_tab(hidden_tab, "https://example.com/chat", "Chat (1)");
        assert_eq!(state.selected_space_id, selected_space);
        assert_eq!(state.tab(hidden_tab).unwrap().title, "Chat (1)");
        state.update_tab(hidden_tab, "https://example.com/chat", " ");
        assert_eq!(
            state.tab(hidden_tab).unwrap().title,
            "https://example.com/chat"
        );
    }

    #[test]
    fn save_and_reload_preserves_spaces_tabs_settings_and_id_allocation() {
        let root = TestDir::new();
        let mut state = BrowserState::load_from(&root.0).unwrap();
        let space = state.create_space("Research");
        let tab = state.new_tab(space, "rust browser").unwrap();
        state.update_tab(tab, "https://example.com", "Example");
        let favorite = state.new_tab(space, "example.org").unwrap();
        state.set_pin(favorite, Pin::Favorite);
        state.set_space_icon(space, Some("🔬"));
        state.set_space_theme(space, 5);
        state.sidebar_hidden = true;
        state.set_sidebar_width(300.);
        state.set_blocking_disabled("www.Example.com", true);
        state.save().unwrap();
        let mut loaded = BrowserState::load_from(&root.0).unwrap();
        assert_eq!(loaded.spaces, state.spaces);
        assert_eq!(loaded.selected_space_id, space);
        assert_eq!(loaded.root_dir(), root.0);
        assert!(loaded.sidebar_hidden);
        assert_eq!(loaded.sidebar_width(), 300.);
        assert!(loaded.is_blocking_disabled("example.com"));
        assert!(loaded.new_tab(space, "example.net").unwrap() > favorite);
        let json = fs::read_to_string(root.0.join(STATE_FILE)).unwrap();
        assert!(!json.contains("storage_dir"));
        assert!(json.contains("\"pin\": \"favorite\""));
        assert!(json.contains("\"schema_version\": 2"));
    }

    #[test]
    fn version_1_sessions_migrate_without_blank_tabs() {
        let root = TestDir::new();
        let mut spaces = vec![
            serde_json::json!({
                "id": 1, "name": "Personal", "selected_tab_id": 15,
                "tabs": [
                    { "id": 13, "url": "https://news.ycombinator.com/", "title": "Hacker News" },
                    { "id": 14, "url": "https://example.com/", "title": "Example" },
                    { "id": 15, "url": "about:blank", "title": "New tab" }
                ]
            }),
            serde_json::json!({
                "id": 3, "name": "Work", "selected_tab_id": 4,
                "tabs": [ { "id": 4, "url": "about:blank", "title": "New tab" } ]
            }),
        ];
        for id in 20..27 {
            spaces.push(serde_json::json!({
                "id": id, "name": format!("Space {id}"), "selected_tab_id": 99, "tabs": []
            }));
        }
        let v1 = serde_json::json!({
            "schema_version": 1, "spaces": spaces, "selected_space_id": 1, "next_id": 100
        });
        fs::write(root.0.join(STATE_FILE), v1.to_string()).unwrap();
        let mut state = BrowserState::load_from(&root.0).unwrap();
        assert_eq!(state.schema_version, SCHEMA_VERSION);
        assert_eq!(sidebar(&state, 1), [13, 14]);
        assert!(state.spaces[0].tabs.iter().all(|tab| tab.pin == Pin::Today));
        // The selected blank tab is closed like any other: its neighbour takes over.
        assert_eq!(active_id(&state), Some(14));
        assert!(state.space(3).unwrap().tabs.is_empty());
        assert_eq!(selected(&state, 3), None);
        assert_eq!(selected(&state, 20), None);
        let themes = state.spaces.iter().map(Space::theme).collect::<Vec<_>>();
        assert_eq!(themes, [0, 1, 2, 3, 4, 5, 6, 7, 0]);
        assert!(state.spaces.iter().all(|space| space.icon.is_none()));
        assert!(!state.sidebar_hidden);
        assert_eq!(state.sidebar_width(), DEFAULT_SIDEBAR_WIDTH);
        assert_eq!(state.new_tab(1, "example.org"), Some(100));
        state.save().unwrap();
        let json = fs::read_to_string(root.0.join(STATE_FILE)).unwrap();
        assert!(json.contains("\"schema_version\": 2"));
        assert!(!json.contains("about:blank"));
    }

    #[test]
    fn background_writes_never_replace_a_newer_snapshot() {
        let root = TestDir::new();
        let mut state = BrowserState::load_from(&root.0).unwrap();
        let space = state.selected_space_id;
        let saved_name = || {
            BrowserState::load_from(&root.0)
                .unwrap()
                .active_space()
                .name
                .clone()
        };

        state.rename_space(space, "Older");
        let older = state.snapshot().unwrap();
        state.rename_space(space, "Newer");
        let newer = state.snapshot().unwrap();
        assert!(newer.write().unwrap());
        assert!(!older.write().unwrap());
        assert_eq!(saved_name(), "Newer");

        let snapshots = (0..8)
            .map(|n| {
                state.rename_space(space, format!("Name {n}"));
                state.snapshot().unwrap()
            })
            .collect::<Vec<_>>();
        std::thread::scope(|scope| {
            for snapshot in snapshots.iter().rev() {
                scope.spawn(move || snapshot.write().unwrap());
            }
        });
        assert_eq!(saved_name(), "Name 7");

        // The synchronous quit save outranks writes still queued in the background.
        let pending = state.snapshot().unwrap();
        state.rename_space(space, "Quit");
        state.save().unwrap();
        assert!(!pending.write().unwrap());
        assert_eq!(saved_name(), "Quit");
    }

    #[test]
    fn corrupted_main_state_recovers_previous_save() {
        let root = TestDir::new();
        let mut state = BrowserState::load_from(&root.0).unwrap();
        state.rename_space(state.selected_space_id, "Earlier name");
        state.save().unwrap();
        state.rename_space(state.selected_space_id, "Later name");
        state.save().unwrap();
        fs::write(root.0.join(STATE_FILE), "{ partial write").unwrap();
        let recovered = BrowserState::load_from(&root.0).unwrap();
        assert_eq!(recovered.active_space().name, "Earlier name");
        assert!(fs::read_dir(&root.0).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("state.json.corrupt-")
        }));
        recovered.save().unwrap();
        assert_eq!(
            BrowserState::load_from(&root.0)
                .unwrap()
                .active_space()
                .name,
            "Earlier name"
        );
    }

    #[test]
    fn corrupt_state_without_backup_starts_with_defaults_and_retains_bad_file() {
        let root = TestDir::new();
        fs::write(root.0.join(STATE_FILE), "broken").unwrap();
        let state = BrowserState::load_from(&root.0).unwrap();
        assert_eq!(state.spaces.len(), 2);
        assert!(!root.0.join(STATE_FILE).exists());
        assert_eq!(
            fs::read_dir(&root.0)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| { entry.file_name().to_string_lossy().contains("corrupt-") })
                .count(),
            1
        );
    }

    #[test]
    fn load_repairs_stale_selection_sections_themes_and_next_id() {
        let root = TestDir::new();
        let mut state = BrowserState::default();
        let personal = state.spaces[0].id;
        let work = state.spaces[1].id;
        let pinned = state.new_tab(personal, "a.com").unwrap();
        let today = state.new_tab(personal, "b.com").unwrap();
        let other = state.new_tab(work, "c.com").unwrap();
        state.selected_space_id = 9999;
        state.spaces[0].selected_tab_id = Some(9999);
        // Out of section order, as a hand-edited file might be.
        assert_eq!(sidebar(&state, personal), [today, pinned]);
        state.spaces[0].tabs[1].pin = Pin::Pinned;
        state.spaces[0].theme = Some(11);
        state.spaces[0].icon = Some("\u{7}".into());
        state.spaces[1].selected_tab_id = None;
        state.sidebar_width = 9000.;
        state.blocking_exceptions = ["WWW.Example.com".to_owned(), " ".to_owned()].into();
        state.next_id = 0;
        fs::write(root.0.join(STATE_FILE), serde_json::to_vec(&state).unwrap()).unwrap();
        let mut loaded = BrowserState::load_from(&root.0).unwrap();
        assert_eq!(loaded.selected_space_id, personal);
        assert_eq!(sidebar(&loaded, personal), [pinned, today]);
        assert_eq!(active_id(&loaded), Some(pinned));
        assert_eq!(selected(&loaded, work), Some(other));
        assert_eq!(loaded.spaces[0].theme, Some(3));
        assert_eq!(loaded.spaces[0].icon, None);
        assert_eq!(loaded.sidebar_width(), MAX_SIDEBAR_WIDTH);
        assert_eq!(
            loaded.blocking_exceptions,
            BTreeSet::from(["example.com".to_owned()])
        );
        let ids = loaded
            .spaces
            .iter()
            .flat_map(|space| std::iter::once(space.id).chain(space.tabs.iter().map(|tab| tab.id)))
            .collect::<HashSet<_>>();
        let new_space = loaded.create_space("New");
        assert!(!ids.contains(&new_space));
    }

    #[test]
    fn sidebar_width_is_clamped() {
        let mut state = BrowserState::default();
        assert_eq!(state.sidebar_width(), DEFAULT_SIDEBAR_WIDTH);
        state.set_sidebar_width(100.);
        assert_eq!(state.sidebar_width(), MIN_SIDEBAR_WIDTH);
        state.set_sidebar_width(250.5);
        assert_eq!(state.sidebar_width(), 250.5);
        state.set_sidebar_width(f32::NAN);
        assert_eq!(state.sidebar_width(), DEFAULT_SIDEBAR_WIDTH);
    }

    #[test]
    fn blocking_exceptions_use_normalized_hosts() {
        let mut state = BrowserState::default();
        assert!(!state.is_blocking_disabled("example.com"));
        assert!(state.set_blocking_disabled("WWW.Example.COM", true));
        assert!(!state.set_blocking_disabled("example.com", true));
        assert!(state.is_blocking_disabled("example.com"));
        assert!(state.is_blocking_disabled("www.example.com"));
        assert_eq!(
            state.blocking_exceptions(),
            HashSet::from(["example.com".to_owned()])
        );
        assert!(!state.set_blocking_disabled("", true));
        assert!(state.set_blocking_disabled("www.example.com", false));
        assert!(!state.set_blocking_disabled("example.com", false));
        assert!(!state.is_blocking_disabled("example.com"));
    }

    #[test]
    fn blocking_exceptions_cover_subdomains_like_the_blocker() {
        let mut state = BrowserState::default();
        assert!(state.set_blocking_disabled("www.nytimes.com", true));
        // The content blocker lets ads through on every subdomain too, and
        // the shield must say so.
        assert!(state.is_blocking_disabled("cooking.nytimes.com"));
        assert!(!state.is_blocking_disabled("notnytimes.com"));
        assert!(!state.is_blocking_disabled("nytimes.com.evil.example"));
        // Already off there: nothing to add.
        assert!(!state.set_blocking_disabled("cooking.nytimes.com", true));
        // Turning blocking back on from a subdomain works, and so lifts the
        // parent's exception.
        assert!(state.set_blocking_disabled("cooking.nytimes.com", false));
        assert!(!state.is_blocking_disabled("cooking.nytimes.com"));
        assert!(!state.is_blocking_disabled("nytimes.com"));
        assert!(state.blocking_exceptions().is_empty());
        // A subdomain's own exception leaves its parent blocked.
        assert!(state.set_blocking_disabled("cooking.nytimes.com", true));
        assert!(!state.is_blocking_disabled("nytimes.com"));
        assert!(!state.set_blocking_disabled("nytimes.com", false));
        assert!(state.is_blocking_disabled("cooking.nytimes.com"));
    }

    #[test]
    fn newer_schema_is_preserved_and_reported_instead_of_reset() {
        let root = TestDir::new();
        let state = BrowserState {
            schema_version: SCHEMA_VERSION + 1,
            ..BrowserState::default()
        };
        let compatible = serde_json::to_vec(&state).unwrap();
        // A newer version may add variants and change types; this one
        // can't be read as version 2 at all.
        let incompatible = serde_json::json!({
            "schema_version": SCHEMA_VERSION + 1,
            "spaces": [{ "id": 1, "name": "x", "tabs": [
                { "id": 2, "url": "u", "title": "t", "pin": "archived" }
            ]}],
            "selected_space_id": "changed"
        })
        .to_string()
        .into_bytes();
        for bytes in [compatible, incompatible] {
            fs::write(root.0.join(STATE_FILE), &bytes).unwrap();
            assert_eq!(
                BrowserState::load_from(&root.0).unwrap_err().kind(),
                io::ErrorKind::Unsupported
            );
            assert_eq!(fs::read(root.0.join(STATE_FILE)).unwrap(), bytes);
            // Writing over it is refused too.
            let older = BrowserState::at_root(&root.0);
            assert_eq!(older.save().unwrap_err().kind(), io::ErrorKind::Unsupported);
            assert_eq!(fs::read(root.0.join(STATE_FILE)).unwrap(), bytes);
            assert_eq!(
                fs::read_dir(&root.0).unwrap().count(),
                1,
                "nothing set aside"
            );
        }
    }

    #[test]
    fn unchanged_state_is_not_written_again() {
        let root = TestDir::new();
        let mut state = BrowserState::load_from(&root.0).unwrap();
        state.rename_space(state.selected_space_id, "Saved");
        assert!(state.snapshot().unwrap().write().unwrap());
        // Mark the file: an unchanged snapshot must not touch it.
        let path = root.0.join(STATE_FILE);
        let marked = [fs::read(&path).unwrap(), b" ".to_vec()].concat();
        fs::write(&path, &marked).unwrap();
        assert!(!state.snapshot().unwrap().write().unwrap());
        state.save().unwrap();
        assert_eq!(fs::read(&path).unwrap(), marked);
        // A real change is written.
        state.rename_space(state.selected_space_id, "Changed");
        assert!(state.snapshot().unwrap().write().unwrap());
        assert_ne!(fs::read(&path).unwrap(), marked);
    }

    #[test]
    fn the_backup_keeps_the_state_found_at_launch() {
        let root = TestDir::new();
        let mut state = BrowserState::load_from(&root.0).unwrap();
        let space = state.selected_space_id;
        state.rename_space(space, "Last session");
        state.save().unwrap();

        let mut state = BrowserState::load_from(&root.0).unwrap();
        for name in ["First edit", "Second edit", "Third edit"] {
            state.rename_space(space, name);
            assert!(state.snapshot().unwrap().write().unwrap());
        }
        let backup = read_state(&root.0.join(BACKUP_FILE), &root.0).unwrap();
        assert_eq!(backup.active_space().name, "Last session");
        assert_eq!(
            BrowserState::load_from(&root.0)
                .unwrap()
                .active_space()
                .name,
            "Third edit"
        );
    }

    #[test]
    fn update_tab_reports_whether_anything_changed() {
        let mut state = BrowserState::default();
        let space = state.selected_space_id;
        let tab = state.new_tab(space, "https://example.com/").unwrap();
        assert!(state.update_tab(tab, "https://example.com/", "Example"));
        assert!(!state.update_tab(tab, "https://example.com/", "Example"));
        // Credentials are never kept, so they are no change either.
        assert!(!state.update_tab(tab, "https://user:pw@example.com/", "Example"));
        assert!(state.update_tab(tab, "https://example.com/next", "Example"));
        // A blank title falls back to the URL, as before.
        assert!(state.update_tab(tab, "https://example.com/next", ""));
        assert!(!state.update_tab(tab, "https://example.com/next", " "));
        assert!(!state.update_tab(12345, "https://example.com/", "Example"));
    }

    #[test]
    fn pages_open_tabs_with_their_urls_as_they_are() {
        let mut state = BrowserState::default();
        let space = state.selected_space_id;
        for url in [
            "blob:https://site.example/6d2c-41f7",
            "https://example.com/a b",
            "file:///tmp/page.html",
        ] {
            let tab = state.new_background_tab(space, url).unwrap();
            assert_eq!(state.tab(tab).unwrap().url, url);
        }
        // Address-bar text is the caller's to normalize first.
        let typed = state
            .new_tab(space, normalize_address("rust gpui"))
            .unwrap();
        assert_eq!(
            state.tab(typed).unwrap().url,
            "https://www.google.com/search?q=rust+gpui"
        );
        let blank = state.new_tab(space, "  ").unwrap();
        assert_eq!(state.tab(blank).unwrap().url, BLANK_URL);
        assert_eq!(state.tab(blank).unwrap().title, "New Tab");
    }

    #[test]
    fn blank_pinned_tabs_and_favorites_survive_loading() {
        let root = TestDir::new();
        let mut state = BrowserState::load_from(&root.0).unwrap();
        let space = state.selected_space_id;
        let favorite = state.new_tab(space, "https://a.example/").unwrap();
        let pinned = state.new_tab(space, "https://b.example/").unwrap();
        let today = state.new_tab(space, "https://c.example/").unwrap();
        state.set_pin(favorite, Pin::Favorite);
        state.set_pin(pinned, Pin::Pinned);
        // Pages can leave for about:blank, e.g. on logging out.
        for tab in [favorite, pinned, today] {
            state.update_tab(tab, BLANK_URL, "");
        }
        state.save().unwrap();
        let loaded = BrowserState::load_from(&root.0).unwrap();
        assert_eq!(sidebar(&loaded, space), [favorite, pinned]);
    }

    #[test]
    fn address_bar_distinguishes_urls_local_servers_and_searches() {
        for (input, expected) in [
            ("  ", "about:blank"),
            ("example.com/path?q=a", "https://example.com/path?q=a"),
            ("https://example.com", "https://example.com"),
            ("HTTP://example.com", "HTTP://example.com"),
            ("localhost:8080/cookies", "http://localhost:8080/cookies"),
            ("LOCALHOST:8080/cookies", "http://LOCALHOST:8080/cookies"),
            ("mapbrowser.localhost", "http://mapbrowser.localhost"),
            ("127.0.0.1:8000", "http://127.0.0.1:8000"),
            ("[::1]:3000", "http://[::1]:3000"),
            ("//example.com/path", "https://example.com/path"),
            ("//localhost:8080/path", "http://localhost:8080/path"),
            (
                "user:password@example.com/path",
                "https://user:password@example.com/path",
            ),
            (
                "user:password@localhost:8080",
                "http://user:password@localhost:8080",
            ),
            (
                "user:password@[::1]:8080",
                "http://user:password@[::1]:8080",
            ),
            ("file:///tmp/welcome.html", "file:///tmp/welcome.html"),
            ("rust gpui", "https://www.google.com/search?q=rust+gpui"),
            ("a&b c?", "https://www.google.com/search?q=a%26b+c%3F"),
            ("café", "https://www.google.com/search?q=caf%C3%A9"),
            // Real domain suffixes, addresses, ports and paths open.
            ("foo.dev", "https://foo.dev"),
            ("Docs.RS/gpui", "https://Docs.RS/gpui"),
            ("bücher.de", "https://bücher.de"),
            ("user.github.io", "https://user.github.io"),
            ("example.com.", "https://example.com."),
            ("localhost:8000", "http://localhost:8000"),
            ("raspberrypi.local", "http://raspberrypi.local"),
            // IP addresses and intranet names open over HTTP.
            ("192.168.1.1", "http://192.168.1.1"),
            ("intranet.corp/wiki", "http://intranet.corp/wiki"),
            ("build.internal:8080", "http://build.internal:8080"),
            // Dotted words and email addresses are searched.
            ("node.js", "https://www.google.com/search?q=node.js"),
            ("next.js", "https://www.google.com/search?q=next.js"),
            ("e.g.", "https://www.google.com/search?q=e.g."),
            (
                "me@gmail.com",
                "https://www.google.com/search?q=me%40gmail.com",
            ),
        ] {
            assert_eq!(normalize_address(input), expected, "input: {input}");
            assert_eq!(
                is_search_query(input),
                expected.starts_with("https://www.google.com/search?q="),
                "input: {input}"
            );
        }
    }

    #[test]
    fn script_and_external_protocol_text_is_not_mistaken_for_http_credentials() {
        for input in [
            "javascript:alert(document.cookie)",
            "javascript:foo@example.com",
            "javascript://example.com/%0Aalert(1)",
            "mailto:person@example.com",
        ] {
            assert!(normalize_address(input).starts_with("https://www.google.com/search?q="));
        }
    }

    #[test]
    fn display_urls_remove_only_authority_credentials() {
        for (input, expected) in [
            (
                "https://fake-user:fake-password@example.com/path?q=a#fragment",
                "https://example.com/path?q=a#fragment",
            ),
            ("HTTP://fake-user@example.com", "HTTP://example.com"),
            (
                "http://user:password@[::1]:8080/path",
                "http://[::1]:8080/path",
            ),
            ("//user:password@example.com", "//example.com"),
            (
                "https://example.com/path?email=person@example.org",
                "https://example.com/path?email=person@example.org",
            ),
            (
                "https://example.com/path@name",
                "https://example.com/path@name",
            ),
            (
                "data:text/plain,https://person@example.org",
                "data:text/plain,https://person@example.org",
            ),
            ("about:blank", "about:blank"),
            ("file:///tmp/welcome.html", "file:///tmp/welcome.html"),
        ] {
            assert_eq!(display_url(input), expected, "input: {input}");
        }
    }

    #[test]
    fn tab_urls_and_fallback_titles_never_persist_authority_credentials() {
        let root = TestDir::new();
        let mut state = BrowserState::load_from(&root.0).unwrap();
        let space = state.selected_space_id;
        let url = "https://fake-user:fake-password@example.com/path";
        let id = state.new_tab(space, url).unwrap();
        let active = state.active_tab().unwrap();
        assert_eq!(active.url, "https://example.com/path");
        assert_eq!(active.title, "https://example.com/path");
        state.update_tab(id, url, url);
        state.save().unwrap();
        state.update_tab(id, url, "Example site");
        state.save().unwrap();
        assert_eq!(state.active_tab().unwrap().title, "Example site");
        for file in [STATE_FILE, BACKUP_FILE] {
            let json = fs::read_to_string(root.0.join(file)).unwrap();
            assert!(!json.contains("fake-user"));
            assert!(!json.contains("fake-password"));
        }
    }

    #[test]
    fn removing_a_space_selects_a_neighbour_and_never_the_last_space() {
        let mut state = BrowserState::default();
        let (personal, work) = (state.spaces[0].id, state.spaces[1].id);
        let research = state.create_space("Research");
        let tab = state.new_tab(personal, "a.com").unwrap();
        let other = state.new_tab(research, "b.com").unwrap();
        // The selected space hands over to the next one.
        state.select_space(work);
        assert_eq!(state.remove_space(work).unwrap().id, work);
        assert_eq!(state.selected_space_id, research);
        // The last in the list hands over to the one before it.
        assert_eq!(state.remove_space(research).unwrap().tabs[0].id, other);
        assert_eq!(state.selected_space_id, personal);
        assert_eq!(active_id(&state), Some(tab));
        // The only space stays.
        assert_eq!(state.remove_space(personal), None);
        assert_eq!(state.remove_space(12345), None);
        assert_eq!(state.spaces.len(), 1);

        // Other spaces keep their selection.
        let mut state = BrowserState::default();
        let (personal, work) = (state.spaces[0].id, state.spaces[1].id);
        let selected = state.new_tab(personal, "a.com").unwrap();
        state.new_background_tab(personal, "b.com");
        assert!(state.remove_space(work).is_some());
        assert_eq!(state.selected_space_id, personal);
        assert_eq!(active_id(&state), Some(selected));
        assert_eq!(state.tab(selected).unwrap().url, "a.com");
    }

    #[test]
    fn profile_names_are_only_the_ones_the_app_gives() {
        assert_eq!(profile_space_id("space-7"), Some(7));
        assert_eq!(
            profile_space_id("space-18446744073709551615"),
            Some(u64::MAX)
        );
        for name in [
            "space-",
            "space-07",
            "space-+7",
            "space--7",
            "space-7 ",
            "space-7a",
            "Space-7",
            "space-18446744073709551616",
            "default",
            "7",
        ] {
            assert_eq!(profile_space_id(name), None, "{name}");
        }
    }

    #[test]
    fn launch_removes_only_profiles_of_deleted_spaces() {
        let root = TestDir::new();
        let mut state = BrowserState::load_from(&root.0).unwrap();
        let (personal, work) = (state.spaces[0].id, state.spaces[1].id);
        let profiles = root.0.join(PROFILES_DIR);
        fs::write(state.cache_dir(work).join("Cookies"), "work").unwrap();
        assert!(state.remove_space(work).is_some());
        state.save().unwrap();
        // Not the app's, or not a space's directory: all stay.
        let outside = TestDir::new();
        fs::write(outside.0.join("keep"), "").unwrap();
        std::os::unix::fs::symlink(&outside.0, profiles.join("space-90")).unwrap();
        for name in ["default", "space-091", "space-x"] {
            fs::create_dir(profiles.join(name)).unwrap();
        }
        fs::write(profiles.join("space-92"), "").unwrap();

        let loaded = BrowserState::load_from(&root.0).unwrap();
        loaded.remove_orphaned_profiles().unwrap();
        let left = fs::read_dir(&profiles)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<BTreeSet<_>>();
        let personal_dir = format!("space-{personal}");
        let expected = [
            "default",
            &personal_dir,
            "space-091",
            "space-90",
            "space-92",
            "space-x",
        ];
        assert_eq!(left, expected.map(str::to_owned).into());
        assert!(outside.0.join("keep").exists());
        assert!(loaded.cache_dir(personal).is_dir());
    }

    #[test]
    fn profiles_stay_unless_the_session_is_the_one_saved_last() {
        let root = TestDir::new();
        let state = BrowserState::load_from(&root.0).unwrap();
        let orphan = root.0.join(PROFILES_DIR).join("space-50");
        fs::create_dir_all(&orphan).unwrap();
        // No state.json: the defaults.
        state.remove_orphaned_profiles().unwrap();
        assert!(orphan.is_dir());
        // A damaged state.json: the backup, which may predate space 50.
        state.save().unwrap();
        fs::copy(root.0.join(STATE_FILE), root.0.join(BACKUP_FILE)).unwrap();
        fs::write(root.0.join(STATE_FILE), "{ partial").unwrap();
        let recovered = BrowserState::load_from(&root.0).unwrap();
        recovered.remove_orphaned_profiles().unwrap();
        assert!(orphan.is_dir());
        // Another instance (or a crashed one) holds the profiles.
        recovered.save().unwrap();
        let lock = root.0.join(PROFILES_DIR).join(CHROMIUM_LOCK);
        std::os::unix::fs::symlink("host-123", &lock).unwrap();
        let loaded = BrowserState::load_from(&root.0).unwrap();
        loaded.remove_orphaned_profiles().unwrap();
        assert!(orphan.is_dir());
        fs::remove_file(&lock).unwrap();
        loaded.remove_orphaned_profiles().unwrap();
        assert!(!orphan.exists());
    }

    #[test]
    fn the_window_frame_is_saved_and_optional() {
        let root = TestDir::new();
        let mut state = BrowserState::load_from(&root.0).unwrap();
        assert_eq!(state.window, None);
        let window = SavedWindow {
            x: 40.,
            y: 60.5,
            width: 1400.,
            height: 900.,
            display: Some("37D8832A-2D66-02CA-B9F7-8F30A301B230".into()),
            fullscreen: true,
        };
        state.window = Some(window.clone());
        state.save().unwrap();
        assert_eq!(
            BrowserState::load_from(&root.0).unwrap().window,
            Some(window)
        );
        let json = serde_json::json!({
            "spaces": [{ "id": 1, "name": "x", "tabs": [] }],
            "selected_space_id": 1,
            "window": { "x": 1, "y": 2, "width": 3, "height": 4 }
        });
        fs::write(root.0.join(STATE_FILE), json.to_string()).unwrap();
        let loaded = BrowserState::load_from(&root.0).unwrap().window.unwrap();
        assert_eq!((loaded.display, loaded.fullscreen), (None, false));
    }

    #[test]
    fn selecting_within_a_space_keeps_the_active_space() {
        let mut state = BrowserState::default();
        let (personal, work) = (state.spaces[0].id, state.spaces[1].id);
        let opener = state.new_tab(personal, "a.com").unwrap();
        let blank = state.new_tab(personal, "b.com").unwrap();
        state.select_space(work);
        assert!(state.select_tab_in_space(personal, opener));
        assert_eq!(state.selected_space_id, work);
        assert_eq!(selected(&state, personal), Some(opener));
        assert!(!state.select_tab_in_space(work, blank));
        assert!(!state.select_tab_in_space(12345, blank));
        assert!(state.select_tab(personal, blank));
        assert_eq!(state.selected_space_id, personal);
    }

    #[test]
    fn tabs_move_to_their_section_of_another_space_under_a_new_id() {
        let root = TestDir::new();
        let mut state = BrowserState::load_from(&root.0).unwrap();
        let (personal, work) = (state.spaces[0].id, state.spaces[1].id);
        let pinned = state.new_tab(personal, "a.com").unwrap();
        state.set_pin(pinned, Pin::Pinned);
        let older = state.new_tab(personal, "b.com").unwrap();
        let newer = state.new_tab(personal, "c.com").unwrap();
        state.update_tab(newer, "https://c.com/", "C");

        // Into an empty space: it is selected there; here the selection
        // moves on as on closing. The active space stays.
        let moved = state.move_tab_to_space(newer, work).unwrap();
        assert!(moved > newer);
        assert_eq!(sidebar(&state, personal), [pinned, older]);
        assert_eq!(selected(&state, personal), Some(older));
        assert_eq!(sidebar(&state, work), [moved]);
        assert_eq!(selected(&state, work), Some(moved));
        assert_eq!(state.selected_space_id, personal);
        let tab = state.tab(moved).unwrap();
        assert_eq!(
            (tab.url.as_str(), tab.title.as_str()),
            ("https://c.com/", "C")
        );
        assert_eq!(state.tab(newer), None);

        // Today tabs go to the top of Today, pinned tabs after the pinned
        // ones; the other space keeps its selection.
        let moved_today = state.move_tab_to_space(older, work).unwrap();
        let moved_pinned = state.move_tab_to_space(pinned, work).unwrap();
        assert_eq!(sidebar(&state, work), [moved_pinned, moved_today, moved]);
        assert_eq!(state.tab(moved_pinned).unwrap().pin, Pin::Pinned);
        assert_eq!(selected(&state, work), Some(moved));
        // The last tab leaves an empty space.
        assert!(state.active_space().tabs.is_empty());
        assert_eq!(state.active_tab(), None);

        // Within the same space, unknown tabs and spaces: refused.
        assert_eq!(state.move_tab_to_space(moved, work), None);
        assert_eq!(state.move_tab_to_space(12345, personal), None);
        assert_eq!(state.move_tab_to_space(moved, 12345), None);
        assert_eq!(sidebar(&state, work), [moved_pinned, moved_today, moved]);
        state.save().unwrap();
        let loaded = BrowserState::load_from(&root.0).unwrap();
        assert_eq!(loaded.spaces, state.spaces);
    }

    fn closed_tab(id: TabId) -> Tab {
        Tab {
            id,
            url: format!("https://{id}.test/"),
            title: id.to_string(),
            ..Tab::default()
        }
    }

    #[test]
    fn closed_tabs_are_capped_and_taken_newest_first() {
        let mut state = BrowserState::default();
        let space = state.selected_space_id;
        assert_eq!(state.take_closed(None), None);
        for id in 100..100 + REOPEN_LIMIT as u64 + 2 {
            state.remember_closed(space, closed_tab(id));
        }
        let ids = |state: &BrowserState| -> Vec<TabId> {
            state
                .closed_tabs()
                .iter()
                .map(|closed| closed.tab.id)
                .collect()
        };
        assert_eq!(ids(&state).len(), REOPEN_LIMIT);
        assert_eq!(ids(&state)[0], 102);
        assert_eq!(state.take_closed(None).unwrap().tab.id, 121);
        assert_eq!(state.take_closed(Some(0)).unwrap().tab.id, 102);
        assert_eq!(state.take_closed(Some(REOPEN_LIMIT)), None);
        state.forget_closed(110);
        assert!(!ids(&state).contains(&110));
        assert_eq!(ids(&state).len(), REOPEN_LIMIT - 3);
    }

    #[test]
    fn closed_tabs_are_saved_and_go_with_their_space() {
        let root = TestDir::new();
        let mut state = BrowserState::load_from(&root.0).unwrap();
        let (personal, work) = (state.spaces[0].id, state.spaces[1].id);
        state.remember_closed(personal, closed_tab(100));
        state.remember_closed(work, closed_tab(101));
        state.save().unwrap();
        let mut loaded = BrowserState::load_from(&root.0).unwrap();
        assert_eq!(loaded.closed_tabs(), state.closed_tabs());
        loaded.remove_space(work);
        assert_eq!(loaded.closed_tabs().len(), 1);
        assert_eq!(loaded.closed_tabs()[0].space_id, personal);

        // Files from before have none; entries of unknown spaces go.
        let json = serde_json::json!({
            "spaces": [{ "id": 1, "name": "x", "tabs": [] }],
            "selected_space_id": 1,
        });
        fs::write(root.0.join(STATE_FILE), json.to_string()).unwrap();
        assert!(BrowserState::load_from(&root.0).unwrap().closed.is_empty());
        let closed: Vec<serde_json::Value> = (0..25)
            .map(|index| {
                serde_json::json!({
                    "space_id": if index == 24 { 9 } else { 1 },
                    "tab": { "id": 100 + index, "url": "https://a.test/", "title": "A" }
                })
            })
            .collect();
        let json = serde_json::json!({
            "spaces": [{ "id": 1, "name": "x", "tabs": [] }],
            "selected_space_id": 1,
            "closed": closed,
        });
        fs::write(root.0.join(STATE_FILE), json.to_string()).unwrap();
        let loaded = BrowserState::load_from(&root.0).unwrap();
        assert_eq!(loaded.closed.len(), REOPEN_LIMIT);
        assert!(loaded.closed.iter().all(|closed| closed.space_id == 1));
        assert_eq!(loaded.closed.last().unwrap().tab.id, 123);
    }
}
