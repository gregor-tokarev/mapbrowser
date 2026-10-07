//! Built-in ad and tracker blocking with EasyList and EasyPrivacy, matched by
//! Brave's adblock engine. Ads and trackers are the largest avoidable CPU and
//! network cost on ad-funded sites. Only network rules apply: pages themselves
//! (main-frame navigations) are never blocked and no cosmetic CSS is injected.
//! The lists ship with the app and update with it; nothing is downloaded.
// Only the Chromium engine matches requests; Linux previews just load the lists.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use crate::favicons::{host_and_parents, normalize_host};
use adblock::{
    lists::{FilterSet, ParseOptions, RuleTypes},
    request::Request,
};
use anyhow::{Context, Result};
use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, PoisonError, RwLock},
    thread,
    time::Instant,
};

const LISTS: [&str; 2] = ["easylist.txt", "easyprivacy.txt"];
/// Part of the cache key: adblock's serialized format may change between
/// releases, and the cache holds network rules only. A test keeps the version
/// in step with Cargo.lock.
const CACHE_VERSION: &str = "adblock-0.13.3/network";

/// Chromium's resource types, reduced to what filter options distinguish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    MainFrame,
    SubFrame,
    Stylesheet,
    Script,
    Image,
    Font,
    Object,
    Media,
    Xhr,
    Ping,
    CspReport,
    Other,
}

impl ResourceKind {
    fn filter_type(self) -> &'static str {
        match self {
            Self::MainFrame => "main_frame",
            Self::SubFrame => "sub_frame",
            Self::Stylesheet => "stylesheet",
            Self::Script => "script",
            Self::Image => "image",
            Self::Font => "font",
            Self::Object => "object",
            Self::Media => "media",
            Self::Xhr => "xhr",
            Self::Ping => "ping",
            Self::CspReport => "csp_report",
            Self::Other => "other",
        }
    }
}

/// Compiled network rules, immutable once loaded. adblock's one internal lock
/// (its lazy regex cache) stays uncontended: Chromium matches on one IO thread.
pub struct Blocker {
    engine: adblock::Engine,
}

impl Blocker {
    /// Deserializes the engine cached for these exact lists, or compiles the
    /// lists and caches the result for the next launch.
    pub fn load(lists_dir: &Path, cache_dir: &Path) -> Result<Self> {
        let lists = LISTS
            .iter()
            .map(|name| {
                fs::read_to_string(lists_dir.join(name))
                    .with_context(|| format!("Couldn't read the {name} filter list"))
            })
            .collect::<Result<Vec<_>>>()?;
        let key = LISTS.iter().zip(&lists).fold(
            fnv1a(FNV_OFFSET, CACHE_VERSION.as_bytes()),
            |hash, (name, list)| {
                let hash = fnv1a(hash, name.as_bytes());
                let hash = fnv1a(hash, &(list.len() as u64).to_le_bytes());
                fnv1a(hash, list.as_bytes())
            },
        );
        let cache = cache_dir.join(format!("engine-{key:016x}.dat"));
        if let Ok(bytes) = fs::read(&cache) {
            let mut engine = adblock::Engine::default();
            // A damaged or foreign file fails its checksum and is rebuilt.
            if engine.deserialize(&bytes).is_ok() {
                return Ok(Self { engine });
            }
        }
        let mut filters = FilterSet::new(false);
        let options = ParseOptions {
            rule_types: RuleTypes::NetworkOnly,
            ..ParseOptions::default()
        };
        for list in lists {
            filters.add_filter_list(list, options);
        }
        let engine = adblock::Engine::new_with_filter_set(filters);
        if let Err(error) = write_cache(&cache, &engine.serialize()) {
            eprintln!("Couldn't cache the content blocker: {error}");
        }
        Ok(Self { engine })
    }

    /// `source_url` is the requesting document (its origin is enough); it
    /// decides third-party and per-domain rules.
    pub fn should_block(&self, url: &str, source_url: &str, kind: ResourceKind) -> bool {
        if kind == ResourceKind::MainFrame {
            return false;
        }
        Request::new(url, source_url, kind.filter_type(), "")
            .is_ok_and(|request| self.engine.check_network_request(&request).should_block())
    }
}

/// Shared by the shell, the loader thread and Chromium's IO thread. Allows
/// everything until the engine has loaded.
#[derive(Clone, Default)]
pub struct ContentBlocker {
    blocker: Arc<OnceLock<Blocker>>,
    /// Page hosts where blocking is off, without a leading "www.".
    exceptions: Arc<RwLock<HashSet<String>>>,
}

impl ContentBlocker {
    /// Loads the bundled lists on a background thread, so neither the main
    /// thread nor Chromium's IO thread waits for compilation or disk reads.
    pub fn load_in_background(cache_dir: PathBuf) -> Self {
        let content_blocker = Self::default();
        let blocker = content_blocker.blocker.clone();
        let spawned = thread::Builder::new()
            .name("content-blocker".into())
            .spawn(move || {
                let started = Instant::now();
                match lists_dir().and_then(|lists| Blocker::load(&lists, &cache_dir)) {
                    Ok(loaded) => {
                        let _ = blocker.set(loaded);
                        if std::env::var_os("MAPBROWSER_TRACE_CEF").is_some() {
                            eprintln!("Content blocker ready in {:?}", started.elapsed());
                        }
                    }
                    Err(error) => eprintln!("Content blocking is off: {error:#}"),
                }
            });
        if let Err(error) = spawned {
            eprintln!("Content blocking is off: {error}");
        }
        content_blocker
    }

    pub fn set_exceptions(&self, hosts: impl IntoIterator<Item = String>) {
        let hosts = hosts
            .into_iter()
            .filter_map(|host| normalize_host(&host))
            .collect();
        *self
            .exceptions
            .write()
            .unwrap_or_else(PoisonError::into_inner) = hosts;
    }

    /// `page_url` (the tab's top-level URL) is only resolved for requests a
    /// rule blocks while exceptions exist; without it the initiator counts as
    /// the page.
    pub fn should_block(
        &self,
        url: &str,
        initiator: &str,
        kind: ResourceKind,
        page_url: impl FnOnce() -> Option<String>,
    ) -> bool {
        let Some(blocker) = self.blocker.get() else {
            return false;
        };
        if !blocker.should_block(url, initiator, kind) {
            return false;
        }
        let exceptions = self
            .exceptions
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        if exceptions.is_empty() {
            return true;
        }
        let page_url = page_url();
        let page_host = adblock::url_parser::parse_url(page_url.as_deref().unwrap_or(initiator))
            .and_then(|page| normalize_host(page.hostname()));
        // An exception for a domain also covers its subdomains, as
        // `BrowserState::is_blocking_disabled` shows it.
        !page_host.is_some_and(|host| host_and_parents(&host).any(|host| exceptions.contains(host)))
    }
}

/// The macOS bundle carries the lists in Contents/Resources/filters; Linux
/// previews and tests read the repository copy.
fn lists_dir() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let exe = std::env::current_exe()?;
        Ok(exe
            .parent()
            .context("executable path")?
            .join("../Resources/filters"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/resources/filters"
        )))
    }
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// Stable across Rust releases, unlike `DefaultHasher`, so cache keys survive
/// toolchain updates.
fn fnv1a(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Replaces the cache atomically and removes engines built from older lists.
/// No fsync: a torn file fails adblock's checksum and is rebuilt.
fn write_cache(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(dir)?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temporary, bytes)
        .and_then(|()| fs::rename(&temporary, path))
        .inspect_err(|_| {
            let _ = fs::remove_file(&temporary);
        })?;
    for entry in fs::read_dir(dir)? {
        let stale = entry?.path();
        let is_engine = stale
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("engine-") && name.ends_with(".dat"));
        if is_engine && stale != path {
            let _ = fs::remove_file(stale);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            LazyLock,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    const NEWS: &str = "https://www.nytimes.com/2026/10/07/world/europe/story.html";

    /// The shipped lists, compiled once for every test in this run.
    static SHIPPED: LazyLock<Blocker> = LazyLock::new(|| {
        let cache = TestDir::new();
        Blocker::load(&lists_dir().unwrap(), &cache.0).unwrap()
    });

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "mapbrowser-blocker-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn write_lists(&self, easylist: &str, easyprivacy: &str) {
            fs::write(self.0.join("easylist.txt"), easylist).unwrap();
            fs::write(self.0.join("easyprivacy.txt"), easyprivacy).unwrap();
        }

        fn files(&self) -> Vec<PathBuf> {
            let mut files: Vec<_> = fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            files.sort();
            files
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn loaded(rules: &str) -> (ContentBlocker, TestDir) {
        let lists = TestDir::new();
        lists.write_lists(rules, "");
        let content_blocker = ContentBlocker::default();
        let blocker = Blocker::load(&lists.0, &lists.0.join("cache")).unwrap();
        assert!(content_blocker.blocker.set(blocker).is_ok());
        (content_blocker, lists)
    }

    fn page(url: &'static str) -> impl FnOnce() -> Option<String> {
        move || Some(url.to_owned())
    }

    #[test]
    fn news_page_ads_and_trackers_are_blocked() {
        for (url, kind) in [
            (
                "https://securepubads.g.doubleclick.net/tag/js/gpt.js",
                ResourceKind::Script,
            ),
            (
                "https://pagead2.googlesyndication.com/pagead/js/adsbygoogle.js?client=ca-pub-1",
                ResourceKind::Script,
            ),
            (
                "https://tpc.googlesyndication.com/safeframe/1-0-45/html/container.html",
                ResourceKind::SubFrame,
            ),
            (
                "https://www.google-analytics.com/analytics.js",
                ResourceKind::Script,
            ),
            (
                "https://www.googletagmanager.com/gtm.js?id=GTM-ABC123",
                ResourceKind::Script,
            ),
            (
                "https://sb.scorecardresearch.com/p?c1=2&c2=3005059&cv=3.6",
                ResourceKind::Image,
            ),
            (
                "https://c.amazon-adsystem.com/aax2/apstag.js",
                ResourceKind::Script,
            ),
        ] {
            assert!(
                SHIPPED.should_block(url, "https://www.nytimes.com", kind),
                "allowed {url}"
            );
        }
    }

    #[test]
    fn first_party_resources_and_pages_are_allowed() {
        for (url, kind) in [
            (
                "https://static01.nyt.com/images/2026/10/07/multimedia/photo.jpg",
                ResourceKind::Image,
            ),
            (
                "https://www.nytimes.com/vi-assets/static-assets/main.js",
                ResourceKind::Script,
            ),
            (
                "https://www.nytimes.com/svc/weather/v2/current.json",
                ResourceKind::Xhr,
            ),
            (
                "https://fonts.gstatic.com/s/inter/v13/inter.woff2",
                ResourceKind::Font,
            ),
            (
                "https://www.youtube.com/embed/dQw4w9WgXcQ",
                ResourceKind::SubFrame,
            ),
        ] {
            assert!(
                !SHIPPED.should_block(url, "https://www.nytimes.com", kind),
                "blocked {url}"
            );
        }
        // Typing or following a link to an ad domain still opens it.
        for url in [
            "https://doubleclick.net/",
            "https://www.google-analytics.com/",
        ] {
            assert!(!SHIPPED.should_block(url, "", ResourceKind::MainFrame));
            assert!(!SHIPPED.should_block(url, NEWS, ResourceKind::MainFrame));
        }
    }

    #[test]
    fn list_exceptions_follow_the_requesting_site() {
        let gtm = "https://www.googletagmanager.com/gtm.js?id=GTM-ABC123";
        assert!(!SHIPPED.should_block(gtm, "https://9to5mac.com", ResourceKind::Script));
        assert!(SHIPPED.should_block(gtm, "https://www.nytimes.com", ResourceKind::Script));
    }

    #[test]
    fn site_exceptions_disable_blocking_for_that_page_and_its_subdomains() {
        let (blocker, _lists) = loaded("||ads.example^\n");
        let ad = "https://ads.example/banner.js";
        let origin = "https://www.nytimes.com";
        assert!(blocker.should_block(ad, origin, ResourceKind::Script, page(NEWS)));

        blocker.set_exceptions(["WWW.NYTimes.com.".to_owned(), " ".to_owned()]);
        assert_eq!(
            *blocker.exceptions.read().unwrap(),
            HashSet::from(["nytimes.com".to_owned()])
        );
        for page_url in [NEWS, "https://nytimes.com/", "https://cooking.nytimes.com/"] {
            assert!(!blocker.should_block(ad, origin, ResourceKind::Script, page(page_url)));
        }
        // Requests from embedded frames follow the top-level page.
        let frame = "https://ads.example";
        assert!(!blocker.should_block(ad, frame, ResourceKind::Script, page(NEWS)));
        for page_url in [
            "https://www.theguardian.com/world",
            "https://notnytimes.com/",
        ] {
            assert!(blocker.should_block(ad, origin, ResourceKind::Script, page(page_url)));
        }
        // Service workers have no page; their origin stands in for it.
        assert!(!blocker.should_block(ad, origin, ResourceKind::Script, || None));
        let other = "https://example.org";
        assert!(blocker.should_block(ad, other, ResourceKind::Script, || None));

        blocker.set_exceptions([]);
        assert!(blocker.should_block(ad, origin, ResourceKind::Script, page(NEWS)));
    }

    #[test]
    fn page_url_is_resolved_only_for_blocked_requests_with_exceptions() {
        let (blocker, _lists) = loaded("||ads.example^\n");
        let unreachable = || -> Option<String> { panic!("page URL resolved") };
        let origin = "https://news.example";
        let ad = "https://ads.example/a.js";
        assert!(blocker.should_block(ad, origin, ResourceKind::Script, unreachable));
        blocker.set_exceptions(["other.example".to_owned()]);
        let asset = "https://cdn.news.example/a.js";
        assert!(!blocker.should_block(asset, origin, ResourceKind::Script, unreachable));
        assert!(blocker.should_block(ad, origin, ResourceKind::Script, page(NEWS)));
    }

    #[test]
    fn nothing_is_blocked_before_the_engine_loads() {
        let blocker = ContentBlocker::default();
        let ad = "https://securepubads.g.doubleclick.net/tag/js/gpt.js";
        assert!(!blocker.should_block(
            ad,
            "https://www.nytimes.com",
            ResourceKind::Script,
            page(NEWS)
        ));
    }

    #[test]
    fn cache_round_trips_and_is_replaced_when_lists_change() {
        let lists = TestDir::new();
        let cache = TestDir::new();
        let ad = "https://ads.example/a.js";
        lists.write_lists("||ads.example^\n", "||tracker.example^\n");
        let built = Blocker::load(&lists.0, &cache.0).unwrap();
        assert!(built.should_block(ad, NEWS, ResourceKind::Script));
        let engines = cache.files();
        assert_eq!(engines.len(), 1);

        // The next launch deserializes instead of compiling: plant an engine
        // with different rules under the same key.
        let planted = adblock::Engine::new_with_list_text("||planted.example^");
        fs::write(&engines[0], planted.serialize()).unwrap();
        let cached = Blocker::load(&lists.0, &cache.0).unwrap();
        let planted_ad = "https://planted.example/a.js";
        assert!(cached.should_block(planted_ad, NEWS, ResourceKind::Script));
        assert!(!cached.should_block(ad, NEWS, ResourceKind::Script));

        // Changed lists get a new key, and the stale engine is removed.
        lists.write_lists("||ads.example^\n||more.example^\n", "||tracker.example^\n");
        let rebuilt = Blocker::load(&lists.0, &cache.0).unwrap();
        assert!(rebuilt.should_block("https://more.example/a.js", NEWS, ResourceKind::Script));
        let replaced = cache.files();
        assert_eq!(replaced.len(), 1);
        assert_ne!(replaced, engines);

        // A damaged cache is rebuilt rather than trusted.
        fs::write(&replaced[0], b"not an engine").unwrap();
        let repaired = Blocker::load(&lists.0, &cache.0).unwrap();
        let tracker = "https://tracker.example/t.gif";
        assert!(repaired.should_block(tracker, NEWS, ResourceKind::Image));
        assert_ne!(fs::read(&replaced[0]).unwrap(), b"not an engine");
    }

    #[test]
    fn missing_lists_are_reported() {
        let lists = TestDir::new();
        let error = Blocker::load(&lists.0, &lists.0).err().unwrap();
        assert!(format!("{error:#}").contains("easylist.txt"), "{error:#}");
    }

    #[test]
    fn cache_key_tracks_the_locked_adblock_version() {
        let version = include_str!("../Cargo.lock")
            .split("[[package]]")
            .find_map(|package| {
                package
                    .strip_prefix("\nname = \"adblock\"\nversion = \"")?
                    .split('"')
                    .next()
            })
            .unwrap();
        assert_eq!(CACHE_VERSION, format!("adblock-{version}/network"));
    }

    /// Prints build, cache and matching costs for the shipped lists. Run it
    /// alone in release:
    /// `cargo test --release --bin mapbrowser measure -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn measure_shipped_lists() {
        fn resident_kib() -> u64 {
            fs::read_to_string("/proc/self/statm")
                .ok()
                .and_then(|statm| statm.split_whitespace().nth(1)?.parse::<u64>().ok())
                .map_or(0, |pages| pages * 4)
        }
        let cache = TestDir::new();
        let lists = lists_dir().unwrap();
        let before = resident_kib();
        let started = Instant::now();
        let built = Blocker::load(&lists, &cache.0).unwrap();
        let build = started.elapsed();
        let after_build = resident_kib();
        let cache_size = fs::metadata(&cache.files()[0]).unwrap().len();
        drop(built);
        let before_cached = resident_kib();
        let started = Instant::now();
        let cached = Blocker::load(&lists, &cache.0).unwrap();
        let cached_load = started.elapsed();
        let after_cached = resident_kib();
        let started = Instant::now();
        std::hint::black_box(LISTS.iter().fold(FNV_OFFSET, |hash, name| {
            fnv1a(hash, &fs::read(lists.join(name)).unwrap())
        }));
        let hashing = started.elapsed();

        let requests: Vec<_> = (0..2_000)
            .flat_map(|i| {
                [
                    (
                        format!("https://static01.nyt.com/images/{i}/a.jpg"),
                        ResourceKind::Image,
                    ),
                    (
                        format!("https://www.nytimes.com/svc/{i}/data.json"),
                        ResourceKind::Xhr,
                    ),
                    (
                        format!("https://cdn.example.net/lib-{i}.js"),
                        ResourceKind::Script,
                    ),
                    (
                        format!("https://securepubads.g.doubleclick.net/gampad/ads?slot={i}"),
                        ResourceKind::Xhr,
                    ),
                    (
                        format!("https://www.google-analytics.com/g/collect?v=2&n={i}"),
                        ResourceKind::Ping,
                    ),
                ]
            })
            .collect();
        let started = Instant::now();
        let blocked = requests
            .iter()
            .filter(|(url, kind)| cached.should_block(url, "https://www.nytimes.com", *kind))
            .count();
        let per_request = started.elapsed() / requests.len() as u32;
        println!(
            "build {build:?}, cache {} KiB, cached load {cached_load:?} (reading and \
             hashing the lists {hashing:?}), per request {per_request:?} ({} requests, \
             {blocked} blocked), RSS +{} KiB after build, +{} KiB after cached load",
            cache_size / 1024,
            requests.len(),
            after_build.saturating_sub(before),
            after_cached.saturating_sub(before_cached),
        );
        assert_eq!(blocked, requests.len() * 2 / 5);
        assert!(per_request < Duration::from_millis(1));
    }
}
