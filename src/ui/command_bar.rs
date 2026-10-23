//! Arc's command bar (⌘T, ⌘L). It opens in its own pop-up window over the
//! page card, because the native web views would cover anything GPUI draws
//! in the browser window. A new tab exists only once a result is picked.
//! The same bar lists the pages visited (History, ⌘Y) and the few settings
//! there are (⌘,).

use super::{
    UI_FONT,
    icons::{Icon, icon},
    placeholder_favicon, shadow,
    theme::Theme,
    truncated,
};
use crate::{
    engine::{Navigation, Shortcut, navigation_for},
    favicons::{host_of, normalize_host},
    history::VisitedPage,
    input::{self, InputEvent, InputStyle, TextInput},
    model::{self, Pin, SearchEngine, SpaceId, TabId},
};
use gpui::{
    App, AppContext, Bounds, Context, DisplayId, Entity, EventEmitter, FontWeight, Image,
    InteractiveElement, IntoElement, KeyBinding, MouseButton, ParentElement, Pixels, Point, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Subscription, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, actions, div,
    http_client::Url, img, point, prelude::FluentBuilder, px, size,
};
use std::{cmp::Reverse, collections::HashMap, sync::Arc};

actions!(command_bar, [SelectPrevious, SelectNext, SubmitInNewTab]);

/// Rows the list shows at most before it scrolls; fewer on a short card
/// (`visible_rows`).
const MAX_VISIBLE_ROWS: usize = 8;
/// Open tabs listed at most. Every matching tab can be reached: the list
/// scrolls.
const MAX_TABS: usize = 50;
/// After the tabs: recently closed tabs, visited pages and actions.
const MAX_CLOSED: usize = 3;
const MAX_PAGES: usize = 6;
const MAX_ACTIONS: usize = 4;
/// Pages the History view lists at most.
const MAX_HISTORY: usize = 200;
/// Fewer characters than this never name a tab (`named`).
const MIN_TAB_NAME: usize = 4;
const MAX_WIDTH: f32 = 760.;
const MIN_WIDTH: f32 = 320.;
const INPUT_HEIGHT: f32 = 68.;
const ROW_HEIGHT: f32 = 48.;
const LIST_PADDING_TOP: f32 = 7.;
const LIST_PADDING_BOTTOM: f32 = 8.;
/// The panel's least distance from the card's bottom edge.
const CARD_GAP: f32 = 10.;
/// Transparent room around the panel for its shadow. X11 previews have no
/// compositor, so there the window is exactly the panel.
pub(super) const TRANSPARENT: bool = cfg!(target_os = "macos");
const MARGIN: f32 = if TRANSPARENT { 48. } else { 0. };
const MARGIN_BOTTOM: f32 = if TRANSPARENT { 72. } else { 0. };

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SelectPrevious, Some("CommandBar")),
        KeyBinding::new("down", SelectNext, Some("CommandBar")),
        KeyBinding::new("ctrl-p", SelectPrevious, Some("CommandBar")),
        KeyBinding::new("ctrl-n", SelectNext, Some("CommandBar")),
        // As in Chrome's address bar.
        KeyBinding::new("tab", SelectNext, Some("CommandBar")),
        KeyBinding::new("shift-tab", SelectPrevious, Some("CommandBar")),
        // ⇧ still held after typing a capital doesn't swallow Return.
        KeyBinding::new("shift-enter", input::Submit, Some("CommandBar")),
        // As in Safari and Chrome: the address in a new tab, from ⌘L too.
        KeyBinding::new("cmd-enter", SubmitInNewTab, Some("CommandBar")),
        KeyBinding::new("alt-enter", SubmitInNewTab, Some("CommandBar")),
    ]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandMode {
    /// ⌘T: Enter opens a new tab.
    NewTab,
    /// ⌘L: the active tab's address, selected; Enter loads it in that tab.
    EditAddress,
    /// ⌘Y: the pages visited, the most recent first.
    History,
    /// ⌘,: the search engine and the other settings.
    Settings,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// A new tab for an address or search.
    Open(String),
    /// An address or search in the active tab.
    Navigate(String),
    /// A mail, phone or messages address, for its app.
    OpenInApp(String),
    Reload,
    SwitchToTab(TabId),
    SwitchToSpace(SpaceId),
    /// The most recently closed tab, back in its space.
    ReopenClosedTab,
    /// A recently closed tab, by its id, back in its space.
    ReopenTab(TabId),
    TogglePin,
    ToggleFavorite,
    ToggleSidebar,
    CopyUrl,
    ClearToday,
    NewSpace,
    DuplicateTab,
    MoveTab(SpaceId),
    RenameSpace,
    UnloadSpace,
    /// Content blocking on the host, off or back on.
    ToggleBlocking(String),
    /// What a browser shortcut does: close the tab, find, zoom, print.
    Shortcut(Shortcut),
    ShowHistory,
    ClearHistory,
    ShowSettings,
    SearchWith(SearchEngine),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemIcon {
    /// A page's favicon, by host.
    Page(Option<String>),
    Glyph(Icon),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandItem {
    pub command: Command,
    pub title: String,
    /// Shown after the title: a tab's host or space.
    pub detail: Option<String>,
    /// Shown at the right of the highlighted row: what Return does.
    pub hint: SharedString,
    pub icon: ItemIcon,
}

pub struct TabEntry {
    pub id: TabId,
    pub space_id: SpaceId,
    pub space_name: String,
    pub title: String,
    pub url: String,
}

pub struct ActiveTab {
    pub id: TabId,
    pub url: String,
    pub pin: Pin,
    pub host: Option<String>,
    pub blocking_disabled: bool,
}

/// A recently closed tab.
pub struct ClosedEntry {
    pub id: TabId,
    pub space_id: SpaceId,
    pub title: String,
    pub url: String,
}

/// A visited page, its text lowercased once for matching as you type.
pub struct HistoryEntry {
    pub url: String,
    /// Empty if the page had none.
    pub title: String,
    pub count: u32,
    /// The latest visit, in seconds since the Unix epoch.
    pub last: u64,
    title_lower: String,
    /// `url_text`, lowercased.
    address_lower: String,
}

impl From<VisitedPage> for HistoryEntry {
    fn from(page: VisitedPage) -> Self {
        Self {
            title_lower: page.title.to_lowercase(),
            address_lower: url_text(&page.url).to_lowercase(),
            url: page.url,
            title: page.title,
            count: page.count,
            last: page.last,
        }
    }
}

impl HistoryEntry {
    fn score(&self, words: &[String]) -> Option<u32> {
        score_lowercase(&self.title_lower, words).max(score_lowercase(&self.address_lower, words))
    }

    /// How often and how recently the page was visited: visits count less
    /// the older the last one is (half after a week).
    fn frecency(&self, now: u64) -> f64 {
        let days = now.saturating_sub(self.last) as f64 / 86_400.;
        f64::from(self.count) / (1. + days / 7.)
    }
}

/// What the results draw from, captured when the bar opens.
pub struct CommandContext {
    pub space_id: SpaceId,
    /// Every tab: the active space's first, each space in sidebar order.
    pub tabs: Vec<TabEntry>,
    /// The active space's tabs, the most recently shown first.
    pub recent: Vec<TabId>,
    pub active: Option<ActiveTab>,
    pub spaces: Vec<(SpaceId, String)>,
    /// Recently closed tabs, the most recent first.
    pub closed: Vec<ClosedEntry>,
    /// Visited pages, the most recent first.
    pub history: Vec<HistoryEntry>,
    /// Seconds since the Unix epoch, for how long ago pages were visited.
    pub now: u64,
    pub sidebar_hidden: bool,
    pub has_today: bool,
    pub search_engine: SearchEngine,
}

impl CommandContext {
    fn space_name(&self, id: SpaceId) -> Option<&str> {
        self.spaces
            .iter()
            .find(|(space, _)| *space == id)
            .map(|(_, name)| name.as_str())
    }
}

/// How well `text` matches every query word: 3 for a word at the start,
/// 2 at the start of one of its words, 1 anywhere; `None` if one is missing.
fn score(text: &str, words: &[String]) -> Option<u32> {
    score_lowercase(&text.to_lowercase(), words)
}

fn score_lowercase(text: &str, words: &[String]) -> Option<u32> {
    words
        .iter()
        .map(|word| {
            let at_word_start = |(index, _): (usize, &str)| {
                text[..index]
                    .chars()
                    .next_back()
                    .is_none_or(|before| !before.is_alphanumeric())
            };
            if text.starts_with(word.as_str()) {
                Some(3)
            } else if text.match_indices(word.as_str()).any(at_word_start) {
                Some(2)
            } else if text.contains(word.as_str()) {
                Some(1)
            } else {
                None
            }
        })
        .sum()
}

/// The lowercase words of a query.
fn query_words(query: &str) -> Vec<String> {
    query
        .to_lowercase()
        .split_whitespace()
        .map(String::from)
        .collect()
}

/// Lowercase, with single spaces.
fn normalized(text: &str) -> String {
    query_words(text).join(" ")
}

/// URLs match without their scheme and "www.".
fn url_text(url: &str) -> &str {
    let url = url.split_once("://").map_or(url, |(_, rest)| rest);
    url.strip_prefix("www.").unwrap_or(url)
}

/// One page, however its address is written: open tabs aren't offered
/// again as visited pages or closed tabs.
fn page_key(url: &str) -> String {
    url_text(url).trim_end_matches('/').to_lowercase()
}

/// A URL's host, with its port if it names one: "localhost:8000" is
/// another server than "localhost".
fn site(url: &str) -> Option<String> {
    let url = Url::parse(url).ok()?;
    let host = normalize_host(url.host_str()?)?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

/// How long ago a page was visited, in words.
fn ago(now: u64, then: u64) -> String {
    let minutes = now.saturating_sub(then) / 60;
    let (hours, days) = (minutes / 60, minutes / (60 * 24));
    let plural = |count: u64, unit: &str| {
        if count == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{count} {unit}s ago")
        }
    };
    if minutes < 1 {
        "Just now".into()
    } else if hours < 1 {
        plural(minutes, "minute")
    } else if days < 1 {
        plural(hours, "hour")
    } else if days < 14 {
        plural(days, "day")
    } else {
        plural(days / 7, "week")
    }
}

/// How clearly typed text names a tab or space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Naming {
    /// One of the name's words ("gmail" for "Inbox - me@gmail.com - Gmail").
    Word,
    /// The start of the name ("hack", "hacker ne" for "Hacker News").
    Prefix,
    Exact,
}

/// How `query` (lowercase, single spaces) names `name`, if clearly. Inside
/// a name only a whole word counts, and only typed alone: phrases are
/// mostly searches ("rust lang" for "The Rust Programming Language").
fn naming(name: &str, query: &str) -> Option<Naming> {
    let name = normalized(name);
    if name == query {
        Some(Naming::Exact)
    } else if name.starts_with(query) {
        Some(Naming::Prefix)
    } else if !query.contains(' ')
        && name
            .split(|ch: char| !ch.is_alphanumeric())
            .any(|word| word == query)
    {
        Some(Naming::Word)
    } else {
        None
    }
}

/// What Return should switch to rather than search for or open, as with
/// Arc's "⌘T, then the tab's or space's name": the other tab or space that
/// typed text names best (`naming`). A tab is named by its title or the
/// start of its host ("mail" for mail.google.com, not "google"), and only
/// by four characters or more: two or three start too many names ("do",
/// "git", "new"), so they name a space only exactly. Spaces win ties; two
/// spaces, or two pages, named equally well are no clear choice, and the
/// text is searched. An address names the tab at it (`named_address`).
fn named(query: &str, context: &CommandContext) -> Option<CommandItem> {
    let query = normalized(query);
    let length = query.chars().count();
    // A single letter is too little to go on; "?" asks for a search.
    if length < 2 || model::forced_search(&query).is_some() {
        return None;
    }
    let tabs_named = length >= MIN_TAB_NAME;
    let active = context.active.as_ref().map(|active| active.id);
    let tabs: Vec<&TabEntry> = context
        .tabs
        .iter()
        .filter(|tab| tabs_named && Some(tab.id) != active)
        .collect();
    if !model::is_search_query(&query) {
        return named_address(&query, &tabs).map(|tab| switch_item(tab, context));
    }
    let spaces: Vec<(Naming, SpaceId, &str)> = context
        .spaces
        .iter()
        .filter(|(id, _)| *id != context.space_id)
        .filter_map(|(id, name)| {
            let naming = naming(name, &query).filter(|naming| {
                // Short text names a space only exactly ("dev" for "Dev").
                tabs_named || *naming == Naming::Exact
            })?;
            Some((naming, *id, name.as_str()))
        })
        .collect();
    let tabs: Vec<(Naming, &TabEntry)> = tabs
        .into_iter()
        .filter_map(|tab| {
            let host = host_of(&tab.url)
                .and_then(|host| naming(&host, &query))
                .filter(|naming| *naming > Naming::Word);
            Some((naming(&tab.title, &query).max(host)?, tab))
        })
        .collect();
    let best = spaces
        .iter()
        .map(|(naming, ..)| *naming)
        .chain(tabs.iter().map(|(naming, _)| *naming))
        .max()?;
    let spaces: Vec<_> = spaces
        .into_iter()
        .filter(|(naming, ..)| *naming == best)
        .collect();
    match spaces.as_slice() {
        [(_, id, name)] => return Some(space_item(*id, name)),
        [] => {}
        _ => return None,
    }
    let tabs: Vec<&TabEntry> = tabs
        .into_iter()
        .filter(|(naming, _)| *naming == best)
        .map(|(_, tab)| tab)
        .collect();
    one_page(&tabs).map(|tab| switch_item(tab, context))
}

/// The tab an address names: the one at that address, else, for a bare
/// host (with its port, if typed), the only page open on it. A search on
/// Google among other Google pages isn't "google.com".
fn named_address<'a>(query: &str, tabs: &[&'a TabEntry]) -> Option<&'a TabEntry> {
    let typed = url_text(query).trim_end_matches('/');
    let at_address = tabs.iter().find(|tab| {
        url_text(&tab.url)
            .trim_end_matches('/')
            .eq_ignore_ascii_case(typed)
    });
    if let Some(tab) = at_address {
        return Some(tab);
    }
    if typed.contains(['/', '?', '#']) {
        return None;
    }
    let typed_site = site(&format!("http://{typed}"))?;
    let on_site: Vec<&TabEntry> = tabs
        .iter()
        .copied()
        .filter(|tab| site(&tab.url).as_deref() == Some(typed_site.as_str()))
        .collect();
    one_page(&on_site)
}

/// The first of `tabs` if they all show one page (a page open twice is
/// still one choice): the active space's first, as the bar orders them.
fn one_page<'a>(tabs: &[&'a TabEntry]) -> Option<&'a TabEntry> {
    let first = *tabs.first()?;
    let page = page_key(&first.url);
    tabs.iter()
        .all(|tab| page_key(&tab.url) == page)
        .then_some(first)
}

/// The action whose title the text is (any case), or the start of, in two
/// words or more ("new sp", "clear today"), if only one starts so.
fn named_action(query: &str, actions: &[(CommandItem, &'static str)]) -> Option<usize> {
    let query = normalized(query);
    let title = |item: &CommandItem| normalized(item.title.trim_end_matches('…'));
    if let Some(index) = actions.iter().position(|(item, _)| title(item) == query) {
        return Some(index);
    }
    if !query.contains(' ') {
        return None;
    }
    let mut starting = actions
        .iter()
        .enumerate()
        .filter(|(_, (item, _))| title(item).starts_with(&query));
    let (index, _) = starting.next()?;
    starting.next().is_none().then_some(index)
}

/// Where typed text that begins a visited address goes (`completion`).
struct Completion {
    url: String,
    /// The page's title, if it was visited and had one.
    title: Option<String>,
}

/// A visited site or page whose address typed text begins, for Return to
/// go to, as Chrome's address bar completes it. Text that stops inside a
/// host goes to that site ("doc.ru" to doc.rust-lang.org), the one visited
/// most, and most recently, of those it begins; text that goes on into a
/// path goes to that page ("doc.rust-lang.org/b" to the Book). The text
/// must look like an address's start (four characters, or three with a
/// dot), and without a dot only sites visited more than once complete.
fn completion(query: &str, context: &CommandContext) -> Option<Completion> {
    if query.contains(char::is_whitespace) || model::forced_search(query).is_some() {
        return None;
    }
    let lower = query.to_lowercase();
    let typed = url_text(&lower);
    let dotted = typed.contains('.');
    let length = typed.chars().count();
    if length < 3 || (length < 4 && !dotted) {
        return None;
    }
    let best = |a: &(f64, &HistoryEntry), b: &(f64, &HistoryEntry)| {
        a.0.total_cmp(&b.0)
            .then_with(|| b.1.url.len().cmp(&a.1.url.len()))
    };
    if typed.contains('/') {
        let (_, page) = context
            .history
            .iter()
            .filter(|page| dotted || page.count > 1)
            .filter(|page| {
                page.address_lower.starts_with(typed)
                    && page.address_lower.trim_end_matches('/') != typed.trim_end_matches('/')
            })
            .map(|page| (page.frecency(context.now), page))
            .max_by(best)?;
        return Some(Completion {
            url: page.url.clone(),
            title: (!page.title.is_empty()).then(|| page.title.clone()),
        });
    }
    // Sites by their visits all together, each with its best page.
    let mut sites: HashMap<&str, (f64, u32, &HistoryEntry)> = HashMap::new();
    for page in &context.history {
        let address = page.address_lower.as_str();
        let host = &address[..address.find(['/', '?', '#']).unwrap_or(address.len())];
        if typed.len() >= host.len() || !host.starts_with(typed) {
            continue;
        }
        let frecency = page.frecency(context.now);
        let site = sites.entry(host).or_insert((0., 0, page));
        if best(&(frecency, page), &(site.2.frecency(context.now), site.2)).is_gt() {
            site.2 = page;
        }
        site.0 += frecency;
        site.1 = site.1.saturating_add(page.count);
    }
    let (_, _, page) = sites
        .into_values()
        .filter(|(_, count, _)| dotted || *count > 1)
        .max_by(|a, b| best(&(a.0, a.2), &(b.0, b.2)))?;
    let origin = Url::parse(&page.url).ok()?.origin().ascii_serialization();
    let url = format!("{origin}/");
    let root = page_key(&url);
    let title = context
        .history
        .iter()
        .find(|page| page_key(&page.url) == root && !page.title.is_empty())
        .map(|page| page.title.clone());
    Some(Completion { url, title })
}

/// A typed mail, phone or messages address, which opens in its app as a
/// click on such a link does: the address, and where it goes.
fn app_link(query: &str) -> Option<(String, &'static str)> {
    if navigation_for(query) != Navigation::OpenInApp {
        return None;
    }
    let (scheme, rest) = query.split_once(':')?;
    let scheme = scheme.trim().to_ascii_lowercase();
    let hint = match scheme.as_str() {
        "mailto" => "Open in Mail",
        "sms" => "Open in Messages",
        _ => "Call with FaceTime",
    };
    // Phone numbers are often written with spaces; addresses have none.
    let rest: String = if scheme == "mailto" {
        rest.trim().to_owned()
    } else {
        rest.split_whitespace().collect()
    };
    (!rest.is_empty() && !rest.contains(char::is_whitespace))
        .then(|| (format!("{scheme}:{rest}"), hint))
}

fn switch_item(tab: &TabEntry, context: &CommandContext) -> CommandItem {
    let shown = site(&tab.url);
    let (detail, hint) = if tab.space_id != context.space_id {
        // Return changes space, maybe to the same site under another login.
        let detail = match shown {
            Some(site) => format!("{site} · {}", tab.space_name),
            None => tab.space_name.clone(),
        };
        (Some(detail), format!("Switch to {}", tab.space_name).into())
    } else {
        (
            shown.filter(|site| *site != tab.title),
            "Switch to Tab".into(),
        )
    };
    CommandItem {
        command: Command::SwitchToTab(tab.id),
        title: tab.title.clone(),
        detail,
        hint,
        icon: ItemIcon::Page(host_of(&tab.url)),
    }
}

fn space_item(id: SpaceId, name: &str) -> CommandItem {
    CommandItem {
        command: Command::SwitchToSpace(id),
        title: name.to_owned(),
        detail: Some("Space".into()),
        hint: "Switch to Space".into(),
        icon: ItemIcon::Glyph(Icon::ArrowRight),
    }
}

/// A recently closed tab found by typing; it reopens in its own space.
fn closed_item(closed: &ClosedEntry, context: &CommandContext) -> CommandItem {
    let detail = match context.space_name(closed.space_id) {
        Some(space) if closed.space_id != context.space_id => format!("Closed in {space}"),
        _ => "Recently Closed".into(),
    };
    CommandItem {
        command: Command::ReopenTab(closed.id),
        title: closed.title.clone(),
        detail: Some(detail),
        hint: "Reopen Tab".into(),
        icon: ItemIcon::Page(host_of(&closed.url)),
    }
}

/// A visited page; `when` adds how long ago (the History view).
fn page_item(page: &HistoryEntry, command: Command, when: Option<String>) -> CommandItem {
    let title = if page.title.is_empty() {
        url_text(&page.url).to_owned()
    } else {
        page.title.clone()
    };
    // Titles that are the host already say it.
    let address = site(&page.url).filter(|address| *address != title);
    let detail = match (address, when) {
        (Some(address), Some(when)) => Some(format!("{address} · {when}")),
        (address, when) => address.or(when),
    };
    CommandItem {
        command,
        title,
        detail,
        hint: "Open".into(),
        icon: ItemIcon::Page(host_of(&page.url)),
    }
}

fn action(
    command: Command,
    title: impl Into<String>,
    icon: Icon,
    hint: &'static str,
) -> CommandItem {
    CommandItem {
        command,
        title: title.into(),
        detail: None,
        hint: hint.into(),
        icon: ItemIcon::Glyph(icon),
    }
}

/// Actions available now, each with extra words it is found by. Of equal
/// matches, the earlier one is listed first.
fn actions(context: &CommandContext) -> Vec<(CommandItem, &'static str)> {
    let mut actions = Vec::new();
    let other_spaces = || {
        context
            .spaces
            .iter()
            .filter(|(id, _)| *id != context.space_id)
    };
    if let Some(active) = &context.active {
        // ⌘D returns pinned tabs and favorites to Today.
        actions.push(if active.pin == Pin::Today {
            (action(Command::TogglePin, "Pin Tab", Icon::Pin, "⌘D"), "")
        } else {
            (
                action(Command::TogglePin, "Unpin Tab", Icon::PinOff, "⌘D"),
                "",
            )
        });
        actions.push(if active.pin == Pin::Favorite {
            let item = action(
                Command::ToggleFavorite,
                "Remove from Favorites",
                Icon::Star,
                "",
            );
            (item, "unfavorite")
        } else {
            let item = action(Command::ToggleFavorite, "Add to Favorites", Icon::Star, "");
            (item, "favorite")
        });
        actions.push((
            action(Command::DuplicateTab, "Duplicate Tab", Icon::Duplicate, ""),
            "copy clone",
        ));
        actions.push((
            action(
                Command::Shortcut(Shortcut::CloseTab),
                "Close Tab",
                Icon::Close,
                "⌘W",
            ),
            "",
        ));
        for (id, name) in other_spaces() {
            let item = action(
                Command::MoveTab(*id),
                format!("Move Tab to {name}"),
                Icon::ArrowRight,
                "",
            );
            actions.push((item, "send space"));
        }
        actions.push((
            action(Command::CopyUrl, "Copy URL", Icon::Copy, "⇧⌘C"),
            "link address",
        ));
        for (shortcut, title, glyph, hint, keywords) in [
            (Shortcut::Reload, "Reload", Icon::Reload, "⌘R", "refresh"),
            (
                Shortcut::Find,
                "Find on Page",
                Icon::Search,
                "⌘F",
                "search text",
            ),
            (Shortcut::ZoomIn, "Zoom In", Icon::ZoomIn, "⌘+", "larger"),
            (
                Shortcut::ZoomOut,
                "Zoom Out",
                Icon::ZoomOut,
                "⌘−",
                "smaller",
            ),
            (
                Shortcut::ZoomReset,
                "Actual Size",
                Icon::Monitor,
                "⌘0",
                "zoom reset",
            ),
            (Shortcut::Print, "Print…", Icon::Printer, "⌘P", ""),
            (
                Shortcut::DevTools,
                "Developer Tools",
                Icon::Code,
                "⌥⌘I",
                "inspect console devtools",
            ),
        ] {
            actions.push((
                action(Command::Shortcut(shortcut), title, glyph, hint),
                keywords,
            ));
        }
        if let Some(host) = &active.host {
            actions.push((
                blocking_action(host, active.blocking_disabled),
                BLOCKING_WORDS,
            ));
        }
    }
    actions.push((sidebar_action(context), "toggle"));
    if context.has_today {
        actions.push((
            action(Command::ClearToday, "Clear Today Tabs", Icon::Broom, "⇧⌘K"),
            "archive",
        ));
    }
    if !context.closed.is_empty() {
        actions.push((
            action(
                Command::ReopenClosedTab,
                "Reopen Closed Tab",
                Icon::Reopen,
                "⇧⌘T",
            ),
            "undo restore",
        ));
    }
    actions.push((
        action(Command::NewSpace, "New Space", Icon::Plus, ""),
        "create",
    ));
    actions.push((
        action(Command::RenameSpace, "Rename Space", Icon::Pencil, ""),
        "name",
    ));
    actions.push((
        action(Command::UnloadSpace, "Unload Space", Icon::Archive, ""),
        "sleep",
    ));
    for (id, name) in other_spaces() {
        let item = action(
            Command::SwitchToSpace(*id),
            format!("Switch to Space {name}"),
            Icon::ArrowRight,
            "",
        );
        actions.push((item, "go"));
    }
    actions.push((
        action(Command::ShowHistory, "Show History", Icon::History, "⌘Y"),
        "visited pages",
    ));
    actions.push((clear_history_action(), "delete forget"));
    for engine in SearchEngine::ALL {
        if engine != context.search_engine {
            actions.push((search_engine_action(engine, context), "engine default"));
        }
    }
    actions.push((
        action(Command::ShowSettings, "Settings…", Icon::Settings, "⌘,"),
        "preferences",
    ));
    actions
}

const BLOCKING_WORDS: &str = "content ads trackers blocker";

fn blocking_action(host: &str, disabled: bool) -> CommandItem {
    if disabled {
        let title = format!("Turn On Ad Blocking for {host}");
        action(
            Command::ToggleBlocking(host.into()),
            title,
            Icon::Shield,
            "",
        )
    } else {
        let title = format!("Turn Off Ad Blocking for {host}");
        action(
            Command::ToggleBlocking(host.into()),
            title,
            Icon::ShieldOff,
            "",
        )
    }
}

fn sidebar_action(context: &CommandContext) -> CommandItem {
    let title = if context.sidebar_hidden {
        "Show Sidebar"
    } else {
        "Hide Sidebar"
    };
    action(Command::ToggleSidebar, title, Icon::Sidebar, "⌘S")
}

fn clear_history_action() -> CommandItem {
    action(Command::ClearHistory, "Clear History…", Icon::Broom, "")
}

fn search_engine_action(engine: SearchEngine, context: &CommandContext) -> CommandItem {
    let current = engine == context.search_engine;
    CommandItem {
        command: Command::SearchWith(engine),
        title: format!("Search with {}", engine.name()),
        detail: current.then(|| "Current".into()),
        hint: if current { "" } else { "Choose" }.into(),
        icon: ItemIcon::Glyph(if current { Icon::Check } else { Icon::Search }),
    }
}

/// The bar's rows for `query`. Without a query: the active space's other
/// tabs, the most recently used first, and the tab to reopen. With one:
/// opening it (an address or a search), matching tabs from every space,
/// recently closed tabs, visited pages, then matching actions. Before all
/// of them goes what the text names, for Return: a tab, space or action
/// (`named`, `named_action`; not in ⌘L, which loads what is typed), or a
/// visited page whose address it starts (`completion`).
pub fn results(mode: CommandMode, query: &str, context: &CommandContext) -> Vec<CommandItem> {
    let query = query.trim();
    match mode {
        CommandMode::History => return history_results(query, context),
        CommandMode::Settings => return settings_results(query, context),
        CommandMode::NewTab | CommandMode::EditAddress => {}
    }
    if query.is_empty() {
        return empty_results(context);
    }

    let current_url = context.active.as_ref().map(|active| active.url.as_str());
    let active_id = context.active.as_ref().map(|active| active.id);
    let in_place = mode == CommandMode::EditAddress && current_url.is_some();
    let open = |input: &str| {
        if in_place {
            Command::Navigate(input.to_owned())
        } else {
            Command::Open(input.to_owned())
        }
    };
    let mut items = vec![if in_place && current_url == Some(query) {
        CommandItem {
            command: Command::Reload,
            title: query.to_owned(),
            detail: None,
            hint: "Reload".into(),
            icon: ItemIcon::Glyph(Icon::Globe),
        }
    } else if let Some((address, hint)) = app_link(query) {
        CommandItem {
            command: Command::OpenInApp(address.clone()),
            title: address,
            detail: None,
            hint: hint.into(),
            icon: ItemIcon::Glyph(Icon::Link),
        }
    } else if model::is_search_query(query) {
        CommandItem {
            command: open(query),
            title: model::search_text(query).to_owned(),
            detail: Some(context.search_engine.search_label()),
            hint: "Search".into(),
            icon: ItemIcon::Glyph(Icon::Search),
        }
    } else {
        CommandItem {
            command: open(query),
            title: model::normalize_address(query),
            detail: None,
            hint: "Open".into(),
            icon: ItemIcon::Glyph(Icon::Globe),
        }
    }];

    let actions = actions(context);
    let typed_page = || {
        let page = completion(query, context)?;
        // A page open in a tab is switched to rather than opened again, as
        // is the only page open on a site; one just closed in this space
        // comes back where it was.
        let key = page_key(&page.url);
        let host = host_of(&page.url);
        let others: Vec<&TabEntry> = context
            .tabs
            .iter()
            .filter(|tab| !in_place && Some(tab.id) != active_id)
            .collect();
        let whole_site = !key.contains('/');
        let tab = others
            .iter()
            .copied()
            .find(|tab| page_key(&tab.url) == key)
            .or_else(|| {
                let on_site: Vec<&TabEntry> = others
                    .iter()
                    .copied()
                    .filter(|tab| whole_site && site(&tab.url) == site(&page.url))
                    .collect();
                one_page(&on_site)
            });
        let closed = context
            .closed
            .iter()
            .filter(|closed| !in_place && closed.space_id == context.space_id)
            .find(|closed| page_key(&closed.url) == key);
        Some(match (tab, closed) {
            (Some(tab), _) => switch_item(tab, context),
            (None, Some(closed)) => closed_item(closed, context),
            (None, None) => CommandItem {
                command: open(&page.url),
                title: page.url,
                detail: page.title,
                hint: "Open".into(),
                icon: ItemIcon::Page(host),
            },
        })
    };
    // ⌘L edits the tab's own address: Return loads it, completed at most.
    let promoted = if in_place {
        typed_page()
    } else {
        named(query, context)
            .or_else(|| {
                (model::forced_search(query).is_none())
                    .then(|| named_action(query, &actions))
                    .flatten()
                    .map(|index| actions[index].0.clone())
            })
            .or_else(typed_page)
    };
    let promoted_command = promoted.as_ref().map(|item| item.command.clone());
    let is_promoted = |command: &Command| promoted_command.as_ref() == Some(command);
    items.splice(0..0, promoted);

    let words = query_words(model::search_text(query));
    let mut tabs: Vec<(u32, &TabEntry)> = context
        .tabs
        .iter()
        .filter(|tab| !(in_place && Some(tab.id) == active_id))
        .filter(|tab| !is_promoted(&Command::SwitchToTab(tab.id)))
        .filter_map(|tab| {
            let best = score(&tab.title, &words).max(score(url_text(&tab.url), &words))?;
            Some((best, tab))
        })
        .collect();
    tabs.sort_by_key(|(score, _)| Reverse(*score));
    items.extend(
        tabs.into_iter()
            .take(MAX_TABS)
            .map(|(_, tab)| switch_item(tab, context)),
    );

    // Pages open in a tab are switched to, and closed ones reopened, rather
    // than offered again as visited pages.
    let mut shown: Vec<String> = context.tabs.iter().map(|tab| page_key(&tab.url)).collect();
    if let Some(Command::ReopenTab(id)) = &promoted_command {
        let reopened = context.closed.iter().find(|closed| closed.id == *id);
        shown.extend(reopened.map(|closed| page_key(&closed.url)));
    }
    let mut closed: Vec<(u32, &ClosedEntry)> = context
        .closed
        .iter()
        .filter(|closed| !shown.contains(&page_key(&closed.url)))
        .filter(|closed| !is_promoted(&Command::ReopenTab(closed.id)))
        .filter_map(|closed| {
            let best = score(&closed.title, &words).max(score(url_text(&closed.url), &words))?;
            Some((best, closed))
        })
        .collect();
    closed.sort_by_key(|(score, _)| Reverse(*score));
    closed.truncate(MAX_CLOSED);
    shown.extend(closed.iter().map(|(_, closed)| page_key(&closed.url)));
    items.extend(
        closed
            .into_iter()
            .map(|(_, closed)| closed_item(closed, context)),
    );

    let mut pages: Vec<(u32, f64, &HistoryEntry)> = context
        .history
        .iter()
        .filter(|page| !is_promoted(&open(&page.url)))
        .filter_map(|page| Some((page.score(&words)?, page.frecency(context.now), page)))
        .filter(|(_, _, page)| !shown.contains(&page_key(&page.url)))
        .collect();
    pages.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.total_cmp(&a.1)));
    items.extend(
        pages
            .into_iter()
            .take(MAX_PAGES)
            .map(|(_, _, page)| page_item(page, open(&page.url), None)),
    );

    let mut actions: Vec<(u32, CommandItem)> = actions
        .into_iter()
        .filter(|(item, _)| !is_promoted(&item.command))
        .filter_map(|(item, keywords)| {
            Some((score(&format!("{} {keywords}", item.title), &words)?, item))
        })
        .collect();
    actions.sort_by_key(|(score, _)| Reverse(*score));
    items.extend(actions.into_iter().take(MAX_ACTIONS).map(|(_, item)| item));
    items
}

/// The empty bar: the space's other tabs, the most recently used first,
/// then in sidebar order (the tab on screen is no place to go), and the
/// tab ⇧⌘T would reopen, by name.
fn empty_results(context: &CommandContext) -> Vec<CommandItem> {
    let active = context.active.as_ref().map(|active| active.id);
    let mut tabs: Vec<&TabEntry> = context
        .tabs
        .iter()
        .filter(|tab| tab.space_id == context.space_id && Some(tab.id) != active)
        .collect();
    tabs.sort_by_key(|tab| {
        context
            .recent
            .iter()
            .position(|id| *id == tab.id)
            .unwrap_or(usize::MAX)
    });
    let mut items: Vec<CommandItem> = tabs
        .into_iter()
        .take(MAX_TABS)
        .map(|tab| switch_item(tab, context))
        .collect();
    if let Some(closed) = context.closed.first() {
        let detail = match context.space_name(closed.space_id) {
            Some(space) if closed.space_id != context.space_id => Some(space.to_owned()),
            _ => site(&closed.url),
        };
        items.push(CommandItem {
            command: Command::ReopenClosedTab,
            title: format!("Reopen {}", closed.title),
            detail,
            hint: "⇧⌘T".into(),
            icon: ItemIcon::Glyph(Icon::Reopen),
        });
    }
    items
}

/// The History view: visited pages, the most recent first, or the best
/// matches for the query. Clearing the history ends the full list.
fn history_results(query: &str, context: &CommandContext) -> Vec<CommandItem> {
    let words = query_words(query);
    let mut pages: Vec<(u32, &HistoryEntry)> = context
        .history
        .iter()
        .filter_map(|page| Some((page.score(&words)?, page)))
        .collect();
    // Stable: the most recent first among equal matches.
    pages.sort_by_key(|(score, _)| Reverse(*score));
    let mut items: Vec<CommandItem> = pages
        .into_iter()
        .take(MAX_HISTORY)
        .map(|(_, page)| {
            let when = ago(context.now, page.last);
            page_item(page, Command::Open(page.url.clone()), Some(when))
        })
        .collect();
    if words.is_empty() && !items.is_empty() {
        items.push(clear_history_action());
    }
    items
}

/// The settings (⌘,): the search engine, then switches kept with the
/// session, filtered by the query.
fn settings_results(query: &str, context: &CommandContext) -> Vec<CommandItem> {
    let mut settings: Vec<(CommandItem, &'static str)> = SearchEngine::ALL
        .into_iter()
        .map(|engine| (search_engine_action(engine, context), "engine default"))
        .collect();
    if let Some(active) = &context.active
        && let Some(host) = &active.host
    {
        let item = blocking_action(host, active.blocking_disabled);
        settings.push((item, BLOCKING_WORDS));
    }
    settings.push((sidebar_action(context), "toggle"));
    settings.push((clear_history_action(), "delete forget"));
    let words = query_words(query);
    let mut settings: Vec<(u32, CommandItem)> = settings
        .into_iter()
        .filter_map(|(item, keywords)| {
            Some((score(&format!("{} {keywords}", item.title), &words)?, item))
        })
        .collect();
    settings.sort_by_key(|(score, _)| Reverse(*score));
    settings.into_iter().map(|(_, item)| item).collect()
}

/// Rows the list shows before it scrolls, so that the panel, 20% down the
/// card, ends above the card's bottom edge.
pub fn visible_rows(card: Bounds<Pixels>) -> usize {
    let room = f32::from(card.size.height) * 0.8
        - CARD_GAP
        - (INPUT_HEIGHT + 1. + LIST_PADDING_TOP + LIST_PADDING_BOTTOM);
    ((room / ROW_HEIGHT).floor().max(1.) as usize).min(MAX_VISIBLE_ROWS)
}

/// The panel's height with a list `rows` rows tall (none: just the field).
fn panel_height(rows: f32) -> f32 {
    if rows <= 0. {
        INPUT_HEIGHT
    } else {
        INPUT_HEIGHT + 1. + LIST_PADDING_TOP + rows * ROW_HEIGHT + LIST_PADDING_BOTTOM
    }
}

/// Screen bounds of the bar's window: centred over the card (in window
/// coordinates), 20% down from its top, for a window at `origin`.
pub fn window_bounds(card: Bounds<Pixels>, origin: Point<Pixels>) -> Bounds<Pixels> {
    let card_width = f32::from(card.size.width);
    let width = (card_width - 80.).clamp(MIN_WIDTH.min(card_width), MAX_WIDTH);
    let left = f32::from(card.origin.x) + (card_width - width) / 2.;
    let top = f32::from(card.origin.y) + f32::from(card.size.height) * 0.2;
    // Transparent windows keep their size; others fit the panel as it grows.
    let height = panel_height(if TRANSPARENT {
        visible_rows(card) as f32
    } else {
        0.
    });
    Bounds::new(
        origin + point(px(left - MARGIN), px(top - MARGIN)),
        size(px(width + 2. * MARGIN), px(height + MARGIN + MARGIN_BOTTOM)),
    )
}

pub fn window_options(bounds: Bounds<Pixels>, display_id: Option<DisplayId>) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: None,
        focus: true,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        display_id,
        window_background: if TRANSPARENT {
            WindowBackgroundAppearance::Transparent
        } else {
            WindowBackgroundAppearance::Opaque
        },
        ..Default::default()
    }
}

pub enum CommandBarEvent {
    Run(Command),
    /// Closed without a result. `refocus` is false when another window or
    /// application took focus.
    Dismiss {
        refocus: bool,
    },
}

/// What the bar's field starts with.
pub enum BarInput {
    /// Text, selected, so that typing replaces it.
    Text(String),
    /// The field of the bar this one replaces, with what was typed, the
    /// selection and what ⌘Z can undo.
    Field(Entity<TextInput>),
}

pub struct CommandBar {
    mode: CommandMode,
    input: Entity<TextInput>,
    context: CommandContext,
    items: Vec<CommandItem>,
    /// The highlighted row. Without text nothing is, so a Return out of
    /// habit does nothing; ↓ or Tab picks the first row.
    selected: Option<usize>,
    theme: Theme,
    icons: HashMap<String, Arc<Image>>,
    /// Rows shown before the list scrolls (`visible_rows`).
    rows: usize,
    scroll: ScrollHandle,
    was_active: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CommandBarEvent> for CommandBar {}

impl CommandBar {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mode: CommandMode,
        input: BarInput,
        context: CommandContext,
        theme: Theme,
        icons: HashMap<String, Arc<Image>>,
        rows: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = match input {
            BarInput::Field(input) => {
                input.read(cx).focus(window);
                input
            }
            BarInput::Text(text) => cx.new(|cx| {
                let mut input = TextInput::new(window, cx);
                input.set_placeholder(
                    match mode {
                        CommandMode::NewTab | CommandMode::EditAddress => "Search or Enter URL…",
                        CommandMode::History => "Search History…",
                        CommandMode::Settings => "Search Settings…",
                    },
                    cx,
                );
                input.set_style(
                    InputStyle {
                        text: theme.title(),
                        placeholder: theme.tertiary(),
                        cursor: theme.title(),
                        selection: theme.focus().opacity(0.3),
                        font_size: px(18.),
                        line_height: px(24.),
                    },
                    cx,
                );
                input.set_text_and_select_all(&text, window, cx);
                input
            }),
        };
        let subscriptions = vec![
            cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
                InputEvent::Changed(query) => {
                    this.update_results(query, window);
                    cx.notify();
                }
                InputEvent::Submit(_) => this.run_selected(cx),
                InputEvent::Cancel => cx.emit(CommandBarEvent::Dismiss { refocus: true }),
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.was_active = true;
                } else if this.was_active {
                    cx.emit(CommandBarEvent::Dismiss { refocus: false });
                }
            }),
        ];
        let query = input.read(cx).text().to_owned();
        let mut this = Self {
            mode,
            input,
            context,
            items: Vec::new(),
            selected: None,
            theme,
            icons,
            rows,
            scroll: ScrollHandle::new(),
            was_active: window.is_window_active(),
            _subscriptions: subscriptions,
        };
        this.update_results(&query, window);
        this
    }

    /// The field, for a bar that replaces this one.
    pub fn field(&self) -> Entity<TextInput> {
        self.input.clone()
    }

    /// The results again, for what changed since the bar opened (a
    /// shortcut pinned the tab, cleared Today), keeping the highlight.
    pub fn set_context(
        &mut self,
        context: CommandContext,
        icons: HashMap<String, Arc<Image>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.context = context;
        self.icons.extend(icons);
        let selected = self.selected;
        let query = self.input.read(cx).text().to_owned();
        self.update_results(&query, window);
        if selected.is_some() {
            self.selected = selected.filter(|&index| index < self.items.len());
        }
        cx.notify();
    }

    /// Favicons read from the disk cache after the bar opened.
    pub fn add_icons(&mut self, icons: HashMap<String, Arc<Image>>, cx: &mut Context<Self>) {
        if !icons.is_empty() {
            self.icons.extend(icons);
            cx.notify();
        }
    }

    /// ⌘L again: the whole address selected once more, as in Chrome.
    pub fn select_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.select_all_focus(window, cx));
    }

    fn update_results(&mut self, query: &str, window: &mut Window) {
        self.items = results(self.mode, query, &self.context);
        self.selected = (!query.trim().is_empty() && !self.items.is_empty()).then_some(0);
        self.scroll.set_offset(point(px(0.), px(0.)));
        if !TRANSPARENT {
            let height = px(panel_height(self.list_rows()));
            let viewport = window.viewport_size();
            if viewport.height != height {
                window.resize(size(viewport.width, height));
            }
        }
    }

    /// What the History view says when it lists nothing.
    fn note(&self) -> Option<&'static str> {
        (self.mode == CommandMode::History && self.items.is_empty()).then_some(
            if self.context.history.is_empty() {
                "Pages you visit appear here"
            } else {
                "No matching pages"
            },
        )
    }

    /// The list's height in rows: every result, or, when they don't fit,
    /// half a row less than there is room for, so that the row cut in half
    /// shows there are more.
    fn list_rows(&self) -> f32 {
        let count = self.items.len();
        if count > self.rows {
            self.rows as f32 - 0.5
        } else if count == 0 && self.note().is_some() {
            1.
        } else {
            count as f32
        }
    }

    fn run(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(item) = self.items.get(index) {
            cx.emit(CommandBarEvent::Run(item.command.clone()));
        }
    }

    fn run_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(index) = self.selected {
            self.run(index, cx);
        }
    }

    /// ⌘Return: an address or search goes to a new tab, also from ⌘L,
    /// whose Return loads it in the tab on screen.
    fn run_in_new_tab(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.selected.and_then(|index| self.items.get(index)) else {
            return;
        };
        let command = match &item.command {
            Command::Navigate(input) => Command::Open(input.clone()),
            Command::Reload => match &self.context.active {
                Some(active) => Command::Open(active.url.clone()),
                None => return,
            },
            command => command.clone(),
        };
        cx.emit(CommandBarEvent::Run(command));
    }

    fn move_selection(&mut self, step: isize, cx: &mut Context<Self>) {
        let count = self.items.len();
        if count == 0 {
            return;
        }
        let next = match self.selected {
            Some(index) => (index as isize + step).rem_euclid(count as isize) as usize,
            None if step > 0 => 0,
            None => count - 1,
        };
        self.selected = Some(next);
        self.scroll.scroll_to_item(next);
        cx.notify();
    }

    fn render_icon(&self, item_icon: &ItemIcon, selected: bool) -> impl IntoElement {
        let theme = &self.theme;
        let tile = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(24.))
            .rounded(px(5.));
        match item_icon {
            ItemIcon::Page(host) => match host.as_ref().and_then(|host| self.icons.get(host)) {
                // Favicons are made for light pages: a white tile keeps
                // dark ones visible on dark themes.
                Some(image) => tile
                    .bg(gpui::white())
                    .border_1()
                    .border_color(gpui::black().opacity(0.08))
                    .child(img(image.clone()).size(px(16.))),
                // The sidebar's letter tile, in the bar's own colours.
                None => tile.child(placeholder_favicon(host.as_deref(), theme)),
            },
            ItemIcon::Glyph(glyph) => tile.child(icon(
                *glyph,
                16.,
                if selected {
                    gpui::white()
                } else {
                    theme.ink(0.5)
                },
            )),
        }
    }

    /// A row; its detail is at most `detail_width` wide, so a long one (a
    /// page's title) leaves room for the title.
    fn render_item(
        &self,
        index: usize,
        item: &CommandItem,
        detail_width: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = &self.theme;
        let selected = self.selected == Some(index);
        div()
            .id(("item", index))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(ROW_HEIGHT))
            .mx(px(8.))
            .px(px(13.))
            .rounded(px(7.))
            .cursor_pointer()
            .when(selected, |row| row.bg(theme.focus()))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered && this.selected != Some(index) {
                    this.selected = Some(index);
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| this.run(index, cx)))
            .child(self.render_icon(&item.icon, selected))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .text_size(px(13.))
                    .child(
                        truncated(item.title.clone())
                            .flex_shrink()
                            .min_w_0()
                            .text_color(if selected {
                                gpui::white()
                            } else {
                                theme.title()
                            })
                            .when(selected, |title| title.font_weight(FontWeight::SEMIBOLD)),
                    )
                    .when_some(item.detail.clone(), |row, detail| {
                        row.child(
                            truncated(format!(" — {detail}"))
                                .flex_none()
                                .max_w(detail_width)
                                .text_color(if selected {
                                    gpui::white().opacity(0.72)
                                } else {
                                    theme.ink(0.45)
                                }),
                        )
                    }),
            )
            .when(selected && !item.hint.is_empty(), |row| {
                row.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(8.))
                        .text_size(px(12.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(gpui::white().opacity(0.9))
                        .child(item.hint.clone())
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .h(px(18.))
                                .px(px(5.))
                                .rounded(px(4.))
                                .bg(gpui::white().opacity(0.25))
                                .child("↩"),
                        ),
                )
            })
    }
}

impl Render for CommandBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let width = window.viewport_size().width - px(2. * MARGIN);
        let items = self
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                self.render_item(index, item, width * 0.4, cx)
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let list_height = LIST_PADDING_TOP + self.list_rows() * ROW_HEIGHT + LIST_PADDING_BOTTOM;
        let note = self.note();
        let glyph = match self.mode {
            CommandMode::NewTab | CommandMode::EditAddress => Icon::Search,
            CommandMode::History => Icon::History,
            CommandMode::Settings => Icon::Settings,
        };
        let panel = div()
            .id("command-bar")
            .absolute()
            .left(px(MARGIN))
            .top(px(MARGIN))
            .w(width)
            .flex()
            .flex_col()
            .rounded(px(10.))
            .bg(theme.surface())
            .when(TRANSPARENT, |panel| {
                panel.shadow(vec![shadow(16., 48., 0.2), shadow(0., 1., 0.12)])
            })
            // A hairline keeps the panel apart from a page of the same tone.
            .border_1()
            .border_color(theme.ink(0.12))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(17.))
                    .h(px(INPUT_HEIGHT))
                    .pl(px(22.))
                    .pr(px(22.))
                    .child(icon(glyph, 16., theme.ink(0.55)))
                    .child(self.input.clone()),
            )
            .when(!items.is_empty(), |panel| {
                // The padding stays put, so a row scrolled into view keeps
                // its distance from the panel's edge.
                panel.child(div().h(px(1.)).bg(theme.ink(0.08))).child(
                    div()
                        .pt(px(LIST_PADDING_TOP))
                        .pb(px(LIST_PADDING_BOTTOM))
                        .child(
                            div()
                                .id("results")
                                .track_scroll(&self.scroll)
                                .overflow_y_scroll()
                                .h(px(self.list_rows() * ROW_HEIGHT))
                                .flex()
                                .flex_col()
                                .children(items),
                        ),
                )
            })
            .when_some(note, |panel, note| {
                panel.child(div().h(px(1.)).bg(theme.ink(0.08))).child(
                    div()
                        .flex()
                        .items_center()
                        .h(px(list_height))
                        .px(px(29.))
                        .text_size(px(13.))
                        .text_color(theme.tertiary())
                        .child(note),
                )
            });
        div()
            .size_full()
            .relative()
            .font_family(UI_FONT)
            .key_context("CommandBar")
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &SubmitInNewTab, _, cx| this.run_in_new_tab(cx)))
            .when(!TRANSPARENT, |root| root.bg(theme.card()))
            // A press beside the panel, in the transparent margin, dismisses.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.emit(CommandBarEvent::Dismiss { refocus: true })),
            )
            .child(panel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: TabId, space_id: SpaceId, title: &str, url: &str) -> TabEntry {
        TabEntry {
            id,
            space_id,
            space_name: if space_id == 1 { "Personal" } else { "Work" }.into(),
            title: title.into(),
            url: url.into(),
        }
    }

    fn context() -> CommandContext {
        CommandContext {
            space_id: 1,
            tabs: vec![
                tab(10, 1, "Hacker News", "https://news.ycombinator.com/"),
                tab(
                    11,
                    1,
                    "Arc (web browser) - Wikipedia",
                    "https://en.wikipedia.org/wiki/Arc",
                ),
                tab(
                    12,
                    1,
                    "The Rust Programming Language",
                    "https://www.reddit.com/r/rust/",
                ),
                tab(20, 2, "GitHub", "https://github.com/"),
                tab(21, 2, "gpui - Rust", "https://docs.rs/gpui/latest/gpui/"),
            ],
            recent: Vec::new(),
            active: Some(ActiveTab {
                id: 11,
                url: "https://en.wikipedia.org/wiki/Arc".into(),
                pin: Pin::Today,
                host: Some("en.wikipedia.org".into()),
                blocking_disabled: false,
            }),
            spaces: vec![(1, "Personal".into()), (2, "Work".into())],
            closed: vec![ClosedEntry {
                id: 5,
                space_id: 1,
                title: "Lobsters".into(),
                url: "https://lobste.rs/".into(),
            }],
            history: Vec::new(),
            now: 0,
            sidebar_hidden: false,
            has_today: true,
            search_engine: SearchEngine::Google,
        }
    }

    fn commands(items: &[CommandItem]) -> Vec<Command> {
        items.iter().map(|item| item.command.clone()).collect()
    }

    #[test]
    fn scores_prefixes_over_word_starts_over_substrings() {
        let words = |query: &str| -> Vec<String> {
            query.split_whitespace().map(str::to_lowercase).collect()
        };
        assert_eq!(score("GitHub", &words("git")), Some(3));
        assert_eq!(score("The Rust Book", &words("rust")), Some(2));
        assert_eq!(score("Trusty", &words("rust")), Some(1));
        assert_eq!(score("Rust Book", &words("rust book")), Some(5));
        assert_eq!(score("Rust Book", &words("rust python")), None);
        assert_eq!(url_text("https://www.example.com/a"), "example.com/a");
    }

    #[test]
    fn empty_query_lists_the_spaces_other_tabs_and_the_tab_to_reopen() {
        let items = results(CommandMode::NewTab, "  ", &context());
        assert_eq!(
            commands(&items),
            [
                Command::SwitchToTab(10),
                Command::SwitchToTab(12),
                Command::ReopenClosedTab,
            ]
        );
        assert_eq!(items[0].detail.as_deref(), Some("news.ycombinator.com"));
        assert_eq!(items[0].hint, "Switch to Tab");
    }

    #[test]
    fn typed_text_opens_an_address_or_searches_first() {
        let context = context();
        let url = results(CommandMode::NewTab, "example.com", &context);
        assert_eq!(url[0].command, Command::Open("example.com".into()));
        assert_eq!(url[0].title, "https://example.com");
        assert_eq!(url[0].hint, "Open");
        let search = results(CommandMode::NewTab, "rust gpui", &context);
        assert_eq!(search[0].command, Command::Open("rust gpui".into()));
        assert_eq!(search[0].hint, "Search");
        // ⌘L loads in the active tab instead, and the unchanged URL reloads.
        let edit = results(CommandMode::EditAddress, "example.com", &context);
        assert_eq!(edit[0].command, Command::Navigate("example.com".into()));
        let same = results(
            CommandMode::EditAddress,
            "https://en.wikipedia.org/wiki/Arc",
            &context,
        );
        assert_eq!(same[0].command, Command::Reload);
    }

    #[test]
    fn return_switches_to_the_tab_or_space_the_text_names() {
        let mut context = context();
        context.tabs.push(tab(
            13,
            1,
            "Inbox (3) - me@gmail.com - Gmail",
            "https://mail.google.com/mail/u/0/#inbox",
        ));
        let top = |query: &str| -> Vec<Command> {
            let items = results(CommandMode::NewTab, query, &context);
            commands(&items[..items.len().min(2)])
        };
        let search = |query: &str| Command::Open(query.into());
        // The start of a title, a whole word of one, the start of a host.
        assert_eq!(top("hacker"), [Command::SwitchToTab(10), search("hacker")]);
        assert_eq!(top("Hacker  News")[0], Command::SwitchToTab(10));
        assert_eq!(top("gmail"), [Command::SwitchToTab(13), search("gmail")]);
        assert_eq!(top("mail")[0], Command::SwitchToTab(13));
        assert_eq!(top("news")[0], Command::SwitchToTab(10));
        assert_eq!(top("the rust")[0], Command::SwitchToTab(12));
        // Another space by its name.
        assert_eq!(top("work"), [Command::SwitchToSpace(2), search("work")]);
        // Searches: phrases inside titles, parts of words inside them,
        // single letters, the tab on screen, the active space, and actions'
        // words.
        for query in [
            "rust lang",
            "rust gpui",
            "ews",
            "google",
            "h",
            "arc",
            "wikipedia",
            "personal",
            "go",
            "space",
            "switch",
            "create",
        ] {
            assert_eq!(top(query)[0], search(query), "{query}");
        }
        // Addresses open, unless an open tab is at that address or, for a
        // bare host, on that host.
        assert_eq!(
            top("github.com"),
            [Command::SwitchToTab(20), search("github.com")]
        );
        assert_eq!(top("https://www.GitHub.com/")[0], Command::SwitchToTab(20));
        assert_eq!(top("docs.rs/gpui/latest/gpui")[0], Command::SwitchToTab(21));
        for query in ["github.com/rust-lang", "example.com", "en.wikipedia.org"] {
            assert_eq!(top(query)[0], search(query), "{query}");
        }
        // ⌘L loads what is typed in the tab.
        assert_eq!(
            results(CommandMode::EditAddress, "hacker", &context)[0].command,
            Command::Navigate("hacker".into())
        );
        // The row isn't repeated below, and says what Return does.
        let work = results(CommandMode::NewTab, "work", &context);
        assert_eq!(work[0].hint, "Switch to Space");
        let rows = |items: &[CommandItem], command: Command| {
            items.iter().filter(|item| item.command == command).count()
        };
        assert_eq!(rows(&work, Command::SwitchToSpace(2)), 1);
        let gmail = results(CommandMode::NewTab, "gmail", &context);
        assert_eq!(gmail[0].hint, "Switch to Tab");
        assert_eq!(rows(&gmail, Command::SwitchToTab(13)), 1);
    }

    #[test]
    fn tabs_match_across_spaces_best_first() {
        let items = results(CommandMode::NewTab, "rust", &context());
        let tabs: Vec<Command> = commands(&items)
            .into_iter()
            .filter(|command| matches!(command, Command::SwitchToTab(_)))
            .collect();
        // "Rust" starts a word in both titles; ties keep the sidebar order.
        assert_eq!(tabs, [Command::SwitchToTab(12), Command::SwitchToTab(21)]);
        let other_space = items
            .iter()
            .find(|item| item.command == Command::SwitchToTab(21))
            .unwrap();
        assert_eq!(other_space.detail.as_deref(), Some("docs.rs · Work"));
        // ⌘L never offers the tab being edited.
        let edit = results(CommandMode::EditAddress, "wikipedia", &context());
        assert!(!commands(&edit).contains(&Command::SwitchToTab(11)));
        assert!(
            commands(&results(CommandMode::NewTab, "wikipedia", &context()))
                .contains(&Command::SwitchToTab(11))
        );
    }

    #[test]
    fn actions_match_by_name_and_keywords() {
        let context = context();
        let first_action = |query: &str| {
            results(CommandMode::NewTab, query, &context)
                .into_iter()
                .find(|item| matches!(item.icon, ItemIcon::Glyph(icon) if icon != Icon::Search && icon != Icon::Globe))
                .map(|item| item.command)
        };
        assert_eq!(first_action("pin"), Some(Command::TogglePin));
        assert_eq!(first_action("sidebar"), Some(Command::ToggleSidebar));
        assert_eq!(first_action("clear"), Some(Command::ClearToday));
        assert_eq!(first_action("undo"), Some(Command::ReopenClosedTab));
        assert_eq!(first_action("work"), Some(Command::SwitchToSpace(2)));
        assert_eq!(
            first_action("trackers"),
            Some(Command::ToggleBlocking("en.wikipedia.org".into()))
        );
        assert_eq!(first_action("personal"), None);
    }

    #[test]
    fn pinned_tabs_and_favorites_offer_unpinning() {
        for (pin, title) in [
            (Pin::Today, "Pin Tab"),
            (Pin::Pinned, "Unpin Tab"),
            (Pin::Favorite, "Unpin Tab"),
        ] {
            let mut context = context();
            context.active.as_mut().unwrap().pin = pin;
            let item = results(CommandMode::NewTab, "pin", &context)
                .into_iter()
                .find(|item| item.command == Command::TogglePin)
                .unwrap();
            assert_eq!(item.title, title, "{pin:?}");
        }
    }

    #[test]
    fn every_matching_tab_is_listed_up_to_the_cap() {
        let mut context = context();
        context.tabs = (0..60)
            .map(|id| tab(100 + id, 1, "Page", &format!("https://page{id}.test/")))
            .collect();
        let tabs = |query: &str| {
            results(CommandMode::NewTab, query, &context)
                .iter()
                .filter(|item| matches!(item.command, Command::SwitchToTab(_)))
                .count()
        };
        assert_eq!(tabs(""), MAX_TABS);
        assert_eq!(tabs("page"), MAX_TABS);
    }

    #[test]
    fn the_window_sits_over_the_card() {
        let card = Bounds::new(point(px(228.), px(10.)), size(px(1202.), px(880.)));
        let bounds = window_bounds(card, point(px(100.), px(50.)));
        let panel_left = f32::from(bounds.origin.x) + MARGIN;
        let panel_width = f32::from(bounds.size.width) - 2. * MARGIN;
        assert_eq!(panel_width, MAX_WIDTH);
        assert_eq!(panel_left, 100. + 228. + (1202. - MAX_WIDTH) / 2.);
        assert_eq!(f32::from(bounds.origin.y) + MARGIN, 50. + 10. + 880. * 0.2);
        // Narrow cards keep 40 points on each side.
        let narrow = Bounds::new(point(px(10.), px(10.)), size(px(600.), px(500.)));
        let bounds = window_bounds(narrow, point(px(0.), px(0.)));
        assert_eq!(f32::from(bounds.size.width) - 2. * MARGIN, 520.);
    }
}
