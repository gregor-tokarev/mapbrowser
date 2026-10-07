//! Favicons keyed by host. A small disk cache lets unloaded tabs show their
//! icon without loading the page; the engine downloads icons, the shell only
//! stores them. Disk access never happens on the main thread.

use gpui::{BackgroundExecutor, Image, ImageFormat, Task, http_client::Url};
use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    fmt::Write as _,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
};

/// Engine icons are 32 pt at 2x; anything larger is not a favicon.
const MAX_BYTES: usize = 64 * 1024;
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const MAX_FILE_NAME: usize = 200;

pub type LoadedIcons = Vec<(String, Arc<Image>)>;

pub struct FaviconStore {
    dir: PathBuf,
    icons: HashMap<String, Arc<Image>>,
    /// Hosts already looked up on disk, found or not, so each is read once.
    looked_up: HashSet<String>,
    writer: Option<mpsc::Sender<(PathBuf, Vec<u8>)>>,
}

impl FaviconStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("favicons"),
            icons: HashMap::new(),
            looked_up: HashSet::new(),
            writer: None,
        }
    }

    /// `host` as returned by `host_of`.
    pub fn get(&self, host: &str) -> Option<Arc<Image>> {
        self.icons.get(host).cloned()
    }

    /// Reads cached icons of hosts not seen yet on the background executor.
    /// Pass the result to `insert_loaded`.
    pub fn load_for(
        &mut self,
        hosts: impl IntoIterator<Item = String>,
        executor: &BackgroundExecutor,
    ) -> Task<LoadedIcons> {
        let hosts = hosts
            .into_iter()
            .filter(|host| !self.icons.contains_key(host) && self.looked_up.insert(host.clone()))
            .collect::<Vec<_>>();
        if hosts.is_empty() {
            return Task::ready(Vec::new());
        }
        let dir = self.dir.clone();
        executor.spawn(async move {
            hosts
                .into_iter()
                .filter_map(|host| {
                    let png = read_icon(&dir, &host)?;
                    Some((host, Arc::new(Image::from_bytes(ImageFormat::Png, png))))
                })
                .collect()
        })
    }

    /// Returns whether any icon was added. Icons stored while the read was
    /// running are newer and win.
    pub fn insert_loaded(&mut self, loaded: LoadedIcons) -> bool {
        let mut added = false;
        for (host, image) in loaded {
            if let Entry::Vacant(entry) = self.icons.entry(host) {
                entry.insert(image);
                added = true;
            }
        }
        added
    }

    /// Keeps an icon downloaded by the engine and caches it on disk in the
    /// background. Returns whether the host's icon changed.
    pub fn store(&mut self, host: &str, png: Vec<u8>) -> bool {
        if !is_png(&png) || self.icons.get(host).is_some_and(|icon| icon.bytes == png) {
            return false;
        }
        self.looked_up.insert(host.to_owned());
        if let Some(name) = file_name(host) {
            self.write_in_background(self.dir.join(name), png.clone());
        }
        self.icons.insert(
            host.to_owned(),
            Arc::new(Image::from_bytes(ImageFormat::Png, png)),
        );
        true
    }

    fn write_in_background(&mut self, path: PathBuf, png: Vec<u8>) {
        let writer = self.writer.get_or_insert_with(spawn_writer);
        // A failed send means the thread couldn't start; the cache is optional.
        let _ = writer.send((path, png));
    }
}

/// One thread writes icons in order, so a host's newest icon always lands last.
/// It exits once the store is dropped and the queue is drained.
fn spawn_writer() -> mpsc::Sender<(PathBuf, Vec<u8>)> {
    let (sender, receiver) = mpsc::channel::<(PathBuf, Vec<u8>)>();
    let spawned = thread::Builder::new()
        .name("favicon-cache".into())
        .spawn(move || {
            for (path, png) in receiver {
                if let Err(error) = write_icon(&path, &png) {
                    eprintln!("Couldn't cache favicon {}: {error}", path.display());
                }
            }
        });
    if let Err(error) = spawned {
        eprintln!("Couldn't start the favicon cache: {error}");
    }
    sender
}

/// The host a site is known by, for favicons and per-site settings.
/// `None` for URLs without a network host, such as about:, data: or file:.
pub fn host_of(url: &str) -> Option<String> {
    normalize_host(Url::parse(url).ok()?.host_str()?)
}

/// Lowercase, without a trailing dot or a leading "www.".
pub fn normalize_host(host: &str) -> Option<String> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    (!host.is_empty()).then(|| host.to_owned())
}

/// `host` and every domain above it, most specific first: a.example.com,
/// example.com, com. A per-site setting for a domain covers its subdomains.
pub fn host_and_parents(host: &str) -> impl Iterator<Item = &str> {
    std::iter::successors(Some(host), |host| {
        host.split_once('.').map(|(_, parent)| parent)
    })
}

fn is_png(bytes: &[u8]) -> bool {
    bytes.len() <= MAX_BYTES && bytes.starts_with(PNG_SIGNATURE)
}

/// Lowercase letters, digits, dots and hyphens stay; every other byte is
/// %-escaped, so distinct hosts never share a file (even on case-insensitive
/// file systems) and no name can leave the cache directory.
fn file_name(host: &str) -> Option<String> {
    if host.is_empty() || host.starts_with('.') {
        return None;
    }
    let mut name = String::with_capacity(host.len() + 4);
    for byte in host.bytes() {
        match byte {
            b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' => name.push(char::from(byte)),
            _ => {
                let _ = write!(name, "%{byte:02x}");
            }
        }
    }
    (name.len() <= MAX_FILE_NAME).then(|| name + ".png")
}

fn read_icon(dir: &Path, host: &str) -> Option<Vec<u8>> {
    let path = dir.join(file_name(host)?);
    if fs::metadata(&path).ok()?.len() > MAX_BYTES as u64 {
        return None;
    }
    let png = fs::read(path).ok()?;
    is_png(&png).then_some(png)
}

/// The rename keeps readers from seeing partial files. No fsync: losing an
/// icon to a power cut only means downloading it again.
fn write_icon(path: &Path, png: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let temporary = path.with_extension("png.tmp");
    fs::write(&temporary, png)?;
    fs::rename(&temporary, path).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("mapbrowser-favicons-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn png(marker: u8) -> Vec<u8> {
        [PNG_SIGNATURE, &[marker; 16]].concat()
    }

    #[test]
    fn hosts_are_lowercase_without_www() {
        for (url, expected) in [
            ("https://WWW.Example.COM/path?q=1", Some("example.com")),
            ("https://www.example.com./", Some("example.com")),
            (
                "http://user:secret@news.ycombinator.com:8080/",
                Some("news.ycombinator.com"),
            ),
            ("https://wwwx.example.com", Some("wwwx.example.com")),
            ("https://www2.example.com", Some("www2.example.com")),
            ("http://localhost:3000", Some("localhost")),
            ("http://[::1]:8080/", Some("[::1]")),
            ("https://bücher.example/", Some("xn--bcher-kva.example")),
            ("https://www./", Some("www")),
            ("about:blank", None),
            ("data:text/html,hi", None),
            ("file:///tmp/page.html", None),
            ("example.com", None),
            ("", None),
        ] {
            assert_eq!(host_of(url).as_deref(), expected, "url: {url}");
        }
        assert_eq!(
            normalize_host(" WWW.Example.com ").as_deref(),
            Some("example.com")
        );
        assert_eq!(normalize_host("www."), Some("www".into()));
        assert_eq!(normalize_host(" . "), None);
    }

    #[test]
    fn hosts_list_their_parent_domains() {
        assert_eq!(
            host_and_parents("cooking.nytimes.com").collect::<Vec<_>>(),
            ["cooking.nytimes.com", "nytimes.com", "com"]
        );
        assert_eq!(
            host_and_parents("localhost").collect::<Vec<_>>(),
            ["localhost"]
        );
    }

    #[test]
    fn file_names_are_escaped_and_never_leave_the_directory() {
        for (host, expected) in [
            ("example.com", Some("example.com.png")),
            ("xn--bcher-kva.example", Some("xn--bcher-kva.example.png")),
            ("[::1]", Some("%5b%3a%3a1%5d.png")),
            ("a/b", Some("a%2fb.png")),
            ("a_b", Some("a%5fb.png")),
            ("a%5fb", Some("a%255fb.png")),
            ("Example.com", Some("%45xample.com.png")),
            ("..", None),
            (".hidden", None),
            ("", None),
        ] {
            assert_eq!(file_name(host).as_deref(), expected, "host: {host}");
        }
        assert!(file_name(&"a".repeat(MAX_FILE_NAME)).is_some());
        assert!(file_name(&"a".repeat(MAX_FILE_NAME + 1)).is_none());
    }

    #[test]
    fn icons_round_trip_through_the_disk_cache() {
        let dir = TestDir::new("round-trip");
        assert_eq!(read_icon(&dir.0, "example.com"), None);
        write_icon(&dir.0.join(file_name("example.com").unwrap()), &png(1)).unwrap();
        assert_eq!(read_icon(&dir.0, "example.com"), Some(png(1)));
        write_icon(&dir.0.join(file_name("example.com").unwrap()), &png(2)).unwrap();
        assert_eq!(read_icon(&dir.0, "example.com"), Some(png(2)));
        // Damaged or oversized files are ignored rather than shown.
        fs::write(dir.0.join("broken.com.png"), b"not a png").unwrap();
        assert_eq!(read_icon(&dir.0, "broken.com"), None);
        let huge = [PNG_SIGNATURE, &vec![0; MAX_BYTES]].concat();
        fs::write(dir.0.join("huge.com.png"), huge).unwrap();
        assert_eq!(read_icon(&dir.0, "huge.com"), None);
        assert_eq!(
            fs::read_dir(&dir.0).unwrap().count(),
            3,
            "no temporary files are left behind"
        );
    }

    #[test]
    fn stored_icons_are_kept_in_memory_and_written_in_the_background() {
        let dir = TestDir::new("store");
        let mut store = FaviconStore::new(&dir.0);
        assert!(!store.store("example.com", b"not a png".to_vec()));
        assert!(!store.store("example.com", [PNG_SIGNATURE, &[0; MAX_BYTES]].concat()));
        assert!(store.get("example.com").is_none());
        assert!(store.store("example.com", png(1)));
        assert!(!store.store("example.com", png(1)));
        assert!(store.store("example.com", png(2)));
        assert_eq!(store.get("example.com").unwrap().bytes, png(2));
        // An older disk read never replaces an icon stored meanwhile.
        let stale = Arc::new(Image::from_bytes(ImageFormat::Png, png(1)));
        let other = Arc::new(Image::from_bytes(ImageFormat::Png, png(3)));
        assert!(store.insert_loaded(vec![
            ("example.com".into(), stale),
            ("other.com".into(), other)
        ]));
        assert_eq!(store.get("example.com").unwrap().bytes, png(2));
        assert_eq!(store.get("other.com").unwrap().bytes, png(3));

        let deadline = Instant::now() + Duration::from_secs(5);
        let cached = dir.0.join("favicons");
        while read_icon(&cached, "example.com") != Some(png(2)) {
            assert!(Instant::now() < deadline, "favicon was not written");
            thread::sleep(Duration::from_millis(10));
        }
    }
}
