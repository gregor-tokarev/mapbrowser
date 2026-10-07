//! Persistent spaces and tabs. Each space owns a distinct Chromium profile.

use crate::favicons::normalize_host;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        LazyLock, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use unicode_segmentation::UnicodeSegmentation;

pub type SpaceId = u64;
pub type TabId = u64;

const STATE_FILE: &str = "state.json";
const BACKUP_FILE: &str = "state.backup.json";
const SCHEMA_VERSION: u32 = 2;
const BLANK_URL: &str = "about:blank";
/// Space themes are indices below this; the UI owns the palettes.
pub const THEME_COUNT: u8 = 8;
pub const DEFAULT_SIDEBAR_WIDTH: f32 = 228.;
pub const MIN_SIDEBAR_WIDTH: f32 = 180.;
pub const MAX_SIDEBAR_WIDTH: f32 = 420.;

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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tab {
    pub id: TabId,
    pub url: String,
    pub title: String,
    #[serde(default)]
    pub pin: Pin,
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
    /// Hosts, normalized like `favicons::host_of`, where content blocking is off.
    #[serde(default)]
    blocking_exceptions: BTreeSet<String>,
    #[serde(skip, default = "data_dir")]
    storage_dir: PathBuf,
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
            storage_dir: data_dir(),
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

    /// Selection moves to the next tab of the same section, else the previous
    /// one there, else the nearest tab in sidebar order.
    fn remove_at(&mut self, index: usize) -> Tab {
        let tab = self.tabs.remove(index);
        if self.selected_tab_id == Some(tab.id) {
            let next = self.tabs.get(index);
            let previous = index.checked_sub(1).and_then(|index| self.tabs.get(index));
            self.selected_tab_id = next
                .filter(|next| next.pin == tab.pin)
                .or(previous.filter(|previous| previous.pin == tab.pin))
                .or(next)
                .or(previous)
                .map(|tab| tab.id);
        }
        tab
    }
}

/// Last sequence written per state directory. Holding the lock for the whole
/// write serializes writers.
static WRITTEN: LazyLock<Mutex<HashMap<PathBuf, u64>>> = LazyLock::new(Default::default);
static NEXT_SNAPSHOT: AtomicU64 = AtomicU64::new(1);

/// Serialized state, ordered by when it was taken. Serializing needs the
/// state (main thread); writing it, which waits on fsync, can happen anywhere.
pub struct StateSnapshot {
    root: PathBuf,
    bytes: Vec<u8>,
    seq: u64,
}

impl StateSnapshot {
    /// Atomically replace the saved state, keeping the previous valid save as
    /// a backup. Returns `Ok(false)` without writing if a newer snapshot of
    /// the same directory has already been written.
    pub fn write(&self) -> io::Result<bool> {
        let mut written = WRITTEN.lock().unwrap_or_else(PoisonError::into_inner);
        if written.get(&self.root).is_some_and(|&seq| seq >= self.seq) {
            return Ok(false);
        }
        fs::create_dir_all(&self.root)?;
        let state_path = self.root.join(STATE_FILE);
        match read_state(&state_path, &self.root) {
            Ok(previous) => atomic_write(
                &self.root.join(BACKUP_FILE),
                &serde_json::to_vec_pretty(&previous).map_err(io::Error::other)?,
            )?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) if error.kind() == io::ErrorKind::InvalidData => quarantine(&state_path)?,
            Err(error) => return Err(error),
        }
        atomic_write(&state_path, &self.bytes)?;
        written.insert(self.root.clone(), self.seq);
        Ok(true)
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
        let state_path = root.join(STATE_FILE);
        let backup_path = root.join(BACKUP_FILE);
        let state = match read_state(&state_path, root) {
            Ok(state) => state,
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                quarantine(&state_path)?;
                match read_state(&backup_path, root) {
                    Ok(state) => state,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Self::at_root(root),
                    Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                        quarantine(&backup_path)?;
                        Self::at_root(root)
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match read_state(&backup_path, root) {
                    Ok(state) => state,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Self::at_root(root),
                    Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                        quarantine(&backup_path)?;
                        Self::at_root(root)
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        };
        state.ensure_profile_dirs()?;
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
            .join("profiles")
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

    pub fn snapshot(&self) -> io::Result<StateSnapshot> {
        Ok(StateSnapshot {
            root: self.storage_dir.clone(),
            bytes: serde_json::to_vec_pretty(self).map_err(io::Error::other)?,
            seq: NEXT_SNAPSHOT.fetch_add(1, Ordering::Relaxed),
        })
    }

    /// Write on the calling thread, e.g. while quitting. Ordered after every
    /// earlier snapshot, so a pending background write can't replace it.
    pub fn save(&self) -> io::Result<()> {
        self.snapshot()?.write().map(drop)
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

    pub fn is_blocking_disabled(&self, host: &str) -> bool {
        normalize_host(host).is_some_and(|host| self.blocking_exceptions.contains(&host))
    }

    /// Returns whether the setting changed.
    pub fn set_blocking_disabled(&mut self, host: &str, disabled: bool) -> bool {
        let Some(host) = normalize_host(host) else {
            return false;
        };
        if disabled {
            self.blocking_exceptions.insert(host)
        } else {
            self.blocking_exceptions.remove(&host)
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
        let name = name.into();
        let name = name.trim();
        let name = if name.is_empty() {
            format!("Space {}", self.spaces.len() + 1)
        } else {
            name.to_owned()
        };
        let theme = self.least_used_theme();
        self.spaces.push(Space::new(id, name, theme));
        self.selected_space_id = id;
        id
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

    pub fn rename_space(&mut self, id: SpaceId, name: impl AsRef<str>) -> bool {
        let name = name.as_ref().trim();
        if name.is_empty() {
            return false;
        }
        let Some(space) = self.space_mut(id) else {
            return false;
        };
        space.name = name.to_owned();
        true
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

    /// Opens a tab at the top of Today, like Arc, and selects it.
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
        let url = display_url(&normalize_address(url));
        let title = fallback_title(&url);
        let space = &mut self.spaces[index];
        let at = space.section_start(Pin::Today);
        space.tabs.insert(
            at,
            Tab {
                id,
                url,
                title,
                pin: Pin::Today,
            },
        );
        if select || space.selected_tab_id.is_none() {
            space.selected_tab_id = Some(id);
        }
        Some(id)
    }

    pub fn select_tab(&mut self, space_id: SpaceId, tab_id: TabId) -> bool {
        let Some(space) = self.space_mut(space_id) else {
            return false;
        };
        if space.tab_index(tab_id).is_none() {
            return false;
        }
        space.selected_tab_id = Some(tab_id);
        self.selected_space_id = space_id;
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
        tab.pin = pin;
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
        tab.pin = pin;
        let at = before
            .and_then(|before| space.tab_index(before))
            .unwrap_or_else(|| space.section_end(pin));
        space.tabs.insert(at, tab);
        true
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

    /// Browser callbacks can update hidden tabs without selecting their space.
    pub fn update_tab(&mut self, id: TabId, url: impl AsRef<str>, title: impl AsRef<str>) -> bool {
        let Some((space, index)) = self.locate_tab_mut(id) else {
            return false;
        };
        let tab = &mut space.tabs[index];
        tab.url = display_url(url.as_ref());
        tab.title = if title.as_ref().trim().is_empty() || title.as_ref() == url.as_ref() {
            fallback_title(&tab.url)
        } else {
            title.as_ref().to_owned()
        };
        true
    }

    fn validate_and_repair(&mut self) -> io::Result<()> {
        if self.schema_version > SCHEMA_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this session was created by a newer version of MapBrowser",
            ));
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
            // Arc has no blank tabs; drop them as if they were closed.
            while let Some(blank) = space.tabs.iter().position(|tab| tab.url == BLANK_URL) {
                space.remove_at(blank);
            }
            for tab in &mut space.tabs {
                let sanitized = display_url(&tab.url);
                if tab.title == tab.url {
                    tab.title = sanitized.clone();
                }
                tab.url = sanitized;
            }
            let position_theme = (index % usize::from(THEME_COUNT)) as u8;
            space.theme = Some(space.theme.unwrap_or(position_theme) % THEME_COUNT);
            space.icon = space.icon.as_deref().and_then(sanitize_icon);
            if space.name.trim().is_empty() {
                space.name = format!("Space {}", index + 1);
            }
        }
        if self.space(self.selected_space_id).is_none() {
            self.selected_space_id = self.spaces[0].id;
        }
        self.sidebar_width = clamp_sidebar_width(self.sidebar_width);
        self.blocking_exceptions = std::mem::take(&mut self.blocking_exceptions)
            .iter()
            .filter_map(|host| normalize_host(host))
            .collect();
        Ok(())
    }
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
        "New tab".into()
    } else {
        url.to_owned()
    }
}

fn sanitize_icon(icon: &str) -> Option<String> {
    icon.trim()
        .graphemes(true)
        .next()
        .filter(|grapheme| !grapheme.chars().any(char::is_control))
        .map(str::to_owned)
}

fn read_state(path: &Path, root: &Path) -> io::Result<BrowserState> {
    let bytes = fs::read(path)?;
    let mut state: BrowserState = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    state.storage_dir = root.to_path_buf();
    state.validate_and_repair()?;
    Ok(state)
}

fn unique_suffix() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{timestamp}", std::process::id())
}

fn quarantine(path: &Path) -> io::Result<()> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    fs::rename(
        path,
        path.with_file_name(format!("{name}.corrupt-{}", unique_suffix())),
    )
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = path.with_extension(format!("tmp-{}", unique_suffix()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        if let Some(parent) = path.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
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
    let lower = input.to_ascii_lowercase();
    if [
        "http://",
        "https://",
        "file://",
        "about:",
        "data:",
        "chrome:",
        "devtools:",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
    {
        return input.to_owned();
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
    if !non_web_scheme && !input.chars().any(char::is_whitespace) {
        let schemeless = input.strip_prefix("//").unwrap_or(input);
        let authority = schemeless
            .split(['/', '?', '#'])
            .next()
            .unwrap_or(schemeless);
        // A colon in user:password must not make a URL into a search query.
        let server = authority.rsplit('@').next().unwrap_or(authority);
        let host = server
            .split(':')
            .next()
            .unwrap_or(server)
            .to_ascii_lowercase();
        let local = host == "localhost"
            || host.ends_with(".localhost")
            || host == "127.0.0.1"
            || host == "0.0.0.0"
            || server.starts_with("[::1]");
        if local {
            return format!("http://{schemeless}");
        }
        if host.contains('.') || server.starts_with('[') {
            return format!("https://{schemeless}");
        }
    }
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
    format!("https://www.google.com/search?q={query}")
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
        assert!(!state.is_blocking_disabled("sub.example.com"));
        assert!(!state.set_blocking_disabled("", true));
        assert!(state.set_blocking_disabled("www.example.com", false));
        assert!(!state.set_blocking_disabled("example.com", false));
        assert!(!state.is_blocking_disabled("example.com"));
    }

    #[test]
    fn newer_schema_is_preserved_and_reported_instead_of_reset() {
        let root = TestDir::new();
        let mut state = BrowserState::default();
        state.schema_version = SCHEMA_VERSION + 1;
        let bytes = serde_json::to_vec(&state).unwrap();
        fs::write(root.0.join(STATE_FILE), &bytes).unwrap();
        assert_eq!(
            BrowserState::load_from(&root.0).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(fs::read(root.0.join(STATE_FILE)).unwrap(), bytes);
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
        ] {
            assert_eq!(normalize_address(input), expected, "input: {input}");
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
}
