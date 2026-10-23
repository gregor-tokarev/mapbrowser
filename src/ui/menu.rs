//! Popover menus for tabs, spaces, the library and "+". They are drawn in
//! GPUI and kept inside the sidebar, where no native web view covers them.
//! Like AppKit's menus they work from the keyboard too: ↑ and ↓ move a
//! highlight, → and ← enter and leave "Move to Space", Return chooses, a
//! letter jumps to the next item starting with it, and Esc closes.

use super::{
    UI_FONT,
    downloads::DownloadState,
    icons::{Icon, icon},
    press_only, shadow,
    sidebar::{space_icons, space_shortcut},
    theme::Theme,
    truncated, wrapped_lines,
};
use crate::{
    app::BrowserApp,
    model::{Pin, SpaceId, THEME_COUNT, TabId},
};
use gpui::{
    AnyElement, App, Bounds, Context, Div, Font, FontWeight, InteractiveElement, IntoElement,
    KeyBinding, KeyContext, KeyDownEvent, MouseButton, ParentElement, Pixels, Point, ScrollHandle,
    SharedString, Size, Stateful, StatefulInteractiveElement, Styled, Window, actions, deferred,
    div, font, point, prelude::FluentBuilder, px, relative,
};

actions!(
    menu,
    [
        SelectPrevious,
        SelectNext,
        SelectFirst,
        SelectLast,
        SelectLeft,
        SelectRight,
        NextControl,
        PreviousControl,
        Choose,
        ChooseDestructive
    ]
);

/// While a menu is open the browser window's root has the "Menu" context
/// as well as "Browser", whose Esc closes it.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SelectPrevious, Some("Menu")),
        KeyBinding::new("down", SelectNext, Some("Menu")),
        KeyBinding::new("home", SelectFirst, Some("Menu")),
        KeyBinding::new("end", SelectLast, Some("Menu")),
        KeyBinding::new("pageup", SelectFirst, Some("Menu")),
        KeyBinding::new("pagedown", SelectLast, Some("Menu")),
        KeyBinding::new("left", SelectLeft, Some("Menu")),
        KeyBinding::new("right", SelectRight, Some("Menu")),
        KeyBinding::new("tab", NextControl, Some("Menu")),
        KeyBinding::new("shift-tab", PreviousControl, Some("Menu")),
        KeyBinding::new("enter", Choose, Some("Menu")),
        KeyBinding::new("space", Choose, Some("Menu")),
        // Return never deletes: in the delete prompt it means Cancel, as
        // destructive buttons are never the default on macOS.
        KeyBinding::new("cmd-backspace", ChooseDestructive, Some("Menu")),
    ]);
}

const PADDING: f32 = 5.;
const ITEM_HEIGHT: f32 = 24.;
const SEPARATOR_HEIGHT: f32 = 11.;
const HEADER_HEIGHT: f32 = 22.;
const SWATCH_ROW_HEIGHT: f32 = 30.;
const ICON_ROW_HEIGHT: f32 = 28.;
const ICONS_PER_ROW: usize = 7;
/// A question: a title, which for a long space name wraps onto as many as
/// `PROMPT_TITLE_LINES` lines, and its explanation. Its padding above,
/// between and below them, and their sizes.
const PROMPT_SPACING: f32 = 4. + 2. + 8.;
const PROMPT_PADDING: f32 = 9.;
const PROMPT_TITLE_LINE: f32 = 18.;
const PROMPT_TITLE_LINES: usize = 4;
const PROMPT_TITLE_SIZE: f32 = 13.;
const PROMPT_DETAIL_LINE: f32 = 16.;
const PROMPT_DETAIL_SIZE: f32 = 12.;
const SPACE_NAME_HEIGHT: f32 = 28.;
const CONFIRM_HEIGHT: f32 = 32.;
const CLOSED_LIMIT: usize = 10;
/// Distance the menu keeps from the sidebar's edges.
const EDGE: f32 = 6.;
/// Distance between a tab's row and its menu.
const ROW_GAP: f32 = 3.;
/// Menus asked for this close to the window's bottom edge belong to the
/// bottom bar, and move with it when the window's height changes.
const BOTTOM_BAR_REACH: f32 = 64.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuKind {
    Tab(TabId),
    Space(SpaceId),
    /// Every space, from the "…" ending a switcher too crowded for them all.
    Spaces,
    /// The bottom bar's "+".
    Plus,
    /// This session's downloads and the recently closed tabs, from the
    /// library button.
    Library,
    /// Asks before deleting a space, in place of the space menu.
    DeleteSpace(SpaceId),
    /// The spaces a tab can move to, in place of the tab menu.
    MoveTab(TabId),
}

pub struct OpenMenu {
    pub kind: MenuKind,
    /// Where it was asked for, in window coordinates. Menus open below it,
    /// or, when there is no room, above `above`.
    pub anchor: Point<Pixels>,
    pub above: Pixels,
    /// For menus of the bottom bar, the window's height when they opened.
    bottom_bar: Option<Pixels>,
    /// The entry the pointer or the keyboard is on; without one, the menu's
    /// default (`default_highlight`).
    highlight: Option<Highlight>,
    /// Menus taller than the window scroll.
    scroll: ScrollHandle,
}

impl OpenMenu {
    pub fn new(kind: MenuKind, anchor: Point<Pixels>, window_height: Pixels) -> Self {
        Self {
            kind,
            anchor,
            above: anchor.y,
            bottom_bar: (anchor.y > window_height - px(BOTTOM_BAR_REACH)).then_some(window_height),
            highlight: None,
            scroll: ScrollHandle::new(),
        }
    }

    /// Turns the menu into another in place ("Delete Space…", "Move to
    /// Space ›"), with nothing highlighted and scrolled to its top.
    fn become_menu(&mut self, kind: MenuKind) {
        self.kind = kind;
        self.highlight = None;
        self.scroll = ScrollHandle::new();
    }

    /// Where the menu was asked for, in a window now `window_height` tall:
    /// the bottom bar's menus keep their distance from its bottom edge.
    fn anchors(&self, window_height: Pixels) -> (Point<Pixels>, Pixels) {
        let shift = self
            .bottom_bar
            .map_or(px(0.), |opened| window_height - opened);
        (self.anchor + point(px(0.), shift), self.above + shift)
    }
}

/// A highlighted entry, and in a row of choices (theme swatches, icons, the
/// delete prompt's buttons) which one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Highlight {
    entry: usize,
    part: usize,
}

/// A question highlights its Cancel button, so Return can't delete by
/// accident; other menus start with nothing highlighted, as AppKit's do.
fn default_highlight(entries: &[Entry]) -> Option<Highlight> {
    entries
        .iter()
        .position(|entry| matches!(entry, Entry::Confirm { .. }))
        .map(|entry| Highlight { entry, part: 0 })
}

/// A key moving the highlight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Previous,
    Next,
    First,
    Last,
    Left,
    Right,
    /// Tab and ⇧Tab: every choice in turn, those in rows too.
    NextControl,
    PreviousControl,
}

#[derive(Clone, Copy, Debug)]
enum Action {
    TogglePin(TabId),
    ToggleFavorite(TabId),
    RenameTab(TabId),
    ResetTabName(TabId),
    GoToPinnedPage(TabId),
    CopyUrl(TabId),
    Duplicate(TabId),
    Reload(TabId),
    Close(TabId),
    Delete(TabId),
    CloseOtherTabs(TabId),
    CloseTabsBelow(TabId),
    Rename(SpaceId),
    Theme(SpaceId, u8),
    SpaceIcon(SpaceId, Option<&'static str>),
    SwitchSpace(SpaceId),
    NewSpace,
    UnloadSpace(SpaceId),
    NewTab,
    Reopen(usize),
    /// "Delete Space…": the menu asks first.
    AskDeleteSpace(SpaceId),
    DeleteSpace(SpaceId),
    RevealDownload(u64),
    /// "Move to Space ›": the menu lists the spaces.
    AskMoveTab(TabId),
    MoveTab(TabId, SpaceId),
    /// A question's Cancel: the menu closes.
    Cancel,
}

enum Entry {
    Item {
        label: SharedString,
        shortcut: &'static str,
        action: Action,
        enabled: bool,
    },
    Separator,
    Header(&'static str),
    /// Which space a space menu is for: its icon or dot, and its name.
    SpaceName {
        name: SharedString,
        icon: Option<String>,
        theme: u8,
    },
    Swatches(SpaceId, u8),
    /// The icon choices for a space, and its theme, whose dot is the
    /// choice of no icon.
    Icons(SpaceId, Option<String>, u8),
    Closed {
        index: usize,
        title: SharedString,
    },
    Empty(&'static str),
    /// A question; its title and its explanation wrap onto `lines` lines
    /// each (`fit_prompts`).
    Prompt {
        title: SharedString,
        detail: &'static str,
        lines: (usize, usize),
    },
    /// Cancel, and a red button that runs `action`.
    Confirm {
        label: &'static str,
        action: Action,
    },
    /// A download; finished and running ones show in Finder when clicked.
    Download {
        id: u64,
        name: SharedString,
        status: Option<String>,
        failed: bool,
    },
    /// A space, with its icon or its theme's dot; `current` marks the
    /// active one.
    Space {
        name: SharedString,
        icon: Option<String>,
        theme: u8,
        shortcut: &'static str,
        current: bool,
        action: Action,
    },
    /// A line of explanation.
    Note(&'static str),
}

impl Entry {
    fn item(label: impl Into<SharedString>, shortcut: &'static str, action: Action) -> Self {
        Entry::Item {
            label: label.into(),
            shortcut,
            action,
            enabled: true,
        }
    }

    /// Greyed out and inert unless `enabled`.
    fn enabled(mut self, enabled: bool) -> Self {
        if let Entry::Item { enabled: item, .. } = &mut self {
            *item = enabled;
        }
        self
    }

    fn height(&self) -> f32 {
        match self {
            Entry::Item { .. }
            | Entry::Closed { .. }
            | Entry::Empty(_)
            | Entry::Download { .. }
            | Entry::Space { .. }
            | Entry::Note(_) => ITEM_HEIGHT,
            Entry::Separator => SEPARATOR_HEIGHT,
            Entry::Header(_) => HEADER_HEIGHT,
            Entry::SpaceName { .. } => SPACE_NAME_HEIGHT,
            Entry::Swatches(..) => SWATCH_ROW_HEIGHT,
            Entry::Icons(..) => {
                ICON_ROW_HEIGHT * (space_icons().len() + 1).div_ceil(ICONS_PER_ROW) as f32
            }
            Entry::Prompt {
                lines: (title, detail),
                ..
            } => {
                PROMPT_SPACING
                    + *title as f32 * PROMPT_TITLE_LINE
                    + *detail as f32 * PROMPT_DETAIL_LINE
            }
            Entry::Confirm { .. } => CONFIRM_HEIGHT,
        }
    }

    /// How many choices the entry holds: one for an item, one per swatch or
    /// icon, two for Cancel and the red button, none for text.
    fn parts(&self) -> usize {
        match self {
            Entry::Item { enabled, .. } => usize::from(*enabled),
            Entry::Closed { .. } | Entry::Space { .. } => 1,
            Entry::Download { failed, .. } => usize::from(!failed),
            Entry::Swatches(..) => usize::from(THEME_COUNT),
            Entry::Icons(..) => space_icons().len() + 1,
            Entry::Confirm { .. } => 2,
            Entry::Separator
            | Entry::Header(_)
            | Entry::SpaceName { .. }
            | Entry::Empty(_)
            | Entry::Prompt { .. }
            | Entry::Note(_) => 0,
        }
    }

    /// What choosing `part` of the entry does.
    fn action(&self, part: usize) -> Option<Action> {
        match self {
            Entry::Item {
                action,
                enabled: true,
                ..
            }
            | Entry::Space { action, .. } => Some(*action),
            Entry::Closed { index, .. } => Some(Action::Reopen(*index)),
            Entry::Download {
                id, failed: false, ..
            } => Some(Action::RevealDownload(*id)),
            Entry::Swatches(space, _) => u8::try_from(part)
                .ok()
                .filter(|&theme| theme < THEME_COUNT)
                .map(|theme| Action::Theme(*space, theme)),
            Entry::Icons(space, ..) => match part {
                0 => Some(Action::SpaceIcon(*space, None)),
                _ => Some(Action::SpaceIcon(
                    *space,
                    Some(*space_icons().get(part - 1)?),
                )),
            },
            Entry::Confirm { action, .. } => Some(if part == 0 { Action::Cancel } else { *action }),
            _ => None,
        }
    }

    /// The part a row of choices starts on: the current theme or icon.
    fn current_part(&self) -> usize {
        match self {
            Entry::Swatches(_, current) => usize::from(*current),
            Entry::Icons(_, Some(current), _) => space_icons()
                .iter()
                .position(|icon| icon == current)
                .map_or(0, |index| index + 1),
            _ => 0,
        }
    }

    /// The text a typed letter is matched against.
    fn label(&self) -> Option<&str> {
        match self {
            Entry::Item {
                label,
                enabled: true,
                ..
            } => Some(label),
            Entry::Closed { title: label, .. }
            | Entry::Space { name: label, .. }
            | Entry::Download {
                name: label,
                failed: false,
                ..
            } => Some(label),
            _ => None,
        }
    }
}

/// A space's icon, or a dot in its colour as the switcher shows it, in a
/// 16 pt slot.
fn space_mark(icon: Option<String>, theme: u8, dark: bool) -> gpui::Div {
    let dot = Theme::new(theme, dark).focus();
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(16.))
        .map(|slot| match icon {
            Some(emoji) => slot.text_size(px(13.)).child(emoji),
            None => slot.child(div().size(px(7.)).rounded_full().bg(dot)),
        })
}

/// Counts the lines of each question in a menu `width` wide, so the whole
/// name of a space to delete shows, and the menu is placed by its real
/// height.
fn fit_prompts(entries: &mut [Entry], width: f32, cx: &App) {
    let room = width - 2. * PADDING - 2. * PROMPT_PADDING;
    let bold = Font {
        weight: FontWeight::SEMIBOLD,
        ..font(UI_FONT)
    };
    for entry in entries {
        if let Entry::Prompt {
            title,
            detail,
            lines,
        } = entry
        {
            *lines = (
                wrapped_lines(title, bold.clone(), PROMPT_TITLE_SIZE, room, cx)
                    .min(PROMPT_TITLE_LINES),
                wrapped_lines(detail, font(UI_FONT), PROMPT_DETAIL_SIZE, room, cx),
            );
        }
    }
}

/// Top-left corner of a `size` menu opening below `anchor`, or above
/// `above` when there is no room, kept `EDGE` inside the sidebar (`width`
/// wide) and the window (`height` tall), also after the window shrank
/// under an open menu. Menus are never taller than the window allows.
fn place(
    anchor: Point<Pixels>,
    above: Pixels,
    menu: Size<f32>,
    width: f32,
    height: f32,
) -> Point<Pixels> {
    let (x, y) = (f32::from(anchor.x), f32::from(anchor.y));
    let left = x.min(width - EDGE - menu.width).max(EDGE);
    let top = if y + menu.height <= height - EDGE {
        y
    } else {
        f32::from(above) - menu.height
    };
    point(px(left), px(top.min(height - EDGE - menu.height).max(EDGE)))
}

/// The tallest a menu may be in a window `height` tall; taller ones scroll.
fn max_height(height: f32) -> f32 {
    (height - 2. * EDGE).max(ITEM_HEIGHT + 2. * PADDING)
}

impl BrowserApp {
    /// A tab's menu opens just below its row or tile, level with the
    /// pointer, or above it without room there: never over its title.
    pub fn open_tab_menu(
        &mut self,
        tab_id: TabId,
        pointer: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let row = self.tab_bounds(tab_id);
        self.open_menu(MenuKind::Tab(tab_id), pointer, window, cx);
        if let (Some(menu), Some(row)) = (&mut self.menu, row) {
            menu.anchor.y = row.bottom() + px(ROW_GAP);
            menu.above = row.top() - px(ROW_GAP);
            // Rows don't move with the window's bottom edge.
            menu.bottom_bar = None;
        }
    }

    /// The space menu from the title row opens below the row, like a tab's
    /// menu below its own, leaving the name in sight.
    pub fn open_space_title_menu(
        &mut self,
        space_id: SpaceId,
        pointer: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = self.drop_zones.borrow().title;
        self.open_menu(MenuKind::Space(space_id), pointer, window, cx);
        if let Some(menu) = &mut self.menu
            && !title.is_empty()
        {
            menu.anchor.y = title.bottom() + px(ROW_GAP);
            menu.above = title.top() - px(ROW_GAP);
            // The title doesn't move with the window's bottom edge.
            menu.bottom_bar = None;
        }
    }

    /// Where the active space's tab was last drawn.
    fn tab_bounds(&self, tab_id: TabId) -> Option<Bounds<Pixels>> {
        let space = self.state.active_space();
        let pin = space.tabs.iter().find(|tab| tab.id == tab_id)?.pin;
        let index = space.section(pin).position(|tab| tab.id == tab_id)?;
        self.drop_zones.borrow().tab(pin, index)
    }

    fn menu_entries(&self, kind: MenuKind) -> Vec<Entry> {
        match kind {
            MenuKind::Tab(id) => {
                let Some(tab) = self.state.tab(id) else {
                    return Vec::new();
                };
                // ⌘D pins Today tabs and returns pinned tabs and favorites
                // to Today; a favorite can also step down to Pinned.
                let mut entries = match tab.pin {
                    Pin::Today => vec![
                        Entry::item("Pin Tab", "⌘D", Action::TogglePin(id)),
                        Entry::item("Add to Favorites", "", Action::ToggleFavorite(id)),
                    ],
                    Pin::Pinned => vec![
                        Entry::item("Unpin Tab", "⌘D", Action::TogglePin(id)),
                        Entry::item("Add to Favorites", "", Action::ToggleFavorite(id)),
                    ],
                    Pin::Favorite => vec![
                        Entry::item("Remove from Favorites", "⌘D", Action::TogglePin(id)),
                        Entry::item("Move to Pinned", "", Action::ToggleFavorite(id)),
                    ],
                };
                entries.push(Entry::Separator);
                // Favorite tiles show no title to rename.
                if tab.pin != Pin::Favorite {
                    entries.push(Entry::item("Rename Tab…", "", Action::RenameTab(id)));
                    if tab.custom_title.is_some() {
                        entries.push(Entry::item("Reset Name", "", Action::ResetTabName(id)));
                    }
                }
                if tab.pin != Pin::Today {
                    let away = tab.pinned_url.as_ref().is_some_and(|url| *url != tab.url);
                    entries.push(
                        Entry::item("Go to Pinned Page", "", Action::GoToPinnedPage(id))
                            .enabled(away),
                    );
                }
                entries.extend([
                    Entry::item("Copy URL", "⇧⌘C", Action::CopyUrl(id)),
                    Entry::item("Duplicate", "", Action::Duplicate(id)),
                ]);
                if self.state.spaces.len() > 1 {
                    entries.push(Entry::item("Move to Space", "›", Action::AskMoveTab(id)));
                }
                entries.extend([
                    Entry::item("Reload", "⌘R", Action::Reload(id)),
                    Entry::Separator,
                ]);
                if tab.pin == Pin::Today {
                    let today: Vec<TabId> = self
                        .state
                        .space_of_tab(id)
                        .and_then(|space| self.state.space(space))
                        .map(|space| space.section(Pin::Today).map(|tab| tab.id).collect())
                        .unwrap_or_default();
                    let below = today
                        .iter()
                        .position(|&other| other == id)
                        .is_some_and(|at| at + 1 < today.len());
                    entries.extend([
                        Entry::item("Close Tab", "⌘W", Action::Close(id)),
                        Entry::item("Close Other Tabs", "", Action::CloseOtherTabs(id))
                            .enabled(today.len() > 1),
                        Entry::item("Close Tabs Below", "", Action::CloseTabsBelow(id))
                            .enabled(below),
                    ]);
                } else {
                    // ⌘W only puts a pinned tab or favorite to sleep; Close
                    // Tab removes it, and ⇧⌘T brings it back to its place.
                    entries.extend([
                        Entry::item("Unload Tab", "⌘W", Action::Close(id))
                            .enabled(self.is_loaded(id)),
                        Entry::item("Close Tab", "", Action::Delete(id)),
                    ]);
                }
                entries
            }
            MenuKind::Space(id) => {
                let Some(space) = self.state.space(id) else {
                    return Vec::new();
                };
                // Names are edited in the title row, which only the active
                // space has on screen. Another space's menu, from its small
                // dot, says which space it is for.
                let mut entries = vec![if id == self.state.selected_space_id {
                    Entry::item("Rename Space", "", Action::Rename(id))
                } else {
                    Entry::SpaceName {
                        name: space.name.clone().into(),
                        icon: space.icon.clone(),
                        theme: space.theme(),
                    }
                }];
                entries.extend([
                    Entry::Separator,
                    Entry::Header("Theme"),
                    Entry::Swatches(id, space.theme()),
                    Entry::Header("Icon"),
                    Entry::Icons(id, space.icon.clone(), space.theme()),
                    Entry::Separator,
                    Entry::item("New Space", "", Action::NewSpace),
                    Entry::item("Unload Space", "", Action::UnloadSpace(id)),
                    Entry::Separator,
                    Entry::item("Delete Space…", "", Action::AskDeleteSpace(id))
                        .enabled(self.state.spaces.len() > 1),
                ]);
                entries
            }
            MenuKind::Spaces => {
                let selected = self.state.selected_space_id;
                let spaces =
                    self.state
                        .spaces
                        .iter()
                        .enumerate()
                        .map(|(index, space)| Entry::Space {
                            name: space.name.clone().into(),
                            icon: space.icon.clone(),
                            theme: space.theme(),
                            shortcut: space_shortcut(index),
                            current: space.id == selected,
                            action: Action::SwitchSpace(space.id),
                        });
                std::iter::once(Entry::Header("Spaces"))
                    .chain(spaces)
                    .chain([
                        Entry::Separator,
                        Entry::item("New Space", "", Action::NewSpace),
                    ])
                    .collect()
            }
            MenuKind::Plus => vec![
                Entry::item("New Tab", "⌘T", Action::NewTab),
                Entry::item("New Space", "", Action::NewSpace),
            ],
            MenuKind::Library => {
                let mut entries = Vec::new();
                let downloads = self.downloads.recent().map(|download| Entry::Download {
                    id: download.id,
                    name: download.name.clone(),
                    status: download.status(),
                    failed: download.state == DownloadState::Failed,
                });
                entries.extend(downloads);
                if !entries.is_empty() {
                    entries.insert(0, Entry::Header("Downloads"));
                    entries.push(Entry::Separator);
                }
                entries.push(Entry::Header("Recently Closed"));
                let closed = self.state.closed_tabs();
                entries.extend(closed.iter().enumerate().rev().take(CLOSED_LIMIT).map(
                    |(index, closed)| Entry::Closed {
                        index,
                        title: closed.tab.shown_title().to_owned().into(),
                    },
                ));
                if closed.is_empty() {
                    entries.push(Entry::Empty("No recently closed tabs"));
                }
                entries
            }
            MenuKind::DeleteSpace(id) => {
                let Some(space) = self.state.space(id) else {
                    return Vec::new();
                };
                vec![
                    Entry::Prompt {
                        title: format!("Delete {}?", space.name).into(),
                        detail: if space.tabs.is_empty() {
                            "Its site data will be removed."
                        } else {
                            "Its tabs and site data will be removed."
                        },
                        lines: (1, 2),
                    },
                    Entry::Confirm {
                        label: "Delete",
                        action: Action::DeleteSpace(id),
                    },
                ]
            }
            MenuKind::MoveTab(id) => {
                let Some(from) = self.state.space_of_tab(id) else {
                    return Vec::new();
                };
                let spaces = self.state.spaces.iter().filter(|space| space.id != from);
                std::iter::once(Entry::Header("Move to Space"))
                    .chain(spaces.map(|space| Entry::Space {
                        name: space.name.clone().into(),
                        icon: space.icon.clone(),
                        theme: space.theme(),
                        shortcut: "",
                        current: false,
                        action: Action::MoveTab(id, space.id),
                    }))
                    // Each space is a profile of its own.
                    .chain([
                        Entry::Separator,
                        Entry::Note("Opens with that space's logins"),
                    ])
                    .collect()
            }
        }
    }

    pub(super) fn render_menu(
        &self,
        menu: &OpenMenu,
        theme: &Theme,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sidebar = self.sidebar_width().unwrap_or(f32::from(viewport.width));
        let width: f32 = match menu.kind {
            MenuKind::Tab(_) | MenuKind::Plus | MenuKind::MoveTab(_) => 200.,
            MenuKind::Space(_)
            | MenuKind::Spaces
            | MenuKind::Library
            | MenuKind::DeleteSpace(_) => 212.,
        };
        let width = width.min(sidebar - 2. * EDGE);
        let mut entries = self.menu_entries(menu.kind);
        fit_prompts(&mut entries, width, cx);
        let content = entries.iter().map(Entry::height).sum::<f32>();
        let height = (content + 2. * PADDING).min(max_height(f32::from(viewport.height)));
        let (anchor, above) = menu.anchors(viewport.height);
        let origin = place(
            anchor,
            above,
            Size { width, height },
            sidebar,
            f32::from(viewport.height),
        );
        let highlight = menu.highlight.or_else(|| default_highlight(&entries));
        let list = div()
            .id("menu-list")
            .flex()
            .flex_col()
            .h(px(height - 2. * PADDING))
            .overflow_y_scroll()
            .track_scroll(&menu.scroll)
            .children(entries.into_iter().enumerate().map(|(index, entry)| {
                let lit = highlight
                    .filter(|highlight| highlight.entry == index)
                    .map(|highlight| highlight.part);
                // Kept at their height, so a list taller than the menu scrolls.
                div()
                    .flex_none()
                    .child(self.render_entry(index, entry, lit, theme, cx))
            }));
        let panel = div()
            .id("menu")
            .absolute()
            .left(origin.x)
            .top(origin.y)
            .w(px(width))
            .p(px(PADDING))
            .rounded(px(8.))
            .bg(theme.surface())
            .border_1()
            .border_color(theme.ink(if theme.dark { 0.14 } else { 0.1 }))
            .shadow(vec![shadow(6., 24., 0.18)])
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(list);
        // Behind the menu: a press anywhere else closes it.
        deferred(
            div()
                .id("menu-backdrop")
                .absolute()
                .size_full()
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.close_menu(cx)),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|this, _, _, cx| this.close_menu(cx)),
                )
                .child(panel),
        )
        .with_priority(1)
        .into_any_element()
    }

    /// Draws `entry`, the `index`th of the menu; `lit` is its highlighted
    /// part, if it has the highlight.
    fn render_entry(
        &self,
        index: usize,
        entry: Entry,
        lit: Option<usize>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let height = entry.height();
        let focus = theme.focus();
        let target = |part: usize| Highlight { entry: index, part };
        let item = |id: usize, enabled: bool| {
            let lit = enabled && lit.is_some();
            div()
                .id(("menu-item", id))
                .flex()
                .items_center()
                .h(px(ITEM_HEIGHT))
                .px(px(9.))
                .rounded(px(5.))
                .text_size(px(13.))
                .text_color(if !enabled {
                    theme.disabled()
                } else if lit {
                    gpui::white()
                } else {
                    theme.title()
                })
                .when(lit, |item| item.bg(focus))
        };
        // Secondary text in a row turns white with the row's highlight.
        let secondary = if lit.is_some() {
            gpui::white()
        } else {
            theme.tertiary()
        };
        match entry {
            Entry::Item {
                label,
                shortcut,
                action,
                enabled,
            } => item(index, enabled)
                .child(div().flex_1().child(label))
                .when(!shortcut.is_empty(), |row| {
                    row.child(div().text_color(secondary).child(shortcut))
                })
                .when(enabled, |row| {
                    highlights(row, target(0), cx).on_click(cx.listener(
                        move |this, _, window, cx| this.run_menu_action(action, window, cx),
                    ))
                })
                .into_any_element(),
            Entry::Closed {
                index: closed,
                title,
            } => {
                let tab = &self.state.closed_tabs()[closed].tab;
                let row = item(1000 + closed, true)
                    .gap(px(8.))
                    .child(self.favicon(tab, 14., theme))
                    .child(truncated(title).flex_1().min_w_0());
                highlights(row, target(0), cx)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run_menu_action(Action::Reopen(closed), window, cx)
                    }))
                    .into_any_element()
            }
            Entry::Download {
                id,
                name,
                status,
                failed,
            } => {
                let muted = if failed { theme.tertiary() } else { secondary };
                // A failed download has nothing to show in Finder.
                item(2000 + index, !failed)
                    .gap(px(8.))
                    .when(!failed, |row| {
                        highlights(row, target(0), cx).on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.run_menu_action(Action::RevealDownload(id), window, cx)
                            },
                        ))
                    })
                    .child(icon(Icon::File, 14., muted))
                    .child(truncated(name).flex_1().min_w_0())
                    .children(status.map(|status| {
                        div()
                            .flex_none()
                            .text_size(px(11.5))
                            .text_color(muted)
                            .child(status)
                    }))
                    .into_any_element()
            }
            Entry::Separator => div()
                .h(px(SEPARATOR_HEIGHT))
                .flex()
                .items_center()
                .px(px(4.))
                .child(div().flex_1().h(px(1.)).bg(theme.ink(0.08)))
                .into_any_element(),
            Entry::Header(text) => div()
                .h(px(HEADER_HEIGHT))
                .flex()
                .items_end()
                .pb(px(3.))
                .px(px(9.))
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.tertiary())
                .child(text)
                .into_any_element(),
            Entry::SpaceName {
                name,
                icon: space_icon,
                theme: space_theme,
            } => div()
                .h(px(SPACE_NAME_HEIGHT))
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(9.))
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.title())
                .child(space_mark(space_icon, space_theme, theme.dark))
                .child(truncated(name).flex_1().min_w_0())
                .into_any_element(),
            Entry::Empty(text) => div()
                .h(px(ITEM_HEIGHT))
                .flex()
                .items_center()
                .px(px(9.))
                .text_size(px(13.))
                .text_color(theme.tertiary())
                .child(text)
                .into_any_element(),
            Entry::Swatches(space, current) => div()
                .h(px(SWATCH_ROW_HEIGHT))
                .flex()
                .items_center()
                .justify_between()
                .px(px(4.))
                .children((0..THEME_COUNT).map(|swatch| {
                    let part = usize::from(swatch);
                    let colours = Theme::new(swatch, theme.dark);
                    // The highlight is a soft square behind the swatch, as
                    // behind the icons below.
                    let cell = div()
                        .id(("swatch", part))
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(24.))
                        .rounded(px(6.))
                        .when(lit == Some(part), |cell| cell.bg(theme.control_hover()))
                        .cursor_pointer()
                        .child(
                            div()
                                .size(px(18.))
                                .rounded_full()
                                .bg(colours.swatch())
                                .border_2()
                                .border_color(if swatch == current {
                                    theme.title()
                                } else {
                                    theme.ink(0.08)
                                }),
                        );
                    highlights(press_only(cell), target(part), cx).on_click(cx.listener(
                        move |this, _, window, cx| {
                            this.run_menu_action(Action::Theme(space, swatch), window, cx)
                        },
                    ))
                }))
                .into_any_element(),
            Entry::Icons(space, current, space_theme) => {
                // No icon shows the space's own dot, whichever space's
                // colours the menu has.
                let dot = Theme::new(space_theme, theme.dark).focus();
                let choices: Vec<_> = std::iter::once(None)
                    .chain(space_icons().iter().copied().map(Some))
                    .collect();
                let mut cell = |part: usize, choice: Option<&'static str>| {
                    let selected = current.as_deref() == choice;
                    let cell = div()
                        .id(("space-icon", part))
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(relative(1. / ICONS_PER_ROW as f32))
                        .h(px(ICON_ROW_HEIGHT))
                        .rounded(px(6.))
                        .when(selected, |cell| cell.bg(theme.control_hover()))
                        .when(lit == Some(part), |cell| {
                            cell.bg(theme.ink(if theme.dark { 0.16 } else { 0.13 }))
                        })
                        .text_size(px(15.))
                        .map(|cell| match choice {
                            Some(emoji) => cell.child(emoji),
                            None => cell.child(div().size(px(7.)).rounded_full().bg(dot)),
                        });
                    highlights(press_only(cell), target(part), cx).on_click(cx.listener(
                        move |this, _, window, cx| {
                            this.run_menu_action(Action::SpaceIcon(space, choice), window, cx)
                        },
                    ))
                };
                // Always this many to a row, as `height` counts them, also
                // in a menu narrowed by a narrow sidebar.
                div()
                    .px(px(2.))
                    .children(
                        choices
                            .chunks(ICONS_PER_ROW)
                            .enumerate()
                            .map(|(row, chunk)| {
                                div().flex().children(chunk.iter().enumerate().map(
                                    |(column, &choice)| cell(row * ICONS_PER_ROW + column, choice),
                                ))
                            }),
                    )
                    .into_any_element()
            }
            Entry::Prompt {
                title,
                detail,
                lines: (title_lines, _),
            } => div()
                .h(px(height))
                .flex()
                .flex_col()
                .gap(px(2.))
                .px(px(PROMPT_PADDING))
                .pt(px(4.))
                .child(
                    // The whole name, question mark and all, unless it is
                    // longer than a few lines.
                    div()
                        .line_clamp(title_lines)
                        .text_ellipsis()
                        .line_height(px(PROMPT_TITLE_LINE))
                        .text_size(px(PROMPT_TITLE_SIZE))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.title())
                        .child(title),
                )
                .child(
                    div()
                        .text_size(px(PROMPT_DETAIL_SIZE))
                        .line_height(px(PROMPT_DETAIL_LINE))
                        .text_color(theme.muted())
                        .child(detail),
                )
                .into_any_element(),
            Entry::Confirm { label, action } => {
                // The highlighted button wears a focus ring, as AppKit draws
                // around the button Return or Space would press.
                let button = |id: &'static str, text: &'static str, part: usize| {
                    div()
                        .id(id)
                        .flex()
                        .items_center()
                        .h(px(26.))
                        .px(px(10.))
                        .rounded(px(7.))
                        .border_2()
                        .border_color(if lit == Some(part) {
                            focus
                        } else {
                            gpui::transparent_black()
                        })
                        .text_size(px(12.5))
                        .font_weight(FontWeight::MEDIUM)
                        .child(text)
                };
                let control_hover = theme.control_hover();
                let cancel = button("menu-cancel", "Cancel", 0)
                    .bg(theme.control())
                    .hover(move |style| style.bg(control_hover))
                    .text_color(theme.title());
                let confirm = button("menu-confirm", label, 1)
                    .bg(theme.danger())
                    .hover(|style| style.opacity(0.85))
                    .text_color(gpui::white());
                div()
                    .h(px(CONFIRM_HEIGHT))
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(6.))
                    .px(px(3.))
                    .child(
                        highlights(cancel, target(0), cx)
                            .on_click(cx.listener(|this, _, _, cx| this.close_menu(cx))),
                    )
                    .child(highlights(confirm, target(1), cx).on_click(cx.listener(
                        move |this, _, window, cx| this.run_menu_action(action, window, cx),
                    )))
                    .into_any_element()
            }
            Entry::Space {
                name,
                icon: space_icon,
                theme: space_theme,
                shortcut,
                current,
                action,
            } => {
                let row = item(3000 + index, true)
                    .gap(px(8.))
                    .child(space_mark(space_icon, space_theme, theme.dark))
                    .child(
                        truncated(name)
                            .flex_1()
                            .min_w_0()
                            .when(current, |name| name.font_weight(FontWeight::SEMIBOLD)),
                    )
                    .when(current, |row| {
                        let check = if lit.is_some() {
                            gpui::white()
                        } else {
                            theme.title()
                        };
                        row.child(icon(Icon::Check, 13., check).flex_none())
                    })
                    .when(!shortcut.is_empty(), |row| {
                        row.child(div().flex_none().text_color(secondary).child(shortcut))
                    });
                highlights(row, target(0), cx)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run_menu_action(action, window, cx)
                    }))
                    .into_any_element()
            }
            Entry::Note(text) => div()
                .h(px(ITEM_HEIGHT))
                .flex()
                .items_center()
                .px(px(9.))
                .text_size(px(11.5))
                .text_color(theme.tertiary())
                .child(text)
                .into_any_element(),
        }
    }

    fn run_menu_action(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        // The menu turns into the question, or the list, where it is.
        let next = match action {
            Action::AskDeleteSpace(id) => Some(MenuKind::DeleteSpace(id)),
            Action::AskMoveTab(id) => Some(MenuKind::MoveTab(id)),
            _ => None,
        };
        if let (Some(kind), Some(menu)) = (next, &mut self.menu) {
            menu.become_menu(kind);
            return cx.notify();
        }
        // Colours and icons apply at once and the menu stays, so several
        // can be tried; Esc or a press elsewhere closes it.
        match action {
            Action::Theme(id, theme) => return self.set_space_theme(id, theme, cx),
            Action::SpaceIcon(id, icon) => return self.set_space_icon(id, icon, cx),
            _ => {}
        }
        self.close_menu(cx);
        match action {
            Action::TogglePin(id) => self.toggle_pin(id, cx),
            Action::ToggleFavorite(id) => self.toggle_favorite(id, cx),
            Action::RenameTab(id) => self.start_tab_rename(id, window, cx),
            Action::ResetTabName(id) => self.reset_tab_name(id, cx),
            Action::GoToPinnedPage(id) => self.go_to_pinned_page(id, window, cx),
            Action::CopyUrl(id) => self.copy_url(id, cx),
            Action::Duplicate(id) => self.duplicate_tab(id, window, cx),
            Action::Reload(id) => self.reload(id, cx),
            Action::Close(id) => self.close_tab(id, cx),
            Action::Delete(id) => self.remove_tab(id, cx),
            Action::CloseOtherTabs(id) => self.close_today_tabs_around(id, false, cx),
            Action::CloseTabsBelow(id) => self.close_today_tabs_around(id, true, cx),
            Action::Rename(id) => self.start_rename(id, window, cx),
            Action::Theme(..) | Action::SpaceIcon(..) => {}
            Action::SwitchSpace(id) => self.switch_space(id, window, cx),
            Action::NewSpace => self.new_space(window, cx),
            Action::UnloadSpace(id) => self.unload_space(id, cx),
            Action::NewTab => {
                self.toggle_command_bar(super::command_bar::CommandMode::NewTab, window, cx)
            }
            Action::Reopen(index) => self.reopen_closed(Some(index), window, cx),
            Action::AskDeleteSpace(_) | Action::AskMoveTab(_) | Action::Cancel => {}
            Action::DeleteSpace(id) => self.delete_space(id, window, cx),
            Action::RevealDownload(id) => self.reveal_download(id),
            Action::MoveTab(id, space_id) => self.move_tab_to_space(id, space_id, cx),
        }
    }

    /// The browser window's key context: "Browser", and "Menu" while a menu
    /// is open, for the menu's keys.
    pub(super) fn key_context(&self) -> KeyContext {
        let mut context = KeyContext::default();
        context.add("Browser");
        if self.menu.is_some() {
            context.add("Menu");
        }
        context
    }

    /// The pointer entered or left a choice: it takes the highlight, and
    /// gives it back to the menu's default when it leaves.
    fn hover_menu(&mut self, target: Highlight, hovered: bool, cx: &mut Context<Self>) {
        let Some(menu) = &mut self.menu else {
            return;
        };
        let highlight = if hovered {
            Some(target)
        } else if menu.highlight == Some(target) {
            None
        } else {
            return;
        };
        if menu.highlight != highlight {
            menu.highlight = highlight;
            cx.notify();
        }
    }

    /// Moves the open menu's highlight with the arrow keys, Home, End, Tab
    /// and ⇧Tab. → on "Move to Space ›" lists the spaces, and ← goes back.
    fn step_menu(&mut self, step: Step, cx: &mut Context<Self>) {
        let Some(menu) = &self.menu else {
            return;
        };
        let kind = menu.kind;
        let entries = self.menu_entries(kind);
        // The entries can change under the highlight (a tab closed by a
        // shortcut empties its menu).
        let current = menu
            .highlight
            .filter(|at| {
                entries
                    .get(at.entry)
                    .is_some_and(|entry| at.part < entry.parts())
            })
            .or_else(|| default_highlight(&entries));
        let row_choice = current.filter(|at| entries[at.entry].parts() > 1);
        let next = match (step, current) {
            // Within a row of choices, ← and → pick the neighbour.
            (Step::Left | Step::Right, Some(at)) if row_choice.is_some() => {
                let parts = entries[at.entry].parts();
                let part = if step == Step::Right {
                    (at.part + 1) % parts
                } else {
                    (at.part + parts - 1) % parts
                };
                Some(Highlight { part, ..at })
            }
            (Step::Right, Some(at)) => {
                if let Some(Action::AskMoveTab(id)) = entries[at.entry].action(at.part) {
                    return self.enter_submenu(MenuKind::MoveTab(id), cx);
                }
                return;
            }
            (Step::Left, _) => {
                if let MenuKind::MoveTab(id) = kind {
                    self.leave_submenu(MenuKind::Tab(id), cx);
                }
                return;
            }
            (Step::Right, None) => return,
            (Step::NextControl | Step::PreviousControl, _) => {
                let targets: Vec<Highlight> = entries
                    .iter()
                    .enumerate()
                    .flat_map(|(entry, item)| {
                        (0..item.parts()).map(move |part| Highlight { entry, part })
                    })
                    .collect();
                let count = targets.len();
                let index = current.and_then(|at| targets.iter().position(|t| *t == at));
                let index = match (index, step == Step::NextControl) {
                    (Some(index), true) => (index + 1) % count.max(1),
                    (Some(index), false) => (index + count - 1) % count.max(1),
                    (None, true) => 0,
                    (None, false) => count.saturating_sub(1),
                };
                targets.get(index).copied()
            }
            (Step::Previous | Step::Next | Step::First | Step::Last, _) => {
                let choices: Vec<usize> = (0..entries.len())
                    .filter(|&entry| entries[entry].parts() > 0)
                    .collect();
                let count = choices.len();
                let index = current.and_then(|at| choices.iter().position(|&e| e == at.entry));
                let index = match (step, index) {
                    (Step::First, _) | (Step::Next, None) => 0,
                    (Step::Last, _) | (_, None) => count.saturating_sub(1),
                    (Step::Next, Some(index)) => (index + 1) % count.max(1),
                    (_, Some(index)) => (index + count - 1) % count.max(1),
                };
                choices.get(index).map(|&entry| Highlight {
                    entry,
                    part: entries[entry].current_part(),
                })
            }
        };
        self.highlight_menu(next, cx);
    }

    /// Highlights `target` and scrolls it into view.
    fn highlight_menu(&mut self, target: Option<Highlight>, cx: &mut Context<Self>) {
        let Some(menu) = &mut self.menu else {
            return;
        };
        if let Some(target) = target {
            menu.scroll.scroll_to_item(target.entry);
        }
        if menu.highlight != target {
            menu.highlight = target;
            cx.notify();
        }
    }

    /// From the keyboard, "Move to Space ›" lists the spaces with the first
    /// one highlighted.
    fn enter_submenu(&mut self, kind: MenuKind, cx: &mut Context<Self>) {
        if let Some(menu) = &mut self.menu {
            menu.become_menu(kind);
        }
        self.step_menu(Step::First, cx);
        cx.notify();
    }

    /// ← in the list of spaces: back to the tab's menu, on "Move to Space".
    fn leave_submenu(&mut self, kind: MenuKind, cx: &mut Context<Self>) {
        let entries = self.menu_entries(kind);
        let parent = entries
            .iter()
            .position(|entry| matches!(entry.action(0), Some(Action::AskMoveTab(_))));
        if let Some(menu) = &mut self.menu {
            menu.become_menu(kind);
        }
        self.highlight_menu(parent.map(|entry| Highlight { entry, part: 0 }), cx);
        cx.notify();
    }

    /// Return or Space: chooses the highlighted item, or enters the list it
    /// opens.
    fn choose_in_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = &self.menu else {
            return;
        };
        let entries = self.menu_entries(menu.kind);
        let Some(at) = menu.highlight.or_else(|| default_highlight(&entries)) else {
            return;
        };
        match entries
            .get(at.entry)
            .and_then(|entry| entry.action(at.part))
        {
            Some(Action::AskMoveTab(id)) => self.enter_submenu(MenuKind::MoveTab(id), cx),
            Some(Action::Cancel) => self.close_menu(cx),
            Some(action) => self.run_menu_action(action, window, cx),
            None => {}
        }
    }

    /// ⌘⌫ in a question: its red button, as in AppKit's alerts.
    fn choose_destructive(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = &self.menu else {
            return;
        };
        let action = self
            .menu_entries(menu.kind)
            .iter()
            .find_map(|entry| match entry {
                Entry::Confirm { action, .. } => Some(*action),
                _ => None,
            });
        if let Some(action) = action {
            self.run_menu_action(action, window, cx);
        }
    }

    /// A typed letter or digit jumps to the next item starting with it.
    fn type_in_menu(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(menu) = &self.menu else {
            return;
        };
        let modifiers = &event.keystroke.modifiers;
        if modifiers.platform || modifiers.control || modifiers.alt || modifiers.function {
            return;
        }
        let Some(typed) = event
            .keystroke
            .key_char
            .as_deref()
            .and_then(|text| text.chars().next())
            .filter(|ch| ch.is_alphanumeric())
        else {
            return;
        };
        let entries = self.menu_entries(menu.kind);
        let after = menu.highlight.map_or(0, |at| at.entry + 1);
        let starts_with = |entry: &Entry| {
            entry.label().is_some_and(|label| {
                label
                    .trim_start()
                    .chars()
                    .next()
                    .is_some_and(|first| first.to_lowercase().eq(typed.to_lowercase()))
            })
        };
        let count = entries.len();
        let found = (0..count)
            .map(|offset| (after + offset) % count.max(1))
            .find(|&entry| starts_with(&entries[entry]));
        if let Some(entry) = found {
            self.highlight_menu(Some(Highlight { entry, part: 0 }), cx);
        }
    }
}

/// Keeps the menu's highlight on `element`, a choice, while the pointer is
/// over it.
fn highlights(
    element: Stateful<Div>,
    target: Highlight,
    cx: &mut Context<BrowserApp>,
) -> Stateful<Div> {
    element.on_hover(
        cx.listener(move |this, hovered: &bool, _, cx| this.hover_menu(target, *hovered, cx)),
    )
}

/// The open menu's keys, on the browser window's root.
pub(super) fn handle_menu_keys<E: InteractiveElement>(root: E, cx: &mut Context<BrowserApp>) -> E {
    root.on_action(
        cx.listener(|this, _: &SelectPrevious, _, cx| this.step_menu(Step::Previous, cx)),
    )
    .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step_menu(Step::Next, cx)))
    .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.step_menu(Step::First, cx)))
    .on_action(cx.listener(|this, _: &SelectLast, _, cx| this.step_menu(Step::Last, cx)))
    .on_action(cx.listener(|this, _: &SelectLeft, _, cx| this.step_menu(Step::Left, cx)))
    .on_action(cx.listener(|this, _: &SelectRight, _, cx| this.step_menu(Step::Right, cx)))
    .on_action(cx.listener(|this, _: &NextControl, _, cx| this.step_menu(Step::NextControl, cx)))
    .on_action(
        cx.listener(|this, _: &PreviousControl, _, cx| this.step_menu(Step::PreviousControl, cx)),
    )
    .on_action(cx.listener(|this, _: &Choose, window, cx| this.choose_in_menu(window, cx)))
    .on_action(
        cx.listener(|this, _: &ChooseDestructive, window, cx| this.choose_destructive(window, cx)),
    )
    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.type_in_menu(event, cx)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menus_stay_inside_the_sidebar() {
        let menu = Size {
            width: 200.,
            height: 150.,
        };
        let at = |x: f32, y: f32, width: f32, height: f32| {
            place(point(px(x), px(y)), px(y), menu, width, height)
        };
        // Room below: opens at the anchor.
        assert_eq!(at(20., 100., 228., 800.), point(px(20.), px(100.)));
        // Too far right: pulled left to keep the edge.
        assert_eq!(at(150., 100., 228., 800.), point(px(22.), px(100.)));
        // No room below: opens above the anchor.
        assert_eq!(at(10., 760., 228., 800.), point(px(10.), px(610.)));
        // Never past the top or left edges.
        assert_eq!(at(-5., 100., 228., 200.), point(px(EDGE), px(EDGE)));
    }

    #[test]
    fn tab_menus_leave_their_row_uncovered() {
        let menu = Size {
            width: 200.,
            height: 150.,
        };
        // Below a row from y 300 to 336, or above it near the bottom.
        let below = |top: f32, bottom: f32| {
            place(
                point(px(40.), px(bottom + ROW_GAP)),
                px(top - ROW_GAP),
                menu,
                228.,
                800.,
            )
        };
        assert_eq!(below(300., 336.), point(px(22.), px(339.)));
        assert_eq!(below(700., 736.), point(px(22.), px(547.)));
    }
}
