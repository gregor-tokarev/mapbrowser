//! Map Browser updates itself from its GitHub releases. Only the copies the
//! release workflow builds do (`MAPBROWSER_RELEASE` at build time), so a
//! bundle built with `make run` is never replaced.
//!
//! Every release carries `latest.json`, which names its version and, for
//! each processor, the app's zip with its size and SHA-256, and
//! `latest.json.minisig`, the manifest's minisign signature by the key
//! below. A check fetches both through `releases/latest/download/` and
//! verifies the signature before it reads anything else. For a newer
//! version it downloads the zip into the cache, checks its size and hash,
//! unpacks it with ditto and checks the bundle: its identifier and version,
//! its code signature, and that no quarantine flag is left on it, which
//! would make macOS ask again before the new copy opens.
//!
//! The update is installed when Map Browser quits, after Chromium has shut
//! down: its helpers start from the bundle, so it never changes under a
//! running browser. The staged bundle and the running one swap places in
//! one rename, so a failed install leaves the old app as it was; the old
//! one is deleted at the next launch.

use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fmt, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const FEED: &str =
    "https://github.com/gregor-tokarev/mapbrowser/releases/latest/download/latest.json";
/// The key releases are signed with. Its secret half is the repository's
/// MINISIGN_SECRET_KEY; a copy of it is needed to sign any later release.
const PUBLIC_KEY: &str = "RWRT+zeVLwKgLuRIwXJTx2WEc5fKhobSTfzEGMYBccY4SJuZucCNx5nu";
/// Larger than any manifest or signature.
const MANIFEST_LIMIT: u64 = 64 * 1024;
/// Larger than any app: CEF alone makes the zip about 130 MB.
const ARCHIVE_LIMIT: u64 = 1 << 30;

/// The update installed at quit, once one is staged.
static PENDING: Mutex<Option<Staged>> = Mutex::new(None);
/// Restart to Update: the app opens again once it has quit.
static RELAUNCH: AtomicBool = AtomicBool::new(false);

/// Whether this copy updates itself: release builds, and previews with
/// MAPBROWSER_UPDATE_DEMO, which act one out.
pub fn enabled() -> bool {
    (cfg!(target_os = "macos") && option_env!("MAPBROWSER_RELEASE").is_some()) || demo()
}

fn demo() -> bool {
    cfg!(not(target_os = "macos")) && std::env::var_os("MAPBROWSER_UPDATE_DEMO").is_some()
}

pub fn current() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo.toml's version")
}

/// What a check reports, in order: an update found and downloading, then
/// ready; or none newer; or why it failed.
pub enum Progress {
    UpToDate,
    Downloading(Version),
    Ready(Version),
    Failed(Failure),
}

/// Why a check or a download failed, in words that follow "Can't update: ".
#[derive(Debug)]
pub enum Failure {
    /// No network, or GitHub didn't answer.
    Offline,
    /// GitHub answered with an error.
    Server,
    /// The manifest's signature, the zip's hash or the bundle didn't check
    /// out.
    Invalid,
    /// The cache couldn't be written.
    Disk(io::Error),
    /// The app runs from somewhere it can't be replaced: a disk image, the
    /// copy macOS makes of an app opened where it was downloaded, or a
    /// folder the user can't change.
    Location,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Offline => f.write_str("no connection to GitHub"),
            Self::Server => f.write_str("GitHub sent an error"),
            Self::Invalid => f.write_str("the download is damaged"),
            Self::Disk(error) => write!(f, "{}", crate::app::save_failure_reason(error)),
            Self::Location => f.write_str("it isn't in the Applications folder"),
        }
    }
}

#[derive(Deserialize)]
struct Manifest {
    version: String,
    /// By `std::env::consts::ARCH`: "aarch64" or "x86_64".
    macos: HashMap<String, Asset>,
}

#[derive(Clone, Deserialize)]
struct Asset {
    url: String,
    size: u64,
    sha256: String,
}

struct Release {
    version: Version,
    asset: Asset,
}

struct Staged {
    version: Version,
    /// The unpacked bundle.
    app: PathBuf,
    /// Its folder in the cache, deleted when a newer update replaces it.
    dir: PathBuf,
}

/// Checks for a version newer than `newer_than` and downloads it, on a
/// thread of its own, reporting to `progress`.
pub fn start(newer_than: Version, progress: async_channel::Sender<Progress>) {
    let thread_progress = progress.clone();
    let spawned = std::thread::Builder::new()
        .name("updater".into())
        .spawn(move || {
            let report = |step| {
                let _ = thread_progress.send_blocking(step);
            };
            // What a launch after an install leaves: the old app.
            if PENDING.lock().expect("pending update").is_none() {
                remove_staged();
            }
            let release = match check(&newer_than) {
                Ok(Some(release)) => release,
                Ok(None) => return report(Progress::UpToDate),
                Err(failure) => return report(Progress::Failed(failure)),
            };
            report(Progress::Downloading(release.version.clone()));
            match stage(&release) {
                Ok(staged) => {
                    let version = staged.version.clone();
                    let replaced = PENDING.lock().expect("pending update").replace(staged);
                    if let Some(replaced) = replaced {
                        let _ = fs::remove_dir_all(replaced.dir);
                    }
                    report(Progress::Ready(version));
                }
                Err(failure) => report(Progress::Failed(failure)),
            }
        });
    if let Err(error) = spawned {
        let _ = progress.try_send(Progress::Failed(Failure::Disk(error)));
    }
}

/// Restart to Update: once the app has quit and installed the update, it
/// opens again.
pub fn relaunch_after_quit() {
    RELAUNCH.store(true, Ordering::Release);
}

/// Called last, after Chromium has shut down: installs the staged update,
/// if there is one, and opens the app again if Restart to Update quit it.
pub fn finish() {
    let staged = PENDING.lock().expect("pending update").take();
    if let Some(staged) = staged
        && let Err(error) = platform::install(&staged.app)
    {
        eprintln!("Couldn't install Map Browser {}: {error}", staged.version);
    }
    if RELAUNCH.load(Ordering::Acquire) {
        platform::relaunch();
    }
}

fn check(newer_than: &Version) -> Result<Option<Release>, Failure> {
    if demo() {
        std::thread::sleep(Duration::from_secs(1));
        let version = Version::new(newer_than.major, newer_than.minor + 1, 0);
        let asset = Asset {
            url: String::new(),
            size: 0,
            sha256: String::new(),
        };
        return Ok(Some(Release { version, asset }));
    }
    // Before downloading anything it couldn't install.
    platform::bundle()?;
    let feed = std::env::var("MAPBROWSER_UPDATE_FEED").unwrap_or_else(|_| FEED.to_owned());
    // A release published without the files has nothing to offer yet.
    let Some(manifest) = fetch(&feed)? else {
        return Ok(None);
    };
    let signature = fetch(&format!("{feed}.minisig"))?.ok_or(Failure::Server)?;
    verify(&manifest, &signature)?;
    let manifest: Manifest = serde_json::from_slice(&manifest).map_err(|_| Failure::Invalid)?;
    let version = Version::parse(&manifest.version).map_err(|_| Failure::Invalid)?;
    if version <= *newer_than {
        return Ok(None);
    }
    Ok(manifest
        .macos
        .get(std::env::consts::ARCH)
        .map(|asset| Release {
            version,
            asset: asset.clone(),
        }))
}

fn verify(manifest: &[u8], signature: &[u8]) -> Result<(), Failure> {
    let key = minisign_verify::PublicKey::from_base64(PUBLIC_KEY).expect("the release key");
    let signature = std::str::from_utf8(signature)
        .ok()
        .and_then(|text| minisign_verify::Signature::decode(text).ok())
        .ok_or(Failure::Invalid)?;
    key.verify(manifest, &signature, false)
        .map_err(|_| Failure::Invalid)
}

fn agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .user_agent(format!("MapBrowser/{}", env!("CARGO_PKG_VERSION")))
            .timeout_connect(Some(Duration::from_secs(20)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            // A connection that dies without a word would otherwise hold
            // the check, and every later one, forever. An hour fits the
            // zip at 50 KB/s.
            .timeout_recv_body(Some(Duration::from_secs(60 * 60)))
            .build(),
    )
}

/// A small file, or `None` if there is none at `url`.
fn fetch(url: &str) -> Result<Option<Vec<u8>>, Failure> {
    match agent().get(url).call() {
        Ok(mut response) => response
            .body_mut()
            .with_config()
            .limit(MANIFEST_LIMIT)
            .read_to_vec()
            .map(Some)
            .map_err(network_failure),
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(error) => Err(network_failure(error)),
    }
}

fn network_failure(error: ureq::Error) -> Failure {
    match error {
        ureq::Error::Io(_)
        | ureq::Error::Timeout(_)
        | ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed
        | ureq::Error::BodyStalled => Failure::Offline,
        ureq::Error::BodyExceedsLimit(_) => Failure::Invalid,
        _ => Failure::Server,
    }
}

/// Where updates are unpacked until they are installed.
fn updates_dir() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("MapBrowser").join("Updates"))
}

fn remove_staged() {
    if let Some(dir) = updates_dir()
        && let Err(error) = fs::remove_dir_all(&dir)
        && error.kind() != io::ErrorKind::NotFound
    {
        eprintln!("Couldn't remove an old update: {error}");
    }
}

fn stage(release: &Release) -> Result<Staged, Failure> {
    if demo() {
        std::thread::sleep(Duration::from_secs(2));
        return Ok(Staged {
            version: release.version.clone(),
            app: PathBuf::new(),
            dir: PathBuf::new(),
        });
    }
    let dir = updates_dir()
        .ok_or(Failure::Location)?
        .join(release.version.to_string());
    match fs::remove_dir_all(&dir) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(Failure::Disk(error)),
        _ => {}
    }
    fs::create_dir_all(&dir).map_err(Failure::Disk)?;
    let archive = dir.join("update.zip");
    download(&release.asset, &archive)?;
    let app = platform::unpack(&archive, &dir.join("app"))?;
    let _ = fs::remove_file(&archive);
    platform::check_bundle(&app, &release.version)?;
    Ok(Staged {
        version: release.version.clone(),
        app,
        dir,
    })
}

/// Downloads `asset` to `path`, hashing it as it arrives.
fn download(asset: &Asset, path: &Path) -> Result<(), Failure> {
    if asset.size > ARCHIVE_LIMIT {
        return Err(Failure::Invalid);
    }
    let response = agent().get(&asset.url).call().map_err(network_failure)?;
    let mut body = response
        .into_body()
        .into_with_config()
        .limit(asset.size + 1)
        .reader();
    let mut file = fs::File::create(path).map_err(Failure::Disk)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    let mut received = 0;
    loop {
        let read = match body.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(Failure::Offline),
        };
        received += read as u64;
        if received > asset.size {
            return Err(Failure::Invalid);
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read]).map_err(Failure::Disk)?;
    }
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if received != asset.size || !digest.eq_ignore_ascii_case(&asset.sha256) {
        return Err(Failure::Invalid);
    }
    file.sync_all().map_err(Failure::Disk)
}

#[cfg(target_os = "macos")]
mod platform {
    use super::Failure;
    use semver::Version;
    use std::{
        ffi::{CString, OsStr},
        fs, io,
        os::unix::ffi::OsStrExt,
        path::{Path, PathBuf},
        process::{Command, Stdio},
    };

    const BUNDLE_IDENTIFIER: &str = "dev.mapbrowser.browser";
    const QUARANTINE: &std::ffi::CStr = c"com.apple.quarantine";

    /// The running app's bundle, if it can be replaced.
    pub fn bundle() -> Result<PathBuf, Failure> {
        let executable = std::env::current_exe().map_err(Failure::Disk)?;
        // …/Map Browser.app/Contents/MacOS/mapbrowser
        let app = executable
            .ancestors()
            .nth(3)
            .filter(|app| app.extension() == Some(OsStr::new("app")))
            .ok_or(Failure::Location)?;
        // macOS runs an app opened where it was downloaded from a read-only
        // copy, until it is moved.
        let translocated = app
            .components()
            .any(|part| part.as_os_str() == "AppTranslocation");
        let parent = app.parent().ok_or(Failure::Location)?;
        if translocated || !writable(app) || !writable(parent) {
            return Err(Failure::Location);
        }
        Ok(app.to_owned())
    }

    fn writable(path: &Path) -> bool {
        let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: `path` is a valid NUL-terminated string for the call.
        unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
    }

    /// Unpacks the zip, which holds the bundle at its top, into `dir`.
    pub fn unpack(archive: &Path, dir: &Path) -> Result<PathBuf, Failure> {
        // --noqtn: the zip's quarantine flag stays off the bundle.
        if !run(Command::new("/usr/bin/ditto")
            .args(["-x", "-k", "--noqtn"])
            .arg(archive)
            .arg(dir))
        {
            return Err(Failure::Invalid);
        }
        let mut apps = fs::read_dir(dir)
            .map_err(Failure::Disk)?
            .filter_map(|entry| Some(entry.ok()?.path()))
            .filter(|path| path.extension() == Some(OsStr::new("app")));
        match (apps.next(), apps.next()) {
            (Some(app), None) => Ok(app),
            _ => Err(Failure::Invalid),
        }
    }

    /// The unpacked bundle is Map Browser at the manifest's version, sealed
    /// by its code signature, and macOS opens it without asking.
    pub fn check_bundle(app: &Path, version: &Version) -> Result<(), Failure> {
        let info = app.join("Contents/Info.plist");
        let identifier = plist_value(&info, "CFBundleIdentifier");
        let shown = plist_value(&info, "CFBundleShortVersionString")
            .and_then(|shown| Version::parse(&shown).ok());
        if identifier.as_deref() != Some(BUNDLE_IDENTIFIER) || shown.as_ref() != Some(version) {
            return Err(Failure::Invalid);
        }
        // Files this app writes are quarantined (`LSFileQuarantineEnabled`),
        // so the unpacked ones are too. The signature checked above vouches
        // for the download, as Sparkle's does.
        unquarantine(app);
        let sealed = run(Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(app));
        if !sealed || quarantined(app) || quarantined(&app.join("Contents/MacOS/mapbrowser")) {
            return Err(Failure::Invalid);
        }
        Ok(())
    }

    fn plist_value(plist: &Path, key: &str) -> Option<String> {
        let output = Command::new("/usr/bin/plutil")
            .args(["-extract", key, "raw", "-o", "-"])
            .arg(plist)
            .stderr(Stdio::null())
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    /// Takes the quarantine flag off `path` and everything in it, without
    /// following symlinks.
    fn unquarantine(path: &Path) {
        if let Ok(name) = CString::new(path.as_os_str().as_bytes()) {
            // SAFETY: Both are valid NUL-terminated strings for the call.
            // Files without the flag fail with ENOATTR, which is fine.
            unsafe { libc::removexattr(name.as_ptr(), QUARANTINE.as_ptr(), libc::XATTR_NOFOLLOW) };
        }
        if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
            && let Ok(entries) = fs::read_dir(path)
        {
            for entry in entries.flatten() {
                unquarantine(&entry.path());
            }
        }
    }

    fn quarantined(path: &Path) -> bool {
        let Ok(name) = CString::new(path.as_os_str().as_bytes()) else {
            return true;
        };
        // SAFETY: Valid NUL-terminated strings; a null buffer of size 0 only
        // asks for the value's size.
        let size = unsafe {
            libc::getxattr(
                name.as_ptr(),
                QUARANTINE.as_ptr(),
                std::ptr::null_mut(),
                0,
                0,
                libc::XATTR_NOFOLLOW,
            )
        };
        size >= 0
    }

    fn run(command: &mut Command) -> bool {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// Puts the staged bundle in the running app's place. The old app goes
    /// where the staged one was, in the cache.
    pub fn install(staged: &Path) -> io::Result<()> {
        let app = bundle().map_err(|_| io::Error::other("the app can't be replaced"))?;
        match swap(staged, &app) {
            // The app is on another volume than the cache: copy the update
            // next to it first.
            Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
                let mut name = app.file_name().unwrap_or_default().to_owned();
                name.push(".update");
                let copy = app.with_file_name(format!(".{}", name.to_string_lossy()));
                let _ = fs::remove_dir_all(&copy);
                if !run(Command::new("/usr/bin/ditto")
                    .arg("--noqtn")
                    .arg(staged)
                    .arg(&copy))
                {
                    let _ = fs::remove_dir_all(&copy);
                    return Err(io::Error::other("couldn't copy the update"));
                }
                unquarantine(&copy);
                let swapped = swap(&copy, &app);
                // The old app now, or the copy if the swap failed.
                let _ = fs::remove_dir_all(&copy);
                swapped
            }
            result => result,
        }
    }

    /// Exchanges two paths on the same volume in one step.
    fn swap(first: &Path, second: &Path) -> io::Result<()> {
        let first = CString::new(first.as_os_str().as_bytes())?;
        let second = CString::new(second.as_os_str().as_bytes())?;
        // SAFETY: Both are valid NUL-terminated paths for the call.
        let result =
            unsafe { libc::renamex_np(first.as_ptr(), second.as_ptr(), libc::RENAME_SWAP) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Opens the app once this process has gone: Chromium's profile lock
    /// and Launch Services' running copy go with it.
    pub fn relaunch() {
        let Some(app) = std::env::current_exe()
            .ok()
            .and_then(|executable| executable.ancestors().nth(3).map(Path::to_owned))
        else {
            return;
        };
        let _ = Command::new("/bin/sh")
            .args([
                "-c",
                "while kill -0 \"$0\" 2>/dev/null; do sleep 0.1; done; exec /usr/bin/open \"$1\"",
            ])
            .arg(std::process::id().to_string())
            .arg(app)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

/// Previews install nothing; with MAPBROWSER_UPDATE_DEMO, Restart starts
/// the preview again.
#[cfg(not(target_os = "macos"))]
mod platform {
    use super::Failure;
    use semver::Version;
    use std::{
        io,
        path::{Path, PathBuf},
        process::{Command, Stdio},
    };

    pub fn bundle() -> Result<PathBuf, Failure> {
        Err(Failure::Location)
    }

    pub fn unpack(_archive: &Path, _dir: &Path) -> Result<PathBuf, Failure> {
        Err(Failure::Location)
    }

    pub fn check_bundle(_app: &Path, _version: &Version) -> Result<(), Failure> {
        Err(Failure::Location)
    }

    pub fn install(_staged: &Path) -> io::Result<()> {
        Ok(())
    }

    pub fn relaunch() {
        let Ok(executable) = std::env::current_exe() else {
            return;
        };
        let _ = Command::new("/bin/sh")
            .args([
                "-c",
                "while kill -0 \"$0\" 2>/dev/null; do sleep 0.1; done; exec \"$1\"",
            ])
            .arg(std::process::id().to_string())
            .arg(executable)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}
