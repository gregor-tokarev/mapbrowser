//! Persistent spaces and tabs. Each space owns a distinct Chromium profile.

use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub type SpaceId = u64;
pub type TabId = u64;

const STATE_FILE: &str = "state.json";
const BACKUP_FILE: &str = "state.backup.json";
const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tab {
    pub id: TabId,
    pub url: String,
    pub title: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Space {
    pub id: SpaceId,
    pub name: String,
    pub tabs: Vec<Tab>,
    pub selected_tab_id: TabId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrowserState {
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    pub spaces: Vec<Space>,
    pub selected_space_id: SpaceId,
    #[serde(default)]
    next_id: u64,
    #[serde(skip, default = "data_dir")]
    storage_dir: PathBuf,
}

fn schema_version() -> u32 {
    SCHEMA_VERSION
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
                Space {
                    id: 1,
                    name: "Personal".into(),
                    tabs: vec![blank_tab(2)],
                    selected_tab_id: 2,
                },
                Space {
                    id: 3,
                    name: "Work".into(),
                    tabs: vec![blank_tab(4)],
                    selected_tab_id: 4,
                },
            ],
            selected_space_id: 1,
            next_id: 5,
            storage_dir: data_dir(),
        }
    }
}

fn blank_tab(id: TabId) -> Tab {
    Tab {
        id,
        url: "about:blank".into(),
        title: "New tab".into(),
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

    pub fn save(&self) -> io::Result<()> {
        self.save_to(&self.storage_dir)
    }

    /// Atomically replace state on the same filesystem and retain the last valid save.
    pub fn save_to(&self, root: impl AsRef<Path>) -> io::Result<()> {
        let root = root.as_ref();
        fs::create_dir_all(root)?;
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        let state_path = root.join(STATE_FILE);
        if state_path.exists() {
            match read_state(&state_path, root) {
                Ok(previous) => atomic_write(
                    &root.join(BACKUP_FILE),
                    &serde_json::to_vec_pretty(&previous).map_err(io::Error::other)?,
                )?,
                Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                    quarantine(&state_path)?;
                }
                Err(error) => return Err(error),
            }
        }
        atomic_write(&state_path, &bytes)
    }

    pub fn active_space(&self) -> &Space {
        self.spaces
            .iter()
            .find(|space| space.id == self.selected_space_id)
            .unwrap_or(&self.spaces[0])
    }

    pub fn active_tab(&self) -> &Tab {
        let space = self.active_space();
        space
            .tabs
            .iter()
            .find(|tab| tab.id == space.selected_tab_id)
            .unwrap_or(&space.tabs[0])
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

    fn allocate_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("browser ID space exhausted");
        id
    }

    pub fn create_space(&mut self, name: impl Into<String>) -> SpaceId {
        let id = self.allocate_id();
        let tab_id = self.allocate_id();
        let name = name.into();
        let name = name.trim();
        self.spaces.push(Space {
            id,
            name: if name.is_empty() {
                format!("Space {}", self.spaces.len() + 1)
            } else {
                name.to_owned()
            },
            tabs: vec![blank_tab(tab_id)],
            selected_tab_id: tab_id,
        });
        self.selected_space_id = id;
        id
    }

    pub fn rename_space(&mut self, id: SpaceId, name: impl AsRef<str>) -> bool {
        let name = name.as_ref().trim();
        if name.is_empty() {
            return false;
        }
        let Some(space) = self.spaces.iter_mut().find(|space| space.id == id) else {
            return false;
        };
        space.name = name.to_owned();
        true
    }

    pub fn select_space(&mut self, id: SpaceId) -> bool {
        if self.space(id).is_none() {
            return false;
        }
        self.selected_space_id = id;
        true
    }

    pub fn new_tab(&mut self, space_id: SpaceId, url: impl AsRef<str>) -> Option<TabId> {
        let index = self.spaces.iter().position(|space| space.id == space_id)?;
        let id = self.allocate_id();
        let url = display_url(&normalize_address(url.as_ref()));
        let title = if url == "about:blank" {
            "New tab".into()
        } else {
            url.clone()
        };
        self.spaces[index].tabs.push(Tab { id, url, title });
        self.spaces[index].selected_tab_id = id;
        Some(id)
    }

    pub fn select_tab(&mut self, space_id: SpaceId, tab_id: TabId) -> bool {
        let Some(space) = self.spaces.iter_mut().find(|space| space.id == space_id) else {
            return false;
        };
        if !space.tabs.iter().any(|tab| tab.id == tab_id) {
            return false;
        }
        space.selected_tab_id = tab_id;
        self.selected_space_id = space_id;
        true
    }

    pub fn close_tab(&mut self, space_id: SpaceId, tab_id: TabId) -> bool {
        let Some(space_index) = self.spaces.iter().position(|space| space.id == space_id) else {
            return false;
        };
        let Some(tab_index) = self.spaces[space_index]
            .tabs
            .iter()
            .position(|tab| tab.id == tab_id)
        else {
            return false;
        };
        self.spaces[space_index].tabs.remove(tab_index);
        if self.spaces[space_index].tabs.is_empty() {
            let id = self.allocate_id();
            self.spaces[space_index].tabs.push(blank_tab(id));
            self.spaces[space_index].selected_tab_id = id;
        } else if self.spaces[space_index].selected_tab_id == tab_id {
            let index = tab_index.saturating_sub(1);
            self.spaces[space_index].selected_tab_id = self.spaces[space_index].tabs[index].id;
        }
        true
    }

    /// Browser callbacks can update hidden tabs without selecting their space.
    pub fn update_tab(&mut self, id: TabId, url: impl AsRef<str>, title: impl AsRef<str>) -> bool {
        let Some(tab) = self
            .spaces
            .iter_mut()
            .flat_map(|space| &mut space.tabs)
            .find(|tab| tab.id == id)
        else {
            return false;
        };
        tab.url = display_url(url.as_ref());
        tab.title = if title.as_ref().trim().is_empty() || title.as_ref() == url.as_ref() {
            if tab.url == "about:blank" {
                "New tab".into()
            } else {
                tab.url.clone()
            }
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
        if self.spaces.is_empty() {
            self.create_space("Personal");
        }
        for index in 0..self.spaces.len() {
            if self.spaces[index].tabs.is_empty() {
                let id = self.allocate_id();
                self.spaces[index].tabs.push(blank_tab(id));
            }
            let space = &mut self.spaces[index];
            for tab in &mut space.tabs {
                let sanitized = display_url(&tab.url);
                if tab.title == tab.url {
                    tab.title = sanitized.clone();
                }
                tab.url = sanitized;
            }
            if !space.tabs.iter().any(|tab| tab.id == space.selected_tab_id) {
                space.selected_tab_id = space.tabs[0].id;
            }
            if space.name.trim().is_empty() {
                space.name = format!("Space {}", index + 1);
            }
        }
        if self.space(self.selected_space_id).is_none() {
            self.selected_space_id = self.spaces[0].id;
        }
        Ok(())
    }
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

    #[test]
    fn spaces_preserve_independent_tab_selection_and_profile_paths() {
        let root = TestDir::new();
        let mut state = BrowserState::load_from(&root.0).unwrap();
        let personal = state.spaces[0].id;
        let work = state.spaces[1].id;
        let personal_tab = state.new_tab(personal, "example.com").unwrap();
        let work_tab = state.new_tab(work, "localhost:8080").unwrap();
        state.select_space(work);
        assert_eq!(state.active_tab().id, work_tab);
        state.select_space(personal);
        assert_eq!(state.active_tab().id, personal_tab);
        assert_ne!(state.profile_dir(personal), state.profile_dir(work));
        assert_ne!(state.cache_dir(personal), state.cache_dir(work));
        assert!(state.cache_dir(personal).is_dir());
        let original_path = state.profile_dir(personal);
        state.rename_space(personal, "Renamed");
        assert_eq!(state.profile_dir(personal), original_path);
    }

    #[test]
    fn closing_tabs_preserves_selection_and_replaces_the_final_tab() {
        let mut state = BrowserState::default();
        let space_id = state.selected_space_id;
        let initial = state.active_tab().id;
        let second = state.new_tab(space_id, "example.com").unwrap();
        let third = state.new_tab(space_id, "example.org").unwrap();
        state.close_tab(space_id, second);
        assert_eq!(state.active_tab().id, third);
        state.close_tab(space_id, third);
        assert_eq!(state.active_tab().id, initial);
        assert!(state.close_tab(space_id, initial));
        assert_ne!(state.active_tab().id, initial);
        assert_eq!(state.active_tab().url, "about:blank");
        assert_eq!(state.active_space().tabs.len(), 1);
        assert!(!state.close_tab(space_id, initial));
    }

    #[test]
    fn invalid_selection_does_not_change_current_space_or_tab() {
        let mut state = BrowserState::default();
        let active_space = state.selected_space_id;
        let active_tab = state.active_tab().id;
        let other_space = state.spaces[1].id;
        assert!(!state.select_space(12345));
        assert!(!state.select_tab(other_space, active_tab));
        assert!(!state.rename_space(active_space, "  "));
        assert_eq!(state.selected_space_id, active_space);
        assert_eq!(state.active_tab().id, active_tab);
    }

    #[test]
    fn hidden_tab_updates_do_not_switch_spaces() {
        let mut state = BrowserState::default();
        let selected_space = state.selected_space_id;
        let hidden_tab = state.spaces[1].selected_tab_id;
        state.update_tab(hidden_tab, "https://example.com/chat", "Chat (1)");
        assert_eq!(state.selected_space_id, selected_space);
        assert_eq!(state.tab(hidden_tab).unwrap().title, "Chat (1)");
    }

    #[test]
    fn save_and_reload_preserves_spaces_tabs_and_id_allocation() {
        let root = TestDir::new();
        let mut state = BrowserState::load_from(&root.0).unwrap();
        let space = state.create_space("Research");
        let tab = state.new_tab(space, "rust browser").unwrap();
        state.update_tab(tab, "https://example.com", "Example");
        state.save().unwrap();
        let mut loaded = BrowserState::load_from(&root.0).unwrap();
        assert_eq!(loaded.spaces, state.spaces);
        assert_eq!(loaded.selected_space_id, space);
        assert_eq!(loaded.root_dir(), root.0);
        assert!(loaded.new_tab(space, "about:blank").unwrap() > tab);
        assert!(
            !fs::read_to_string(root.0.join(STATE_FILE))
                .unwrap()
                .contains("storage_dir")
        );
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
    fn load_repairs_stale_selected_ids_and_next_id() {
        let root = TestDir::new();
        let mut state = BrowserState::default();
        state.selected_space_id = 9999;
        state.spaces[0].selected_tab_id = 9999;
        state.spaces[1].tabs.clear();
        state.next_id = 0;
        fs::write(root.0.join(STATE_FILE), serde_json::to_vec(&state).unwrap()).unwrap();
        let mut loaded = BrowserState::load_from(&root.0).unwrap();
        assert_eq!(loaded.selected_space_id, loaded.spaces[0].id);
        assert_eq!(
            loaded.active_space().selected_tab_id,
            loaded.active_tab().id
        );
        assert_eq!(loaded.spaces[1].tabs.len(), 1);
        let ids = loaded
            .spaces
            .iter()
            .flat_map(|space| std::iter::once(space.id).chain(space.tabs.iter().map(|tab| tab.id)))
            .collect::<HashSet<_>>();
        let new_space = loaded.create_space("New");
        assert!(!ids.contains(&new_space));
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
        assert_eq!(state.active_tab().url, "https://example.com/path");
        assert_eq!(state.active_tab().title, "https://example.com/path");
        state.update_tab(id, url, url);
        state.save().unwrap();
        state.update_tab(id, url, "Example site");
        state.save().unwrap();
        assert_eq!(state.active_tab().title, "Example site");
        for file in [STATE_FILE, BACKUP_FILE] {
            let json = fs::read_to_string(root.0.join(file)).unwrap();
            assert!(!json.contains("fake-user"));
            assert!(!json.contains("fake-password"));
        }
    }
}
