//! Arc's command bar (⌘T, ⌘L). It opens in its own pop-up window over the
//! page card, because the native web views would cover anything GPUI draws
//! in the browser window. A new tab exists only once a result is picked.

use super::{
    UI_FONT,
    icons::{Icon, icon},
    placeholder_favicon, shadow,
    theme::Theme,
    truncated,
};
use crate::{
    favicons::host_of,
    input::{InputEvent, InputStyle, TextInput},
    model::{self, Pin, SpaceId, TabId},
};
use gpui::{
    App, AppContext, Bounds, Context, DisplayId, Entity, EventEmitter, FontWeight, Image,
    InteractiveElement, IntoElement, KeyBinding, MouseButton, ParentElement, Pixels, Point, Render,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, actions, div, img, point,
    prelude::FluentBuilder, px, size,
};
use std::{collections::HashMap, sync::Arc};

actions!(command_bar, [SelectPrevious, SelectNext]);

pub const MAX_ITEMS: usize = 8;
const MAX_TABS: usize = 5;
const MAX_ACTIONS: usize = 3;
const MAX_WIDTH: f32 = 760.;
const MIN_WIDTH: f32 = 320.;
const INPUT_HEIGHT: f32 = 68.;
const ROW_HEIGHT: f32 = 48.;
const LIST_PADDING_TOP: f32 = 7.;
const LIST_PADDING_BOTTOM: f32 = 8.;
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
    ]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandMode {
    /// ⌘T: Enter opens a new tab.
    NewTab,
    /// ⌘L: the active tab's address, selected; Enter loads it in that tab.
    EditAddress,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// A new tab for an address or search.
    Open(String),
    /// An address or search in the active tab.
    Navigate(String),
    Reload,
    SwitchToTab(TabId),
    SwitchToSpace(SpaceId),
    TogglePin,
    ToggleFavorite,
    ToggleSidebar,
    CopyUrl,
    ClearToday,
    NewSpace,
    ReopenClosedTab,
    /// Content blocking on the host, off or back on.
    ToggleBlocking(String),
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
    /// Shown at the right of the highlighted row.
    pub hint: &'static str,
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

/// What the results draw from, captured when the bar opens.
pub struct CommandContext {
    pub space_id: SpaceId,
    /// Every tab: the active space's first, each space in sidebar order.
    pub tabs: Vec<TabEntry>,
    pub active: Option<ActiveTab>,
    pub spaces: Vec<(SpaceId, String)>,
    pub can_reopen: bool,
    pub sidebar_hidden: bool,
    pub has_today: bool,
}

/// How well `text` matches every query word: 3 for a word at the start,
/// 2 at the start of one of its words, 1 anywhere; `None` if one is missing.
fn score(text: &str, words: &[String]) -> Option<u32> {
    let text = text.to_lowercase();
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

/// URLs match without their scheme and "www.".
fn url_text(url: &str) -> &str {
    let url = url.split_once("://").map_or(url, |(_, rest)| rest);
    url.strip_prefix("www.").unwrap_or(url)
}

fn switch_item(tab: &TabEntry, context: &CommandContext) -> CommandItem {
    let host = host_of(&tab.url);
    CommandItem {
        command: Command::SwitchToTab(tab.id),
        title: tab.title.clone(),
        detail: if tab.space_id != context.space_id {
            Some(tab.space_name.clone())
        } else {
            host.clone().filter(|host| *host != tab.title)
        },
        hint: "Switch to Tab",
        icon: ItemIcon::Page(host),
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
        hint,
        icon: ItemIcon::Glyph(icon),
    }
}

/// Actions available now, each with extra words it is found by.
fn actions(context: &CommandContext) -> Vec<(CommandItem, &'static str)> {
    let mut actions = Vec::new();
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
            action(Command::CopyUrl, "Copy URL", Icon::Copy, "⇧⌘C"),
            "link address",
        ));
        if let Some(host) = &active.host {
            let (title, icon) = if active.blocking_disabled {
                (format!("Turn On Content Blocking for {host}"), Icon::Shield)
            } else {
                (
                    format!("Turn Off Content Blocking for {host}"),
                    Icon::ShieldOff,
                )
            };
            actions.push((
                action(Command::ToggleBlocking(host.clone()), title, icon, ""),
                "ads trackers blocker",
            ));
        }
    }
    let sidebar = if context.sidebar_hidden {
        "Show Sidebar"
    } else {
        "Hide Sidebar"
    };
    actions.push((
        action(Command::ToggleSidebar, sidebar, Icon::Sidebar, "⌘S"),
        "toggle",
    ));
    if context.has_today {
        actions.push((
            action(Command::ClearToday, "Clear Today Tabs", Icon::Broom, "⇧⌘K"),
            "close archive",
        ));
    }
    actions.push((
        action(Command::NewSpace, "New Space", Icon::Plus, ""),
        "create",
    ));
    if context.can_reopen {
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
    for (id, name) in &context.spaces {
        if *id != context.space_id {
            let item = action(
                Command::SwitchToSpace(*id),
                format!("Switch to Space {name}"),
                Icon::ArrowRight,
                "",
            );
            actions.push((item, "go"));
        }
    }
    actions
}

/// The bar's rows for `query`. Without a query: the active space's tabs,
/// the selected one first. With one: opening it (an address or a search),
/// matching tabs from every space, then matching actions.
pub fn results(mode: CommandMode, query: &str, context: &CommandContext) -> Vec<CommandItem> {
    let query = query.trim();
    let active_id = context.active.as_ref().map(|active| active.id);
    if query.is_empty() {
        let mut tabs: Vec<&TabEntry> = context
            .tabs
            .iter()
            .filter(|tab| tab.space_id == context.space_id)
            .collect();
        tabs.sort_by_key(|tab| Some(tab.id) != active_id);
        let room = MAX_ITEMS - usize::from(context.can_reopen);
        let mut items: Vec<CommandItem> = tabs
            .into_iter()
            .take(room)
            .map(|tab| switch_item(tab, context))
            .collect();
        if context.can_reopen {
            items.push(action(
                Command::ReopenClosedTab,
                "Reopen Closed Tab",
                Icon::Reopen,
                "⇧⌘T",
            ));
        }
        return items;
    }

    let current_url = context.active.as_ref().map(|active| active.url.as_str());
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
            hint: "Reload",
            icon: ItemIcon::Glyph(Icon::Globe),
        }
    } else if model::is_search_query(query) {
        CommandItem {
            command: open(query),
            title: query.to_owned(),
            detail: Some("Google Search".into()),
            hint: "Search",
            icon: ItemIcon::Glyph(Icon::Search),
        }
    } else {
        CommandItem {
            command: open(query),
            title: model::normalize_address(query),
            detail: None,
            hint: "Open",
            icon: ItemIcon::Glyph(Icon::Globe),
        }
    }];

    let words: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .map(String::from)
        .collect();
    let mut tabs: Vec<(u32, &TabEntry)> = context
        .tabs
        .iter()
        .filter(|tab| !(in_place && Some(tab.id) == active_id))
        .filter_map(|tab| {
            let best = score(&tab.title, &words).max(score(url_text(&tab.url), &words))?;
            Some((best, tab))
        })
        .collect();
    tabs.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    items.extend(
        tabs.into_iter()
            .take(MAX_TABS)
            .map(|(_, tab)| switch_item(tab, context)),
    );

    let mut actions: Vec<(u32, CommandItem)> = actions(context)
        .into_iter()
        .filter_map(|(item, keywords)| {
            Some((score(&format!("{} {keywords}", item.title), &words)?, item))
        })
        .collect();
    actions.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    items.extend(actions.into_iter().take(MAX_ACTIONS).map(|(_, item)| item));
    items.truncate(MAX_ITEMS);
    items
}

fn panel_height(items: usize) -> f32 {
    if items == 0 {
        INPUT_HEIGHT
    } else {
        INPUT_HEIGHT + 1. + LIST_PADDING_TOP + items as f32 * ROW_HEIGHT + LIST_PADDING_BOTTOM
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
    let height = panel_height(if TRANSPARENT { MAX_ITEMS } else { 0 });
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

pub struct CommandBar {
    mode: CommandMode,
    input: Entity<TextInput>,
    context: CommandContext,
    items: Vec<CommandItem>,
    selected: usize,
    theme: Theme,
    icons: HashMap<String, Arc<Image>>,
    was_active: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CommandBarEvent> for CommandBar {}

impl CommandBar {
    pub fn new(
        mode: CommandMode,
        prefill: &str,
        context: CommandContext,
        theme: Theme,
        icons: HashMap<String, Arc<Image>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            let mut input = TextInput::new(window, cx);
            input.set_placeholder("Search or Enter URL…", cx);
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
            input.set_text_and_select_all(prefill, window, cx);
            input
        });
        let subscriptions = vec![
            cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
                InputEvent::Changed(query) => {
                    this.update_results(query, window);
                    cx.notify();
                }
                InputEvent::Submit(_) => this.run(this.selected, cx),
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
        let mut this = Self {
            mode,
            input,
            context,
            items: Vec::new(),
            selected: 0,
            theme,
            icons,
            was_active: window.is_window_active(),
            _subscriptions: subscriptions,
        };
        this.update_results(prefill, window);
        this
    }

    fn update_results(&mut self, query: &str, window: &mut Window) {
        self.items = results(self.mode, query, &self.context);
        self.selected = 0;
        if !TRANSPARENT {
            let height = px(panel_height(self.items.len()));
            let viewport = window.viewport_size();
            if viewport.height != height {
                window.resize(size(viewport.width, height));
            }
        }
    }

    fn run(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(item) = self.items.get(index) {
            cx.emit(CommandBarEvent::Run(item.command.clone()));
        }
    }

    fn move_selection(&mut self, step: isize, cx: &mut Context<Self>) {
        let count = self.items.len();
        if count > 0 {
            self.selected = (self.selected as isize + step).rem_euclid(count as isize) as usize;
            cx.notify();
        }
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
            ItemIcon::Page(host) => {
                let favicon = host.as_ref().and_then(|host| self.icons.get(host)).cloned();
                tile.bg(gpui::white())
                    .border_1()
                    .border_color(gpui::black().opacity(0.08))
                    .child(match favicon {
                        Some(image) => img(image).size(px(16.)).into_any_element(),
                        None => placeholder_favicon(host.as_deref(), theme).into_any_element(),
                    })
            }
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

    fn render_item(
        &self,
        index: usize,
        item: &CommandItem,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = &self.theme;
        let selected = index == self.selected;
        div()
            .id(("item", index))
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(ROW_HEIGHT))
            .mx(px(8.))
            .px(px(13.))
            .rounded(px(7.))
            .cursor_pointer()
            .when(selected, |row| row.bg(theme.focus()))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered && this.selected != index {
                    this.selected = index;
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
                            div()
                                .flex_none()
                                .whitespace_nowrap()
                                .text_color(if selected {
                                    gpui::white().opacity(0.72)
                                } else {
                                    theme.ink(0.45)
                                })
                                .child(SharedString::from(format!(" — {detail}"))),
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
                        .child(item.hint)
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
            .map(|(index, item)| self.render_item(index, item, cx).into_any_element())
            .collect::<Vec<_>>();
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
            .when(!TRANSPARENT, |panel| {
                panel.border_1().border_color(theme.ink(0.12))
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(17.))
                    .h(px(INPUT_HEIGHT))
                    .pl(px(22.))
                    .pr(px(22.))
                    .child(icon(Icon::Search, 16., theme.ink(0.55)))
                    .child(self.input.clone()),
            )
            .when(!items.is_empty(), |panel| {
                panel.child(div().h(px(1.)).bg(theme.ink(0.08))).child(
                    div()
                        .flex()
                        .flex_col()
                        .pt(px(LIST_PADDING_TOP))
                        .pb(px(LIST_PADDING_BOTTOM))
                        .children(items),
                )
            });
        div()
            .size_full()
            .relative()
            .font_family(UI_FONT)
            .key_context("CommandBar")
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
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
            active: Some(ActiveTab {
                id: 11,
                url: "https://en.wikipedia.org/wiki/Arc".into(),
                pin: Pin::Today,
                host: Some("en.wikipedia.org".into()),
                blocking_disabled: false,
            }),
            spaces: vec![(1, "Personal".into()), (2, "Work".into())],
            can_reopen: true,
            sidebar_hidden: false,
            has_today: true,
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
    fn empty_query_lists_the_space_with_the_selected_tab_first() {
        let items = results(CommandMode::NewTab, "  ", &context());
        assert_eq!(
            commands(&items),
            [
                Command::SwitchToTab(11),
                Command::SwitchToTab(10),
                Command::SwitchToTab(12),
                Command::ReopenClosedTab,
            ]
        );
        assert_eq!(items[0].detail.as_deref(), Some("en.wikipedia.org"));
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
        assert_eq!(other_space.detail.as_deref(), Some("Work"));
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
    fn results_are_capped() {
        let mut context = context();
        context.tabs = (0..20)
            .map(|id| tab(100 + id, 1, "Page", &format!("https://page{id}.test/")))
            .collect();
        assert_eq!(results(CommandMode::NewTab, "", &context).len(), MAX_ITEMS);
        let typed = results(CommandMode::NewTab, "page", &context);
        assert_eq!(typed.len(), 1 + MAX_TABS);
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
