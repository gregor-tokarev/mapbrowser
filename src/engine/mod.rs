//! Web page engine. macOS embeds Chromium; other platforms build an engine
//! that emulates page events so the GPUI shell can be previewed and tested
//! without web views.

#[cfg(target_os = "macos")]
mod mac;
pub mod page;
#[cfg(target_os = "macos")]
pub use mac::Engine;

#[cfg(not(target_os = "macos"))]
mod stub;
#[cfg(not(target_os = "macos"))]
pub use stub::Engine;

use std::path::PathBuf;

/// Browser shortcuts. On macOS the engine reports those pressed in the
/// browser window. A focused web page gets all but the reserved ones
/// (`Shortcut::reserved`) first, as in Chrome; they run only if the page
/// leaves the key alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    /// ⌘T: open the command bar to start a new tab.
    CommandBar,
    /// ⌘L: edit the current tab's address in the command bar.
    EditAddress,
    /// ⌘W
    CloseTab,
    /// ⇧⌘T
    ReopenClosedTab,
    /// ⌃Tab
    NextTab,
    /// ⌃⇧Tab
    PreviousTab,
    /// ⌥⌘↓: the next tab in sidebar order.
    NextTabInList,
    /// ⌥⌘↑: the previous tab in sidebar order.
    PreviousTabInList,
    /// ⌘1–⌘9, 1-based.
    SelectTab(u8),
    /// ⌃1–⌃9, 1-based.
    SelectSpace(u8),
    /// ⌥⌘←
    PreviousSpace,
    /// ⌥⌘→
    NextSpace,
    /// ⌘R
    Reload,
    /// ⌘[ or ⌘←
    Back,
    /// ⌘] or ⌘→
    Forward,
    /// ⌘S
    ToggleSidebar,
    /// ⌘D
    TogglePin,
    /// ⇧⌘C
    CopyUrl,
    /// ⇧⌘K
    ClearToday,
    /// ⌘+ (or ⌘=): the page larger, in Chrome's zoom steps.
    ZoomIn,
    /// ⌘−
    ZoomOut,
    /// ⌘0: the page at its default size.
    ZoomReset,
    /// ⌘.: stop loading the page.
    Stop,
    /// ⌥⌘I: open or close the page's developer tools.
    DevTools,
    /// ⌘F: find in the page.
    Find,
    /// ⌘G
    FindNext,
    /// ⇧⌘G
    FindPrevious,
    /// ⌘P: print the page.
    Print,
}

#[cfg(any(target_os = "macos", test))]
impl Shortcut {
    /// Shortcuts taken before a focused page sees the key, Chrome's reserved
    /// commands: opening, closing and switching tabs and spaces, and ⌘L. A
    /// page can't keep the user in it by claiming them. Pages get every other
    /// shortcut first (⇧⌘K deletes a line in web editors, ⌘D duplicates in
    /// Figma, ⌘F opens Google Docs' own search) and the browser acts only
    /// if the page leaves it unhandled.
    pub fn reserved(self) -> bool {
        matches!(
            self,
            Self::CommandBar
                | Self::EditAddress
                | Self::CloseTab
                | Self::ReopenClosedTab
                | Self::NextTab
                | Self::PreviousTab
                | Self::NextTabInList
                | Self::PreviousTabInList
                | Self::SelectTab(_)
                | Self::SelectSpace(_)
                | Self::PreviousSpace
                | Self::NextSpace
        )
    }
}

/// Page zoom, in Chrome's steps (90%, 100%, 110%, 125%, ...).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoomStep {
    In,
    Out,
    Reset,
}

#[derive(Clone, Debug)]
// Only the Chromium engine reports every event, and a shell may leave some
// payloads unread (previews show no favicons).
#[allow(dead_code)]
pub enum EngineEvent {
    /// A browser exists for the tab; call `Engine::register_pending`.
    Created {
        tab_id: u64,
    },
    Title {
        tab_id: u64,
        title: String,
    },
    Address {
        tab_id: u64,
        url: String,
    },
    Loading {
        tab_id: u64,
        loading: bool,
        can_go_back: bool,
        can_go_forward: bool,
    },
    /// Load progress in 0.0..=1.0, coalesced to steps of at least 0.1 plus
    /// the final 1.0.
    Progress {
        tab_id: u64,
        progress: f64,
    },
    /// The page's favicon as PNG bytes, rendered for 2x displays. `host` is
    /// the page host it was fetched for: the tab may be on another site by
    /// the time the download finishes.
    Favicon {
        tab_id: u64,
        host: String,
        png: Vec<u8>,
    },
    /// A main-frame load failed. `url` is the address that failed: failed
    /// loads never report it as `Address`.
    LoadError {
        tab_id: u64,
        url: String,
        message: String,
    },
    /// A page asked for a new tab: a popup, a ⌘-, ⇧- or middle-click, or the
    /// context menu's "Open Link in New Tab". `url` is a web (or blob:, or
    /// from file: pages, file:) URL as Chromium resolved it: store it as it
    /// is, never as address-bar text. Background tabs must not be selected.
    OpenTab {
        opener_tab_id: u64,
        url: String,
        background: bool,
    },
    /// A page entered or left HTML fullscreen (video players, slides). The
    /// shell owns the transition: toggle window fullscreen, hide the sidebar
    /// and card insets, and call `Engine::set_fullscreen_layout`.
    Fullscreen {
        tab_id: u64,
        fullscreen: bool,
    },
    /// Text to put on the clipboard ("Copy Link Address").
    CopyText(String),
    /// A mouse button went down on the page. It takes keyboard focus, and
    /// the shell's popovers close as they would on any press outside them.
    /// Only macOS reports it; elsewhere the shell draws the whole window.
    PagePressed,
    /// The tab's browser is gone; the tab is unloaded. Closing takes a while,
    /// so this can arrive after `create_tab` made a new browser for the tab:
    /// ignore it while `Engine::has_tab` is true.
    Closed {
        tab_id: u64,
    },
    Shortcut(Shortcut),
    /// ⌃ was let go of, in any of the app's windows: a ⌃Tab walk ends.
    /// Only macOS reports it, as GPUI doesn't see keys while a page has
    /// keyboard focus.
    ControlReleased,
    /// Progress of a find in the page: `count` matches so far, the
    /// `active` one (1-based) selected. `final_update` ends the search.
    FindResult {
        tab_id: u64,
        count: u32,
        active: u32,
        final_update: bool,
    },
    /// A download started, saved straight to `path` in the Downloads
    /// folder. `blank_tab`: the tab never showed a page (a link that opened
    /// a new tab for the file), so it has nothing to keep it open for.
    /// Events stop once the tab's browser closes: keep it until the
    /// download finishes.
    DownloadStarted {
        id: u64,
        tab_id: u64,
        name: String,
        path: PathBuf,
        blank_tab: bool,
    },
    /// Bytes received so far, coalesced to steps of 5 % or one second.
    DownloadProgress {
        id: u64,
        received: u64,
        total: Option<u64>,
    },
    /// The download completed (`ok`), was cancelled or failed.
    DownloadFinished {
        id: u64,
        ok: bool,
    },
    /// A page asks to use `permissions` (sorted, without repeats). Answer
    /// once with `Engine::respond_permission`.
    PermissionRequest {
        id: u64,
        tab_id: u64,
        origin: String,
        permissions: Vec<Permission>,
    },
    /// The request ended unanswered (the page navigated or closed).
    PermissionDismissed {
        id: u64,
    },
    /// The address of the link under the pointer (or keyboard focus) in
    /// the tab's page, shortened in the middle; empty once there is none.
    Status {
        tab_id: u64,
        text: String,
    },
}

/// Something a page asks permission to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
// Previews ask for a few kinds only.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub enum Permission {
    Camera,
    Microphone,
    /// The screen (getDisplayMedia), with or without its sound.
    Screen,
    Location,
    Notifications,
    Clipboard,
    Midi,
    /// Several downloads from one page.
    Downloads,
    /// Embedded content's own cookies and site data.
    StorageAccess,
    /// Anything else Chromium asks about.
    Other,
}

/// The answer to a permission request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Block,
    /// Neither: the card was closed. Chromium remembers nothing.
    Dismiss,
}

/// cef_media_access_permission_types_t, for OnRequestMediaAccessPermission.
#[cfg(any(target_os = "macos", test))]
const MEDIA_PERMISSIONS: [(u32, Permission); 4] = [
    (1, Permission::Microphone),
    (2, Permission::Camera),
    (4, Permission::Screen),
    (8, Permission::Screen),
];

/// cef_permission_request_types_t, for OnShowPermissionPrompt. Other bits
/// are `Permission::Other`.
#[cfg(any(target_os = "macos", test))]
const PROMPT_PERMISSIONS: [(u32, Permission); 10] = [
    (2, Permission::Camera),
    (4, Permission::Camera),
    (16, Permission::Clipboard),
    (32, Permission::StorageAccess),
    (256, Permission::Location),
    (4096, Permission::Microphone),
    (8192, Permission::Midi),
    (16384, Permission::Downloads),
    (32768, Permission::Notifications),
    (1 << 20, Permission::StorageAccess),
];

/// The permissions in a CEF media access bit set.
#[cfg(any(target_os = "macos", test))]
pub fn media_permissions(bits: u32) -> Vec<Permission> {
    permissions(bits, &MEDIA_PERMISSIONS)
}

/// The permissions in a CEF permission prompt bit set.
#[cfg(any(target_os = "macos", test))]
pub fn prompt_permissions(bits: u32) -> Vec<Permission> {
    permissions(bits, &PROMPT_PERMISSIONS)
}

#[cfg(any(target_os = "macos", test))]
fn permissions(bits: u32, table: &[(u32, Permission)]) -> Vec<Permission> {
    let mut permissions: Vec<Permission> = (0..u32::BITS)
        .map(|bit| 1 << bit)
        .filter(|flag| bits & flag != 0)
        .map(|flag| {
            table
                .iter()
                .find(|(known, _)| *known == flag)
                .map_or(Permission::Other, |(_, permission)| *permission)
        })
        .collect();
    permissions.sort();
    permissions.dedup();
    permissions
}

/// The name a download is saved under: the last part of Chromium's
/// suggestion, without characters Finder or the path would trip over.
#[cfg(any(target_os = "macos", test))]
pub fn download_file_name(suggested: &str) -> String {
    let name: String = suggested
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .chars()
        .map(|ch| {
            if ch == ':' || ch.is_control() {
                '_'
            } else {
                ch
            }
        })
        .collect();
    let name = name.trim().trim_start_matches('.');
    if name.is_empty() {
        "download".to_owned()
    } else {
        name.to_owned()
    }
}

/// `name` if it isn't `taken`, else the first free "name (1).ext",
/// "name (2).ext"…, as Chrome numbers them. ".tar.gz" and the like stay
/// whole.
#[cfg(any(target_os = "macos", test))]
pub fn unique_file_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(name) {
        return name.to_owned();
    }
    let lower = name.to_ascii_lowercase();
    let split = [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst"]
        .iter()
        .find(|double| lower.ends_with(*double) && lower.len() > double.len())
        .map(|double| name.len() - double.len())
        .or_else(|| name.rfind('.').filter(|&dot| dot > 0))
        .unwrap_or(name.len());
    let (stem, extension) = name.split_at(split);
    (1..)
        .map(|count| format!("{stem} ({count}){extension}"))
        .find(|candidate| !taken(candidate))
        .expect("an unused name")
}

/// Download updates arrive about twice a second. Report steps of at least
/// 5 %, or a second's worth of progress; downloads of unknown size only
/// the latter.
#[cfg(any(target_os = "macos", test))]
pub fn download_progress_worth_reporting(
    last: Option<(std::time::Instant, u64)>,
    now: std::time::Instant,
    received: u64,
    total: Option<u64>,
) -> bool {
    let Some((at, reported)) = last else {
        return true;
    };
    received != reported
        && (now.duration_since(at) >= std::time::Duration::from_secs(1)
            || total.is_some_and(|total| received.abs_diff(reported) * 20 >= total))
}

/// Modifier keys held with a key press.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub command: bool,
    pub control: bool,
    pub option: bool,
    pub shift: bool,
}

/// macOS virtual key codes (kVK_ANSI_*) of the digit keys 1 to 9.
#[cfg(any(target_os = "macos", test))]
const DIGIT_KEYS: [u16; 9] = [18, 19, 20, 21, 23, 22, 26, 28, 25];
/// macOS virtual key codes (kVK_ANSI_*) of the letters a to z.
#[cfg(any(target_os = "macos", test))]
const LETTER_KEYS: [u16; 26] = [
    0, 11, 8, 2, 14, 3, 5, 4, 34, 38, 40, 37, 46, 45, 31, 35, 12, 15, 1, 17, 32, 9, 13, 7, 16, 6,
];

/// The character a key press means for shortcuts, lowercased, found the way
/// Chromium's KeyboardCodeFromNSEvent does: the typed character (with ⌘ held,
/// layouts such as "Dvorak – QWERTY ⌘" switch to their command keymap), else
/// the character without modifiers, else, for layouts that type no ASCII on
/// that key (Cyrillic, Greek), the letter on that key in the US layout.
#[cfg(any(target_os = "macos", test))]
pub fn shortcut_character(characters: &str, ignoring_modifiers: &str, key: u16) -> Option<char> {
    fn ascii(text: &str) -> Option<char> {
        let mut chars = text.chars();
        let character = chars.next()?;
        (chars.next().is_none() && character.is_ascii_graphic())
            .then(|| character.to_ascii_lowercase())
    }
    ascii(characters)
        .or_else(|| ascii(ignoring_modifiers))
        .or_else(|| {
            let index = LETTER_KEYS.iter().position(|&code| code == key)?;
            Some(char::from(b'a' + u8::try_from(index).ok()?))
        })
}

/// Maps a key press to a browser shortcut. Letter and symbol shortcuts
/// follow the character the layout types (`shortcut_character`), as the
/// rest of macOS and the menu bar do, so ⌘Z on AZERTY undoes instead of
/// closing the tab and ⌘+ zooms on German keyboards. Tab, the arrows, the
/// brackets and the digits stay on their physical keys, which every layout
/// has. Every shortcut needs ⌘ or ⌃, so typing is never intercepted.
#[cfg(any(target_os = "macos", test))]
pub fn shortcut_for_key(
    key: u16,
    character: Option<char>,
    modifiers: Modifiers,
) -> Option<Shortcut> {
    let Modifiers {
        command,
        control,
        option,
        shift,
    } = modifiers;
    let digit = DIGIT_KEYS
        .iter()
        .position(|&code| code == key)
        .and_then(|index| u8::try_from(index + 1).ok());
    let letter = match (command, control, option, shift, character) {
        (true, false, false, false, Some('t')) => Some(Shortcut::CommandBar),
        (true, false, false, true, Some('t')) => Some(Shortcut::ReopenClosedTab),
        (true, false, false, false, Some('l')) => Some(Shortcut::EditAddress),
        (true, false, false, false, Some('w')) => Some(Shortcut::CloseTab),
        (true, false, false, false, Some('r')) => Some(Shortcut::Reload),
        (true, false, false, false, Some('s')) => Some(Shortcut::ToggleSidebar),
        (true, false, false, false, Some('d')) => Some(Shortcut::TogglePin),
        (true, false, false, true, Some('c')) => Some(Shortcut::CopyUrl),
        (true, false, false, true, Some('k')) => Some(Shortcut::ClearToday),
        (true, false, false, false, Some('f')) => Some(Shortcut::Find),
        (true, false, false, false, Some('g')) => Some(Shortcut::FindNext),
        (true, false, false, true, Some('g')) => Some(Shortcut::FindPrevious),
        (true, false, true, false, Some('i')) => Some(Shortcut::DevTools),
        (true, false, false, false, Some('.')) => Some(Shortcut::Stop),
        (true, false, false, false, Some('p')) => Some(Shortcut::Print),
        // "+" needs ⇧ on many layouts, where ⌘= means the same.
        (true, false, false, _, Some('=' | '+')) => Some(Shortcut::ZoomIn),
        (true, false, false, false, Some('-')) => Some(Shortcut::ZoomOut),
        (true, false, false, false, Some('0')) => Some(Shortcut::ZoomReset),
        _ => None,
    };
    if letter.is_some() {
        return letter;
    }
    Some(match (command, control, option, shift, key) {
        (false, true, false, false, 48) => Shortcut::NextTab,
        (false, true, false, true, 48) => Shortcut::PreviousTab,
        (true, false, true, false, 125) => Shortcut::NextTabInList,
        (true, false, true, false, 126) => Shortcut::PreviousTabInList,
        (true, false, true, false, 123) => Shortcut::PreviousSpace,
        (true, false, true, false, 124) => Shortcut::NextSpace,
        // ⌘← and ⌘→ as in Chrome and Arc. Pages get them first, so text
        // fields keep moving the caret to the line's start or end.
        (true, false, false, false, 33 | 123) => Shortcut::Back,
        (true, false, false, false, 30 | 124) => Shortcut::Forward,
        // kVK_ANSI_0, like the digits for layouts that type no "0" there.
        (true, false, false, false, 29) => Shortcut::ZoomReset,
        (true, false, false, false, _) => Shortcut::SelectTab(digit?),
        (false, true, false, false, _) => Shortcut::SelectSpace(digit?),
        _ => return None,
    })
}

/// Standard editing commands for the focused frame of the active page.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditCommand {
    SelectAll,
    Copy,
    Cut,
    Paste,
    Undo,
    Redo,
}

/// ⌘A, ⌘C, ⌘X, ⌘V, ⌘Z and ⇧⌘Z, by `shortcut_character` like the shortcuts.
#[cfg(any(target_os = "macos", test))]
pub fn edit_command(character: Option<char>, modifiers: Modifiers) -> Option<EditCommand> {
    let Modifiers {
        command: true,
        control: false,
        option: false,
        shift,
    } = modifiers
    else {
        return None;
    };
    Some(match (character?, shift) {
        ('a', false) => EditCommand::SelectAll,
        ('c', false) => EditCommand::Copy,
        ('x', false) => EditCommand::Cut,
        ('v', false) => EditCommand::Paste,
        ('z', false) => EditCommand::Undo,
        ('z', true) => EditCommand::Redo,
        _ => return None,
    })
}

/// Whether a page may open `url` in a new tab (a popup, ⌘-click or "Open
/// Link in New Tab"). Web pages and the blob: documents they make open;
/// file: pages only from file: pages, as Chromium allows. Everything else is
/// dropped: data: (Chromium refuses top-level data: navigations from pages,
/// against spoofing; a new tab would load it as if typed), about:,
/// javascript:, and external protocols such as mailto: or tel:, which a tab
/// can't show and which must never be saved as a tab.
#[cfg(any(target_os = "macos", test))]
pub fn opens_in_tab(url: &str, opener_url: &str) -> bool {
    fn scheme(url: &str) -> Option<String> {
        url.split_once(':')
            .map(|(scheme, _)| scheme.trim().to_ascii_lowercase())
    }
    match scheme(url).as_deref() {
        Some("http" | "https" | "blob") => true,
        Some("file") => scheme(opener_url).as_deref() == Some("file"),
        _ => false,
    }
}

/// What becomes of a navigation to a URL, by its scheme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Navigation {
    /// Web pages and Chromium's own documents load as usual.
    Load,
    /// Mail, phone, messages and FaceTime links open in their macOS app.
    OpenInApp,
    /// Any other app's protocol (zoommtg:, slack:, ...) is dropped quietly:
    /// no prompt, no error page, and never saved as a tab's address.
    Block,
}

pub fn navigation_for(url: &str) -> Navigation {
    let scheme = url
        .split_once(':')
        .map(|(scheme, _)| scheme.trim().to_ascii_lowercase())
        .unwrap_or_default();
    match scheme.as_str() {
        // Chromium's own schemes, including the PDF viewer's extension
        // frame and error pages. No scheme: Chromium resolves it.
        "" | "http" | "https" | "ws" | "wss" | "file" | "blob" | "data" | "filesystem"
        | "about" | "javascript" | "chrome" | "chrome-untrusted" | "chrome-extension"
        | "chrome-error" | "chrome-search" | "devtools" | "view-source" | "urn"
        | "uuid-in-package" => Navigation::Load,
        "mailto" | "tel" | "sms" | "facetime" | "facetime-audio" => Navigation::OpenInApp,
        _ => Navigation::Block,
    }
}

/// How a page asked for a new window (Chromium's window disposition).
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowRequest {
    /// `window.open` with a size or the `popup` feature.
    Popup,
    ForegroundTab,
    /// ⌘- or middle-click.
    BackgroundTab,
    /// ⇧-click, and anything else.
    Window,
}

/// Where a page's new window opens.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewWindow {
    /// A small window of its own whose `window.opener` stays connected, so
    /// sign-in and payment popups can report back to the page.
    Popup,
    /// A tab in the opener's space; see `opens_in_tab`.
    Tab { background: bool },
    /// Nothing opens.
    Blocked,
}

/// Popups need a click, as with Chrome's popup blocker. `target=_blank`
/// links and plain `window.open(url)` open tabs, Arc-style; script that
/// opens a blank window to fill in or navigate later (MSAL, PayPal) keeps
/// it as a popup, since only a popup keeps its opener.
#[cfg(any(target_os = "macos", test))]
pub fn new_window_for(request: WindowRequest, url: &str, user_gesture: bool) -> NewWindow {
    if !user_gesture {
        return NewWindow::Blocked;
    }
    match request {
        WindowRequest::Popup => NewWindow::Popup,
        _ if url.is_empty() || url == "about:blank" => NewWindow::Popup,
        request => NewWindow::Tab {
            background: request == WindowRequest::BackgroundTab,
        },
    }
}

/// A rectangle on screen in points, from the top-left of the main display,
/// as Chromium measures windows.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScreenRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// A popup's content area: the size and place the page asked for (pages
/// usually centre sign-in popups themselves), else 500 × 600 points
/// centred over the window it came from.
#[cfg(any(target_os = "macos", test))]
pub fn popup_rect(requested: [Option<i32>; 4], opener: ScreenRect) -> ScreenRect {
    let [x, y, width, height] = requested;
    let width = width.filter(|&width| width > 0).unwrap_or(500);
    let height = height.filter(|&height| height > 0).unwrap_or(600);
    ScreenRect {
        x: x.unwrap_or(opener.x + (opener.width - width) / 2),
        y: y.unwrap_or(opener.y + (opener.height - height) / 2),
        width,
        height,
    }
}

/// Chromium's zoom level as a percentage: each level is a factor of 1.2.
#[cfg(any(target_os = "macos", test))]
pub fn zoom_percent(level: f64) -> u32 {
    (1.2f64.powf(level) * 100.).round() as u32
}

/// Chrome's zoom presets, in percent.
#[cfg(any(not(target_os = "macos"), test))]
const ZOOM_PRESETS: [u32; 17] = [
    25, 33, 50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300, 400, 500,
];

/// The zoom after `step` from `percent`, through Chrome's presets.
#[cfg(any(not(target_os = "macos"), test))]
pub fn step_zoom(percent: u32, step: ZoomStep) -> u32 {
    match step {
        ZoomStep::In => ZOOM_PRESETS.into_iter().find(|&preset| preset > percent),
        ZoomStep::Out => ZOOM_PRESETS
            .into_iter()
            .rev()
            .find(|&preset| preset < percent),
        ZoomStep::Reset => Some(100),
    }
    .unwrap_or(percent)
}

/// Progress callbacks arrive many times per load. Report only steps of at
/// least 0.1 and the moment a load completes, so the shell redraws rarely.
#[cfg(any(target_os = "macos", test))]
pub fn progress_worth_reporting(last: Option<f64>, progress: f64) -> bool {
    last.is_none_or(|last| (progress >= 1.0 && last < 1.0) || (progress - last).abs() >= 0.1)
}

/// Adds features to a Chromium `--enable-features`/`--disable-features`
/// value. Chromium keeps only the last value of a repeated switch, so new
/// entries must be merged into the existing list. Entries may carry a field
/// trial (`Name<Trial`) or parameters (`Name:key/value`); a feature already
/// listed in any form is left as it is.
#[cfg(any(target_os = "macos", test))]
pub fn merge_features(existing: &str, add: &[&str]) -> String {
    fn name(entry: &str) -> &str {
        entry.split(['<', ':']).next().unwrap_or(entry)
    }
    let mut features: Vec<&str> = existing
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect();
    for feature in add {
        if !features.iter().any(|entry| name(entry) == name(feature)) {
            features.push(feature);
        }
    }
    features.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMAND: Modifiers = Modifiers {
        command: true,
        control: false,
        option: false,
        shift: false,
    };
    const CONTROL: Modifiers = Modifiers {
        command: false,
        control: true,
        option: false,
        shift: false,
    };

    const SHIFT_COMMAND: Modifiers = Modifiers {
        shift: true,
        ..COMMAND
    };
    const OPTION_COMMAND: Modifiers = Modifiers {
        option: true,
        ..COMMAND
    };
    const SHIFT_CONTROL: Modifiers = Modifiers {
        shift: true,
        ..CONTROL
    };

    fn with(modifiers: Modifiers, option: bool, shift: bool) -> Modifiers {
        Modifiers {
            option,
            shift,
            ..modifiers
        }
    }

    /// The US layout: each letter key types its own letter.
    fn us(letter: char) -> (u16, Option<char>) {
        let index = usize::from(letter as u8 - b'a');
        (LETTER_KEYS[index], Some(letter))
    }

    #[test]
    fn maps_every_browser_shortcut() {
        for ((key, character), modifiers, expected) in [
            (us('t'), COMMAND, Shortcut::CommandBar),
            (us('l'), COMMAND, Shortcut::EditAddress),
            (us('w'), COMMAND, Shortcut::CloseTab),
            (us('t'), SHIFT_COMMAND, Shortcut::ReopenClosedTab),
            ((48, None), CONTROL, Shortcut::NextTab),
            ((48, None), SHIFT_CONTROL, Shortcut::PreviousTab),
            ((125, None), OPTION_COMMAND, Shortcut::NextTabInList),
            ((126, None), OPTION_COMMAND, Shortcut::PreviousTabInList),
            ((123, None), OPTION_COMMAND, Shortcut::PreviousSpace),
            ((124, None), OPTION_COMMAND, Shortcut::NextSpace),
            (us('r'), COMMAND, Shortcut::Reload),
            ((33, Some('[')), COMMAND, Shortcut::Back),
            ((30, Some(']')), COMMAND, Shortcut::Forward),
            ((123, None), COMMAND, Shortcut::Back),
            ((124, None), COMMAND, Shortcut::Forward),
            (us('p'), COMMAND, Shortcut::Print),
            (us('s'), COMMAND, Shortcut::ToggleSidebar),
            (us('d'), COMMAND, Shortcut::TogglePin),
            (us('c'), SHIFT_COMMAND, Shortcut::CopyUrl),
            (us('k'), SHIFT_COMMAND, Shortcut::ClearToday),
            ((24, Some('=')), COMMAND, Shortcut::ZoomIn),
            ((24, Some('+')), SHIFT_COMMAND, Shortcut::ZoomIn),
            ((69, Some('+')), COMMAND, Shortcut::ZoomIn),
            ((27, Some('-')), COMMAND, Shortcut::ZoomOut),
            ((29, Some('0')), COMMAND, Shortcut::ZoomReset),
            ((47, Some('.')), COMMAND, Shortcut::Stop),
            (us('i'), OPTION_COMMAND, Shortcut::DevTools),
            (us('f'), COMMAND, Shortcut::Find),
            (us('g'), COMMAND, Shortcut::FindNext),
            (us('g'), SHIFT_COMMAND, Shortcut::FindPrevious),
        ] {
            assert_eq!(
                shortcut_for_key(key, character, modifiers),
                Some(expected),
                "{key} {character:?}"
            );
        }
    }

    #[test]
    fn maps_digits_in_number_row_order() {
        for (index, key) in DIGIT_KEYS.into_iter().enumerate() {
            let digit = index as u8 + 1;
            // AZERTY types "&é\"'(§è!ç" on these keys without ⇧.
            for character in [char::from(b'0' + digit), '&'] {
                assert_eq!(
                    shortcut_for_key(key, Some(character), COMMAND),
                    Some(Shortcut::SelectTab(digit))
                );
                assert_eq!(
                    shortcut_for_key(key, Some(character), CONTROL),
                    Some(Shortcut::SelectSpace(digit))
                );
            }
            assert_eq!(shortcut_for_key(key, None, SHIFT_COMMAND), None);
        }
        // kVK_ANSI_0 resets the zoom, also where it types "à" (AZERTY).
        assert_eq!(
            shortcut_for_key(29, Some('0'), COMMAND),
            Some(Shortcut::ZoomReset)
        );
        assert_eq!(
            shortcut_for_key(29, None, COMMAND),
            Some(Shortcut::ZoomReset)
        );
    }

    #[test]
    fn never_intercepts_typing() {
        for key in 0..=127 {
            for character in [None, Some('t'), Some('w'), Some('1')] {
                for (option, shift) in [(false, false), (false, true), (true, false), (true, true)]
                {
                    let modifiers = with(Modifiers::default(), option, shift);
                    assert_eq!(shortcut_for_key(key, character, modifiers), None, "{key}");
                    assert_eq!(edit_command(character, modifiers), None);
                }
            }
        }
    }

    #[test]
    fn leaves_editing_and_unknown_commands_to_the_focused_view() {
        // ⌘A, ⌘Z, ⌘X, ⌘C, ⌘V, ⌘Q, ⌘B, ⌘⇧Z, ⌘⌃F (window fullscreen), and the
        // text field motions ⌃A, ⌃E and ⌃K.
        for (letter, modifiers) in [
            ('a', CONTROL),
            ('e', CONTROL),
            ('k', CONTROL),
            ('a', COMMAND),
            ('z', COMMAND),
            ('x', COMMAND),
            ('c', COMMAND),
            ('v', COMMAND),
            ('q', COMMAND),
            ('b', COMMAND),
            ('z', SHIFT_COMMAND),
            (
                'f',
                Modifiers {
                    control: true,
                    ..COMMAND
                },
            ),
        ] {
            let (key, character) = us(letter);
            assert_eq!(
                shortcut_for_key(key, character, modifiers),
                None,
                "{letter}"
            );
        }
    }

    #[test]
    fn pages_get_shortcuts_first_except_for_tab_and_space_switching() {
        let reserved = |letter: char, modifiers| {
            let (key, character) = us(letter);
            shortcut_for_key(key, character, modifiers).map(Shortcut::reserved)
        };
        // A page can't keep the user from opening, closing or leaving it.
        for (letter, modifiers) in [
            ('t', COMMAND),
            ('t', SHIFT_COMMAND),
            ('w', COMMAND),
            ('l', COMMAND),
        ] {
            assert_eq!(reserved(letter, modifiers), Some(true), "{letter}");
        }
        for (key, modifiers) in [
            (48, CONTROL),
            (48, SHIFT_CONTROL),
            (18, COMMAND),
            (25, CONTROL),
            (123, OPTION_COMMAND),
            (126, OPTION_COMMAND),
        ] {
            assert_eq!(
                shortcut_for_key(key, None, modifiers).map(Shortcut::reserved),
                Some(true),
                "{key}"
            );
        }
        // Web apps use these: ⇧⌘K deletes a line in VS Code, ⌘D duplicates
        // in Figma, ⌘S saves, ⌘F searches Google Docs, ⌘[ indents.
        for (letter, modifiers) in [
            ('k', SHIFT_COMMAND),
            ('d', COMMAND),
            ('s', COMMAND),
            ('r', COMMAND),
            ('c', SHIFT_COMMAND),
            ('f', COMMAND),
            ('g', COMMAND),
            ('g', SHIFT_COMMAND),
            ('i', OPTION_COMMAND),
            ('p', COMMAND),
        ] {
            assert_eq!(reserved(letter, modifiers), Some(false), "{letter}");
        }
        for (key, character) in [
            (33, '['),
            (30, ']'),
            (24, '='),
            (27, '-'),
            (29, '0'),
            (47, '.'),
        ] {
            assert_eq!(
                shortcut_for_key(key, Some(character), COMMAND).map(Shortcut::reserved),
                Some(false),
                "{character}"
            );
        }
    }

    #[test]
    fn only_command_arrows_go_back_and_forward() {
        // ⇧⌘← selects to the line's start; ⌃⌘← and ⌘↑ are the system's.
        for (key, modifiers) in [
            (123, SHIFT_COMMAND),
            (124, SHIFT_COMMAND),
            (
                123,
                Modifiers {
                    control: true,
                    ..COMMAND
                },
            ),
            (126, COMMAND),
            (125, COMMAND),
        ] {
            assert_eq!(shortcut_for_key(key, None, modifiers), None, "{key}");
        }
        assert_eq!(
            shortcut_for_key(123, None, OPTION_COMMAND),
            Some(Shortcut::PreviousSpace)
        );
        // Pages get them first, as the caret moves with them in text fields.
        for key in [123, 124] {
            assert_eq!(
                shortcut_for_key(key, None, COMMAND).map(Shortcut::reserved),
                Some(false)
            );
        }
    }

    #[test]
    fn zoom_keys_follow_the_layout() {
        // German: "+" has its own key (the US "]"), where ⌘ zooms rather
        // than going forward; "-" is on the US "/" key.
        assert_eq!(
            shortcut_for_key(30, Some('+'), COMMAND),
            Some(Shortcut::ZoomIn)
        );
        assert_eq!(
            shortcut_for_key(44, Some('-'), COMMAND),
            Some(Shortcut::ZoomOut)
        );
        // AZERTY types "-" on the 6 key, like its menu bar.
        assert_eq!(
            shortcut_for_key(22, Some('-'), COMMAND),
            Some(Shortcut::ZoomOut)
        );
        // ⇧⌘- types "_", which is no shortcut.
        assert_eq!(shortcut_for_key(27, Some('_'), SHIFT_COMMAND), None);
    }

    #[test]
    fn shortcut_characters_follow_the_layout() {
        // US: ⌘T types "t".
        assert_eq!(shortcut_character("t", "t", 17), Some('t'));
        // ⇧ and Caps Lock type capitals.
        assert_eq!(shortcut_character("T", "T", 17), Some('t'));
        // ⌃ types a control character; the key's own character counts.
        assert_eq!(shortcut_character("\u{14}", "t", 17), Some('t'));
        // ⌥ types a symbol, likewise.
        assert_eq!(shortcut_character("†", "t", 17), Some('t'));
        // French AZERTY types "z" on the US W key and "q" on the US A key.
        assert_eq!(shortcut_character("z", "z", 13), Some('z'));
        assert_eq!(shortcut_character("q", "q", 0), Some('q'));
        // Dvorak types "," on the US W key: no letter, so no ⌘W.
        assert_eq!(shortcut_character(",", ",", 13), Some(','));
        // "Dvorak – QWERTY ⌘" switches to QWERTY while ⌘ is held.
        assert_eq!(shortcut_character("w", ",", 13), Some('w'));
        // Cyrillic types no ASCII: the US letter on that key stands in.
        assert_eq!(shortcut_character("ц", "ц", 13), Some('w'));
        assert_eq!(shortcut_character("", "", 13), Some('w'));
        // Non-letter keys in such layouts have no stand-in.
        assert_eq!(shortcut_character("х", "х", 33), None);
    }

    #[test]
    fn letter_shortcuts_follow_the_layout() {
        let press = |characters, key, modifiers| {
            let character = shortcut_character(characters, characters, key);
            (
                shortcut_for_key(key, character, modifiers),
                edit_command(character, modifiers),
            )
        };
        // AZERTY ⌘Z (the US W key) undoes instead of closing the tab, ⌘Q
        // (the US A key) quits instead of selecting all, ⌘A (the US Q key)
        // selects all, and ⌘W (the US Z key) closes the tab.
        assert_eq!(press("z", 13, COMMAND), (None, Some(EditCommand::Undo)));
        assert_eq!(press("q", 0, COMMAND), (None, None));
        assert_eq!(
            press("a", 12, COMMAND),
            (None, Some(EditCommand::SelectAll))
        );
        assert_eq!(press("w", 6, COMMAND), (Some(Shortcut::CloseTab), None));
        // QWERTZ ⌘Z (the US Y key) undoes and ⌘Y does nothing.
        assert_eq!(press("z", 16, COMMAND), (None, Some(EditCommand::Undo)));
        assert_eq!(press("y", 6, COMMAND), (None, None));
        // Dvorak ⌘, (the US W key) does nothing, ⌘Q (the US X key) doesn't
        // cut, and ⌘T (the US K key) opens the command bar.
        assert_eq!(press(",", 13, COMMAND), (None, None));
        assert_eq!(press("q", 7, COMMAND), (None, None));
        assert_eq!(press("'", 12, COMMAND), (None, None));
        assert_eq!(press("t", 40, COMMAND), (Some(Shortcut::CommandBar), None));
        // Cyrillic falls back to the US positions.
        assert_eq!(press("ц", 13, COMMAND), (Some(Shortcut::CloseTab), None));
        assert_eq!(press("я", 6, COMMAND), (None, Some(EditCommand::Undo)));
        assert_eq!(
            press("Я", 6, SHIFT_COMMAND),
            (None, Some(EditCommand::Redo))
        );
    }

    #[test]
    fn edit_commands_need_command_alone_or_with_shift_for_redo() {
        for (letter, expected) in [
            ('a', EditCommand::SelectAll),
            ('c', EditCommand::Copy),
            ('x', EditCommand::Cut),
            ('v', EditCommand::Paste),
            ('z', EditCommand::Undo),
        ] {
            assert_eq!(edit_command(Some(letter), COMMAND), Some(expected));
            assert_eq!(edit_command(Some(letter), OPTION_COMMAND), None);
            assert_eq!(edit_command(Some(letter), CONTROL), None);
        }
        assert_eq!(edit_command(Some('a'), SHIFT_COMMAND), None);
        assert_eq!(edit_command(None, COMMAND), None);
    }

    #[test]
    fn pages_open_tabs_only_for_urls_a_tab_can_show() {
        let page = "https://news.example/story";
        for url in [
            "https://example.com/",
            "HTTP://example.com/",
            "blob:https://news.example/2c4e-7f1a",
        ] {
            assert!(opens_in_tab(url, page), "{url}");
        }
        for url in [
            "mailto:alice@corp.com",
            "tel:+15550100",
            "sms:+15550100",
            "javascript:void(0)",
            "data:text/html,<h1>Sign in</h1>",
            "about:blank",
            "file:///etc/hosts",
            "chrome://settings",
            "facetime:alice@corp.com",
            "",
            "example.com",
        ] {
            assert!(!opens_in_tab(url, page), "{url}");
        }
        // Local pages may open other local files.
        assert!(opens_in_tab("file:///tmp/b.html", "file:///tmp/a.html"));
        assert!(opens_in_tab("https://example.com/", "file:///tmp/a.html"));
    }

    #[test]
    fn app_links_open_their_app_and_other_protocols_go_nowhere() {
        for url in [
            "https://example.com/",
            "HTTP://example.com/",
            "file:///tmp/a.html",
            "about:blank",
            "data:text/html,hi",
            "blob:https://example.com/1",
            "chrome://version",
            "devtools://devtools/bundled/inspector.html",
            "chrome-extension://mhjfbmdgcfjbbpaeojofohoefgiehjai/index.html",
            "chrome-error://chromewebdata/",
            "javascript:void(0)",
            "",
        ] {
            assert_eq!(navigation_for(url), Navigation::Load, "{url}");
        }
        for url in [
            "mailto:alice@corp.com",
            "MAILTO:alice@corp.com",
            "tel:+15550100",
            "sms:+15550100",
            "facetime:alice@corp.com",
            "facetime-audio:+15550100",
        ] {
            assert_eq!(navigation_for(url), Navigation::OpenInApp, "{url}");
        }
        for url in [
            "zoommtg://zoom.us/join?confno=1",
            "slack://open",
            "itms-apps://apps.apple.com/app/id1",
            "ftp://example.com/file",
            "x-apple.systempreferences:com.apple.preference.security",
        ] {
            assert_eq!(navigation_for(url), Navigation::Block, "{url}");
        }
    }

    #[test]
    fn sign_in_popups_get_windows_and_links_get_tabs() {
        let sign_in = "https://accounts.google.com/o/oauth2/auth";
        assert_eq!(
            new_window_for(WindowRequest::Popup, sign_in, true),
            NewWindow::Popup
        );
        // MSAL and PayPal open a blank window, then navigate it.
        for url in ["", "about:blank"] {
            assert_eq!(
                new_window_for(WindowRequest::ForegroundTab, url, true),
                NewWindow::Popup
            );
        }
        assert_eq!(
            new_window_for(WindowRequest::ForegroundTab, sign_in, true),
            NewWindow::Tab { background: false }
        );
        assert_eq!(
            new_window_for(WindowRequest::BackgroundTab, sign_in, true),
            NewWindow::Tab { background: true }
        );
        assert_eq!(
            new_window_for(WindowRequest::Window, sign_in, true),
            NewWindow::Tab { background: false }
        );
        // Without a click nothing opens, as with Chrome's popup blocker.
        for request in [WindowRequest::Popup, WindowRequest::ForegroundTab] {
            assert_eq!(new_window_for(request, sign_in, false), NewWindow::Blocked);
        }
    }

    #[test]
    fn popups_take_the_requested_rect_or_centre_over_the_opener() {
        let opener = ScreenRect {
            x: 100,
            y: 50,
            width: 1280,
            height: 820,
        };
        assert_eq!(
            popup_rect([Some(10), Some(20), Some(400), Some(300)], opener),
            ScreenRect {
                x: 10,
                y: 20,
                width: 400,
                height: 300
            }
        );
        assert_eq!(
            popup_rect([None, None, Some(400), Some(300)], opener),
            ScreenRect {
                x: 100 + 440,
                y: 50 + 260,
                width: 400,
                height: 300
            }
        );
        assert_eq!(
            popup_rect([None; 4], opener),
            ScreenRect {
                x: 100 + 390,
                y: 50 + 110,
                width: 500,
                height: 600
            }
        );
        // A zero size means none was given.
        assert_eq!(popup_rect([None, None, Some(0), None], opener).width, 500);
    }

    #[test]
    fn zoom_follows_chromes_steps() {
        assert_eq!(zoom_percent(0.), 100);
        assert_eq!(zoom_percent(1.1f64.ln() / 1.2f64.ln()), 110);
        assert_eq!(zoom_percent(0.9f64.ln() / 1.2f64.ln()), 90);
        assert_eq!(step_zoom(100, ZoomStep::In), 110);
        assert_eq!(step_zoom(110, ZoomStep::Out), 100);
        assert_eq!(step_zoom(105, ZoomStep::Out), 100);
        assert_eq!(step_zoom(500, ZoomStep::In), 500);
        assert_eq!(step_zoom(25, ZoomStep::Out), 25);
        assert_eq!(step_zoom(175, ZoomStep::Reset), 100);
    }

    #[test]
    fn coalesces_progress() {
        assert!(progress_worth_reporting(None, 0.1));
        assert!(!progress_worth_reporting(Some(0.1), 0.15));
        assert!(progress_worth_reporting(Some(0.1), 0.25));
        assert!(progress_worth_reporting(Some(0.95), 1.0));
        assert!(!progress_worth_reporting(Some(1.0), 1.0));
        // A new load starts over.
        assert!(progress_worth_reporting(Some(1.0), 0.1));
    }

    #[test]
    fn merges_features_without_dropping_existing_ones() {
        let cef = "GlicActorUi,AutofillActorMode,LensOverlay,KillOnInvalidNavigationHeaders";
        assert_eq!(
            merge_features(cef, &["MediaRouter", "LensOverlay"]),
            format!("{cef},MediaRouter")
        );
        assert_eq!(merge_features("", &["A", "B", "A"]), "A,B");
        assert_eq!(merge_features(" A , ,B<Trial", &["B", "C"]), "A,B<Trial,C");
        assert_eq!(merge_features("A:x/1", &["A"]), "A:x/1");
    }

    #[test]
    fn reads_permission_bits() {
        use Permission::*;
        // getUserMedia({audio, video}), and getDisplayMedia with its sound.
        assert_eq!(media_permissions(1 | 2), [Camera, Microphone]);
        assert_eq!(media_permissions(4 | 8), [Screen]);
        assert_eq!(media_permissions(0), []);
        assert_eq!(prompt_permissions(256), [Location]);
        assert_eq!(
            prompt_permissions(4 | 4096 | 32768),
            [Camera, Microphone, Notifications]
        );
        // Unknown and unnamed bits (window management, idle detection).
        assert_eq!(prompt_permissions(1 << 23 | 2048 | 16), [Clipboard, Other]);
    }

    #[test]
    fn download_names_are_single_safe_file_names() {
        assert_eq!(download_file_name("report.pdf"), "report.pdf");
        assert_eq!(download_file_name("../../etc/passwd"), "passwd");
        assert_eq!(download_file_name("a\\b:c.txt"), "b_c.txt");
        assert_eq!(download_file_name(".profile"), "profile");
        assert_eq!(download_file_name("  "), "download");
        assert_eq!(download_file_name("dir/"), "download");
    }

    #[test]
    fn numbers_names_that_are_taken() {
        let taken = |names: &'static [&'static str]| move |name: &str| names.contains(&name);
        assert_eq!(unique_file_name("a.pdf", taken(&[])), "a.pdf");
        assert_eq!(unique_file_name("a.pdf", taken(&["a.pdf"])), "a (1).pdf");
        assert_eq!(
            unique_file_name("a.pdf", taken(&["a.pdf", "a (1).pdf"])),
            "a (2).pdf"
        );
        assert_eq!(unique_file_name("README", taken(&["README"])), "README (1)");
        assert_eq!(
            unique_file_name("src.tar.gz", taken(&["src.tar.gz"])),
            "src (1).tar.gz"
        );
        assert_eq!(
            unique_file_name("my.app.zip", taken(&["my.app.zip"])),
            "my.app (1).zip"
        );
        assert_eq!(
            unique_file_name(".tar.gz", taken(&[".tar.gz"])),
            ".tar (1).gz"
        );
    }

    #[test]
    fn coalesces_download_progress() {
        let start = std::time::Instant::now();
        let later = |ms| start + std::time::Duration::from_millis(ms);
        let mb = 1_000_000;
        assert!(download_progress_worth_reporting(
            None,
            start,
            0,
            Some(100 * mb)
        ));
        let last = Some((start, mb));
        // Under 5 % and under a second: skipped.
        assert!(!download_progress_worth_reporting(
            last,
            later(500),
            4 * mb,
            Some(100 * mb)
        ));
        assert!(download_progress_worth_reporting(
            last,
            later(500),
            6 * mb,
            Some(100 * mb)
        ));
        assert!(download_progress_worth_reporting(
            last,
            later(1000),
            2 * mb,
            Some(100 * mb)
        ));
        // Unknown sizes report once a second; nothing new, nothing to say.
        assert!(!download_progress_worth_reporting(
            last,
            later(900),
            90 * mb,
            None
        ));
        assert!(download_progress_worth_reporting(
            last,
            later(1000),
            2 * mb,
            None
        ));
        assert!(!download_progress_worth_reporting(
            last,
            later(5000),
            mb,
            None
        ));
    }
}
