//! Popover menus for tabs, spaces, the library, the site by the address and
//! "+". They are drawn in GPUI and kept inside the sidebar, where no native
//! web view covers them.

use super::{
    Tooltip,
    downloads::DownloadState,
    elide::{Cut, elided},
    icons::{Icon, icon},
    permissions::{permission_icon, permission_name},
    press_only, shadow,
    sidebar::space_icons,
    theme::Theme,
    truncated,
};
use crate::{
    app::BrowserApp,
    engine::{Permission, Shortcut, is_insecure, site_origin},
    favicons::host_of,
    model::{Pin, SpaceId, THEME_COUNT, TabId},
};
use gpui::{
    AnyElement, Bounds, ClickEvent, Context, FontWeight, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Pixels, Point, SharedString, Size, StatefulInteractiveElement,
    StyleRefinement, Styled, Window, deferred, div, point, prelude::FluentBuilder, px,
};

const PADDING: f32 = 5.;
const ITEM_HEIGHT: f32 = 24.;
const SEPARATOR_HEIGHT: f32 = 11.;
const HEADER_HEIGHT: f32 = 22.;
const SWATCH_ROW_HEIGHT: f32 = 30.;
const ICON_ROW_HEIGHT: f32 = 28.;
const ICONS_PER_ROW: usize = 7;
/// A question: its title line and two lines of explanation.
const PROMPT_HEIGHT: f32 = 64.;
const CONFIRM_HEIGHT: f32 = 32.;
/// A download: its name, and its progress or how it ended below.
const DOWNLOAD_HEIGHT: f32 = 40.;
/// The site menu's first row: the host, and a warning line for plain HTTP.
const SITE_HEIGHT: f32 = 28.;
const SITE_WARNING_HEIGHT: f32 = 16.;
const PERMISSION_HEIGHT: f32 = 28.;
const ZOOM_HEIGHT: f32 = 30.;
/// Distance the menu keeps from the sidebar's edges.
const EDGE: f32 = 6.;
/// Distance between a tab's row and its menu.
const ROW_GAP: f32 = 3.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuKind {
    Tab(TabId),
    Space(SpaceId),
    /// The bottom bar's "+".
    Plus,
    /// Recent downloads and the recently closed tabs, from the library
    /// button.
    Library,
    /// Asks before deleting a space, in place of the space menu.
    DeleteSpace(SpaceId),
    /// The spaces a tab can move to, in place of the tab menu.
    MoveTab(TabId),
    /// The site the tab shows, from the shield by the address: blocking,
    /// permissions, zoom and its data in this space.
    Site(TabId),
    /// Asks before clearing the site's data, in place of the site menu.
    ClearSiteData(TabId),
}

pub struct OpenMenu {
    pub kind: MenuKind,
    /// Where it was asked for, in window coordinates. Menus open below it,
    /// or, when there is no room, above `above`.
    pub anchor: Point<Pixels>,
    pub above: Pixels,
}

#[derive(Clone, Copy, Debug)]
enum Action {
    TogglePin(TabId),
    ToggleFavorite(TabId),
    CopyLink(TabId),
    Duplicate(TabId),
    Reload(TabId),
    Close(TabId),
    Delete(TabId),
    Rename(SpaceId),
    Theme(SpaceId, u8),
    SpaceIcon(SpaceId, Option<&'static str>),
    NewSpace,
    UnloadSpace(SpaceId),
    NewTab,
    Reopen(usize),
    /// "Delete Space…": the menu asks first.
    AskDeleteSpace(SpaceId),
    DeleteSpace(SpaceId),
    RevealDownload(u64),
    CancelDownload(u64),
    RetryDownload(u64),
    /// "Move to Space ›": the menu lists the spaces.
    AskMoveTab(TabId),
    MoveTab(TabId, SpaceId),
    ToggleBlocking(TabId),
    ResetPermission(TabId, Permission),
    /// The page zoom a step in, out or back to 100%.
    Zoom(Shortcut),
    /// "Clear Site Data…": the menu asks first.
    AskClearSiteData(TabId),
    ClearSiteData(TabId),
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
    Swatches(SpaceId, u8),
    Icons(SpaceId, Option<String>),
    Closed {
        index: usize,
        title: SharedString,
    },
    Empty(&'static str),
    Prompt {
        title: SharedString,
        detail: &'static str,
    },
    /// Cancel, and a red button that runs `action`.
    Confirm {
        label: &'static str,
        action: Action,
    },
    /// A download, with × while it runs and ↻ once stopped; finished ones
    /// show in Finder when clicked.
    Download {
        id: u64,
        name: SharedString,
        status: String,
        state: DownloadState,
        missing: bool,
        can_retry: bool,
    },
    /// The site a site menu is about, and whether its connection is plain
    /// HTTP.
    Site {
        host: String,
        origin: String,
        insecure: bool,
    },
    /// A setting, with a check mark while it is on.
    Check {
        label: &'static str,
        checked: bool,
        action: Action,
    },
    /// A permission the site was given or refused, with Reset.
    Permission {
        permission: Permission,
        allowed: bool,
        action: Action,
    },
    /// The page's zoom, with − and +; the percentage resets it.
    Zoom(u32),
    /// A space, with its icon or its theme's dot.
    Space {
        name: SharedString,
        icon: Option<String>,
        theme: u8,
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
            | Entry::Check { .. }
            | Entry::Space { .. }
            | Entry::Note(_) => ITEM_HEIGHT,
            Entry::Download { .. } => DOWNLOAD_HEIGHT,
            Entry::Site { insecure, .. } => {
                SITE_HEIGHT + if *insecure { SITE_WARNING_HEIGHT } else { 0. }
            }
            Entry::Permission { .. } => PERMISSION_HEIGHT,
            Entry::Zoom(_) => ZOOM_HEIGHT,
            Entry::Separator => SEPARATOR_HEIGHT,
            Entry::Header(_) => HEADER_HEIGHT,
            Entry::Swatches(..) => SWATCH_ROW_HEIGHT,
            Entry::Icons(..) => {
                ICON_ROW_HEIGHT * (space_icons().len() + 1).div_ceil(ICONS_PER_ROW) as f32
            }
            Entry::Prompt { .. } => PROMPT_HEIGHT,
            Entry::Confirm { .. } => CONFIRM_HEIGHT,
        }
    }
}

/// Top-left corner of a `size` menu opening below `anchor`, or above
/// `above` when there is no room, kept `EDGE` inside the sidebar (`width`
/// wide) and the window (`height` tall).
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
        (f32::from(above) - menu.height).max(EDGE)
    };
    point(px(left), px(top))
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
                // ⌘D pins Today tabs and returns pinned tabs and favorites to Today.
                let mut entries = vec![
                    Entry::item(
                        if tab.pin == Pin::Today {
                            "Pin Tab"
                        } else {
                            "Unpin Tab"
                        },
                        "⌘D",
                        Action::TogglePin(id),
                    ),
                    Entry::item(
                        if tab.pin == Pin::Favorite {
                            "Remove from Favorites"
                        } else {
                            "Add to Favorites"
                        },
                        "",
                        Action::ToggleFavorite(id),
                    ),
                    Entry::Separator,
                    Entry::item("Copy Link", "⇧⌘C", Action::CopyLink(id)),
                    Entry::item("Duplicate", "", Action::Duplicate(id)),
                ];
                if self.state.spaces.len() > 1 {
                    entries.push(Entry::item("Move to Space", "›", Action::AskMoveTab(id)));
                }
                entries.extend([
                    Entry::item("Reload", "⌘R", Action::Reload(id)),
                    Entry::Separator,
                    Entry::item("Close Tab", "⌘W", Action::Close(id)),
                ]);
                // Closing a pinned tab only unloads it; this removes it.
                if tab.pin != Pin::Today {
                    entries.push(Entry::item("Delete Tab", "", Action::Delete(id)));
                }
                entries
            }
            MenuKind::Space(id) => {
                let Some(space) = self.state.space(id) else {
                    return Vec::new();
                };
                let mut entries = Vec::new();
                // Names are edited in the title row, which only the active
                // space has on screen.
                if id == self.state.selected_space_id {
                    entries.extend([
                        Entry::item("Rename Space", "", Action::Rename(id)),
                        Entry::Separator,
                    ]);
                }
                entries.extend([
                    Entry::Header("Theme"),
                    Entry::Swatches(id, space.theme()),
                    Entry::Header("Icon"),
                    Entry::Icons(id, space.icon.clone()),
                    Entry::Separator,
                    Entry::item("New Space", "", Action::NewSpace),
                    Entry::item("Unload Space", "", Action::UnloadSpace(id)),
                    Entry::Separator,
                    Entry::item("Delete Space…", "", Action::AskDeleteSpace(id))
                        .enabled(self.state.spaces.len() > 1),
                ]);
                entries
            }
            MenuKind::Plus => vec![
                Entry::item("New Tab", "⌘T", Action::NewTab),
                Entry::item("New Space", "", Action::NewSpace),
            ],
            MenuKind::Library => {
                let mut entries = vec![Entry::Header("Downloads")];
                let downloads: Vec<Entry> = self
                    .downloads
                    .listed()
                    .map(|download| Entry::Download {
                        id: download.id,
                        name: download.name.clone(),
                        status: download.status(),
                        state: download.state,
                        missing: download.missing,
                        can_retry: download.can_retry()
                            && download
                                .space_id
                                .is_some_and(|id| self.state.space(id).is_some()),
                    })
                    .collect();
                if downloads.is_empty() {
                    entries.push(Entry::Empty("No downloads yet"));
                }
                entries.extend(downloads);
                entries.extend([Entry::Separator, Entry::Header("Recently Closed")]);
                // Every tab ⇧⌘T can still bring back, the most recent first.
                let closed = self.state.closed_tabs();
                entries.extend(closed.iter().enumerate().rev().map(|(index, closed)| {
                    Entry::Closed {
                        index,
                        title: closed.tab.title.clone().into(),
                    }
                }));
                if closed.is_empty() {
                    entries.push(Entry::Empty("No recently closed tabs"));
                }
                entries
            }
            MenuKind::Site(id) => {
                let Some((url, host)) = self
                    .state
                    .tab(id)
                    .and_then(|tab| Some((tab.url.clone(), host_of(&tab.url)?)))
                else {
                    return Vec::new();
                };
                let mut entries = vec![
                    Entry::Site {
                        host: host.clone(),
                        origin: site_origin(&url).unwrap_or_default(),
                        insecure: is_insecure(&url),
                    },
                    Entry::Separator,
                    Entry::Check {
                        label: "Block Ads and Trackers",
                        checked: !self.state.is_blocking_disabled(&host),
                        action: Action::ToggleBlocking(id),
                    },
                    Entry::Separator,
                    Entry::Header("Permissions"),
                ];
                let permissions = self.site_permissions(id);
                if permissions.is_empty() {
                    entries.push(Entry::Empty("Nothing allowed or blocked"));
                }
                entries.extend(permissions.into_iter().map(|(permission, allowed)| {
                    Entry::Permission {
                        permission,
                        allowed,
                        action: Action::ResetPermission(id, permission),
                    }
                }));
                if let Some(percent) = self.engine.borrow().zoom_percent(id) {
                    entries.extend([Entry::Separator, Entry::Zoom(percent)]);
                }
                entries.extend([
                    Entry::Separator,
                    Entry::item("Clear Site Data…", "", Action::AskClearSiteData(id)),
                ]);
                entries
            }
            MenuKind::ClearSiteData(id) => {
                let Some((url, host)) = self
                    .state
                    .tab(id)
                    .and_then(|tab| Some((tab.url.clone(), host_of(&tab.url)?)))
                else {
                    return Vec::new();
                };
                vec![
                    Entry::Site {
                        host,
                        origin: site_origin(&url).unwrap_or_default(),
                        insecure: false,
                    },
                    Entry::Prompt {
                        title: "Clear this site's data?".into(),
                        detail: "It signs you out of the site in this space and reloads the page.",
                    },
                    Entry::Confirm {
                        label: "Clear",
                        action: Action::ClearSiteData(id),
                    },
                ]
            }
            MenuKind::DeleteSpace(id) => {
                let Some(space) = self.state.space(id) else {
                    return Vec::new();
                };
                vec![
                    Entry::Prompt {
                        title: format!("Delete {}?", space.name).into(),
                        detail: "Its tabs and site data will be removed.",
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
            MenuKind::Space(_) | MenuKind::Library | MenuKind::DeleteSpace(_) => 212.,
            MenuKind::Site(_) | MenuKind::ClearSiteData(_) => 236.,
        };
        let width = width.min(sidebar - 2. * EDGE);
        let entries = self.menu_entries(menu.kind);
        // A long menu (the library) scrolls within the window's height.
        let height = (2. * PADDING + entries.iter().map(Entry::height).sum::<f32>())
            .min(f32::from(viewport.height) - 2. * EDGE);
        let origin = place(
            menu.anchor,
            menu.above,
            Size { width, height },
            sidebar,
            f32::from(viewport.height),
        );
        let panel = div()
            .id("menu")
            .absolute()
            .left(origin.x)
            .top(origin.y)
            .w(px(width))
            .h(px(height))
            .overflow_y_scroll()
            .p(px(PADDING))
            .rounded(px(8.))
            .bg(theme.surface())
            .border_1()
            .border_color(theme.ink(if theme.dark { 0.14 } else { 0.1 }))
            .shadow(vec![shadow(6., 24., 0.18)])
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(
                div().flex_none().flex().flex_col().children(
                    entries
                        .into_iter()
                        .enumerate()
                        .map(|(index, entry)| self.render_entry(index, entry, theme, cx)),
                ),
            );
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

    fn render_entry(
        &self,
        index: usize,
        entry: Entry,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focus = theme.focus();
        let item = |id: usize, enabled: bool| {
            div()
                .id(("menu-item", id))
                .group("menu-item")
                .flex()
                .items_center()
                .h(px(ITEM_HEIGHT))
                .px(px(9.))
                .rounded(px(5.))
                .text_size(px(13.))
                .text_color(if enabled {
                    theme.title()
                } else {
                    theme.disabled()
                })
                .when(enabled, |item| {
                    item.hover(move |style| style.bg(focus).text_color(gpui::white()))
                })
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
                    row.child(
                        div()
                            .text_color(theme.tertiary())
                            .group_hover("menu-item", |style| {
                                style.text_color(gpui::white().opacity(0.8))
                            })
                            .child(shortcut),
                    )
                })
                .when(enabled, |row| {
                    row.on_click(cx.listener(move |this, _, window, cx| {
                        this.run_menu_action(action, window, cx)
                    }))
                })
                .into_any_element(),
            Entry::Closed { index, title } => {
                let tab = &self.state.closed_tabs()[index].tab;
                item(1000 + index, true)
                    .gap(px(8.))
                    .child(self.favicon(tab, 14., theme))
                    .child(truncated(title).flex_1().min_w_0())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run_menu_action(Action::Reopen(index), window, cx)
                    }))
                    .into_any_element()
            }
            Entry::Download {
                id,
                name,
                status,
                state,
                missing,
                can_retry,
            } => {
                let muted = theme.tertiary();
                let highlighted =
                    |style: StyleRefinement| style.text_color(gpui::white().opacity(0.8));
                // Only a file that is there shows in Finder.
                let reveal = state == DownloadState::Done && !missing;
                let status_color = if state == DownloadState::Failed {
                    theme.danger()
                } else {
                    muted
                };
                let button = match state {
                    DownloadState::Running => Some((
                        "download-cancel",
                        Icon::Close,
                        "Cancel Download",
                        Action::CancelDownload(id),
                    )),
                    DownloadState::Failed | DownloadState::Cancelled if can_retry => Some((
                        "download-retry",
                        Icon::Reload,
                        "Download Again",
                        Action::RetryDownload(id),
                    )),
                    _ => None,
                };
                let sidebar = self.sidebar_width().unwrap_or_default();
                let hover = theme.hover();
                div()
                    .id(("download", id))
                    .group("download")
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(DOWNLOAD_HEIGHT))
                    .pl(px(9.))
                    .pr(px(4.))
                    .rounded(px(5.))
                    .text_color(theme.title())
                    // The whole name: the row cuts long ones in the middle.
                    .tooltip(Tooltip::build_wrapping(name.clone(), theme, sidebar))
                    .when(reveal, |row| {
                        row.hover(move |style| style.bg(focus).text_color(gpui::white()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.run_menu_action(Action::RevealDownload(id), window, cx)
                            }))
                    })
                    .child(
                        icon(Icon::File, 16., muted)
                            .when(reveal, |icon| icon.group_hover("download", highlighted)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(elided(name, Cut::Middle, 17.).text_size(px(13.)))
                            .child(
                                truncated(status)
                                    .text_size(px(11.5))
                                    .line_height(px(15.))
                                    .text_color(status_color)
                                    .when(reveal, |status| {
                                        status.group_hover("download", highlighted)
                                    }),
                            ),
                    )
                    .children(button.map(|(button_id, glyph, label, action)| {
                        press_only(
                            div()
                                .id(button_id)
                                .flex()
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .size(px(24.))
                                .rounded(px(6.))
                                .hover(move |style| style.bg(hover))
                                .child(icon(glyph, 15., theme.icon())),
                        )
                        .tooltip(Tooltip::build(label, "", theme, sidebar))
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                this.run_menu_action(action, window, cx)
                            },
                        ))
                    }))
                    .into_any_element()
            }
            Entry::Site {
                host,
                origin,
                insecure,
            } => {
                let sidebar = self.sidebar_width().unwrap_or_default();
                let danger = theme.danger();
                div()
                    .id("site")
                    .flex()
                    .flex_col()
                    .px(px(9.))
                    // The whole origin: the line keeps the end of a long host.
                    .tooltip(Tooltip::build_wrapping(origin, theme, sidebar))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(7.))
                            .h(px(SITE_HEIGHT))
                            .child(icon(Icon::Globe, 15., theme.icon()))
                            .child(
                                elided(host, Cut::Start, 18.)
                                    .flex_1()
                                    .text_size(px(13.5))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.title()),
                            ),
                    )
                    .when(insecure, |site| {
                        site.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .h(px(SITE_WARNING_HEIGHT))
                                .mt(px(-4.))
                                .text_size(px(11.5))
                                .text_color(danger)
                                .child(icon(Icon::LockOpen, 12., danger))
                                .child("Connection isn't secure"),
                        )
                    })
                    .into_any_element()
            }
            Entry::Check {
                label,
                checked,
                action,
            } => item(index, true)
                .gap(px(8.))
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .size(px(14.))
                        .when(checked, |slot| {
                            slot.child(
                                icon(Icon::Check, 14., focus).group_hover("menu-item", |style| {
                                    style.text_color(gpui::white())
                                }),
                            )
                        }),
                )
                .child(div().flex_1().child(label))
                .on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.run_menu_action(action, window, cx)
                    }),
                )
                .into_any_element(),
            Entry::Permission {
                permission,
                allowed,
                action,
            } => {
                let sidebar = self.sidebar_width().unwrap_or_default();
                let hover = theme.hover();
                div()
                    .id(("permission", index))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(PERMISSION_HEIGHT))
                    .pl(px(9.))
                    .pr(px(4.))
                    .text_size(px(13.))
                    .text_color(theme.title())
                    .child(icon(permission_icon(permission), 14., theme.icon()))
                    .child(truncated(permission_name(permission)).flex_1().min_w_0())
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.5))
                            .text_color(if allowed {
                                theme.tertiary()
                            } else {
                                theme.danger()
                            })
                            .child(if allowed { "Allowed" } else { "Blocked" }),
                    )
                    .child(
                        press_only(
                            div()
                                .id("reset-permission")
                                .flex()
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .size(px(24.))
                                .rounded(px(6.))
                                .hover(move |style| style.bg(hover))
                                .child(icon(Icon::Reopen, 14., theme.icon())),
                        )
                        // Asked again next time.
                        .tooltip(Tooltip::build("Reset", "", theme, sidebar))
                        .on_click(cx.listener(
                            move |this, _, window, cx| this.run_menu_action(action, window, cx),
                        )),
                    )
                    .into_any_element()
            }
            Entry::Zoom(percent) => {
                let sidebar = self.sidebar_width().unwrap_or_default();
                let hover = theme.hover();
                let button = |id: &'static str, shortcut: Shortcut| {
                    press_only(
                        div()
                            .id(id)
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .h(px(24.))
                            .rounded(px(6.))
                            .hover(move |style| style.bg(hover)),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run_menu_action(Action::Zoom(shortcut), window, cx)
                    }))
                };
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .h(px(ZOOM_HEIGHT))
                    .pl(px(9.))
                    .pr(px(4.))
                    .text_size(px(13.))
                    .text_color(theme.title())
                    .child(div().flex_1().child("Zoom"))
                    .child(
                        button("zoom-out", Shortcut::ZoomOut)
                            .w(px(24.))
                            .tooltip(Tooltip::build("Zoom Out", "⌘−", theme, sidebar))
                            .child(icon(Icon::Minus, 15., theme.icon())),
                    )
                    .child(
                        button("zoom-reset", Shortcut::ZoomReset)
                            .min_w(px(48.))
                            .px(px(6.))
                            .font_weight(FontWeight::MEDIUM)
                            .tooltip(Tooltip::build("Actual Size", "⌘0", theme, sidebar))
                            .child(format!("{percent}%")),
                    )
                    .child(
                        button("zoom-in", Shortcut::ZoomIn)
                            .w(px(24.))
                            .tooltip(Tooltip::build("Zoom In", "⌘+", theme, sidebar))
                            .child(icon(Icon::Plus, 15., theme.icon())),
                    )
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
                .px(px(7.))
                .children((0..THEME_COUNT).map(|index| {
                    let swatch = Theme::new(index, theme.dark);
                    press_only(
                        div()
                            .id(("swatch", usize::from(index)))
                            .size(px(18.))
                            .rounded_full()
                            .bg(swatch.swatch())
                            .border_2()
                            .border_color(if index == current {
                                theme.title()
                            } else {
                                theme.ink(0.08)
                            })
                            .cursor_pointer(),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run_menu_action(Action::Theme(space, index), window, cx)
                    }))
                }))
                .into_any_element(),
            Entry::Icons(space, current) => {
                let hover = theme.hover();
                let choices = std::iter::once(None).chain(space_icons().iter().copied().map(Some));
                div()
                    .flex()
                    .flex_wrap()
                    .px(px(2.))
                    .children(choices.enumerate().map(|(index, choice)| {
                        let selected = current.as_deref() == choice;
                        press_only(
                            div()
                                .id(("space-icon", index))
                                .flex()
                                .items_center()
                                .justify_center()
                                .w(px(26.))
                                .h(px(ICON_ROW_HEIGHT))
                                .rounded(px(6.))
                                .when(selected, |cell| cell.bg(theme.control_hover()))
                                .hover(move |style| style.bg(hover))
                                .text_size(px(15.))
                                .map(|cell| match choice {
                                    Some(emoji) => cell.child(emoji),
                                    None => cell
                                        .child(div().size(px(7.)).rounded_full().bg(theme.focus())),
                                }),
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.run_menu_action(Action::SpaceIcon(space, choice), window, cx)
                            },
                        ))
                    }))
                    .into_any_element()
            }
            Entry::Prompt { title, detail } => div()
                .h(px(PROMPT_HEIGHT))
                .flex()
                .flex_col()
                .gap(px(2.))
                .px(px(9.))
                .pt(px(4.))
                .child(
                    truncated(title)
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.title()),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .text_color(theme.muted())
                        .child(detail),
                )
                .into_any_element(),
            Entry::Confirm { label, action } => {
                let button = |id: &'static str, text: &'static str| {
                    div()
                        .id(id)
                        .flex()
                        .items_center()
                        .h(px(24.))
                        .px(px(11.))
                        .rounded(px(6.))
                        .text_size(px(12.5))
                        .font_weight(FontWeight::MEDIUM)
                        .child(text)
                };
                let control_hover = theme.control_hover();
                div()
                    .h(px(CONFIRM_HEIGHT))
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(6.))
                    .px(px(4.))
                    .child(
                        button("menu-cancel", "Cancel")
                            .bg(theme.control())
                            .hover(move |style| style.bg(control_hover))
                            .text_color(theme.title())
                            .on_click(cx.listener(|this, _, _, cx| this.close_menu(cx))),
                    )
                    .child(
                        button("menu-confirm", label)
                            .bg(theme.danger())
                            .hover(|style| style.opacity(0.85))
                            .text_color(gpui::white())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.run_menu_action(action, window, cx)
                            })),
                    )
                    .into_any_element()
            }
            Entry::Space {
                name,
                icon,
                theme: space_theme,
                action,
            } => {
                let dot = Theme::new(space_theme, theme.dark).swatch();
                item(3000 + index, true)
                    .gap(px(8.))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(px(16.))
                            .map(|slot| match icon {
                                Some(emoji) => slot.text_size(px(13.)).child(emoji),
                                None => slot.child(div().size(px(7.)).rounded_full().bg(dot)),
                            }),
                    )
                    .child(truncated(name).flex_1().min_w_0())
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
            Action::AskClearSiteData(id) => Some(MenuKind::ClearSiteData(id)),
            _ => None,
        };
        if let (Some(kind), Some(menu)) = (next, &mut self.menu) {
            menu.kind = kind;
            return cx.notify();
        }
        // The menu shows these changes as they happen, so it stays.
        match action {
            Action::CancelDownload(id) => return self.cancel_download(id),
            Action::RetryDownload(id) => return self.retry_download(id, cx),
            Action::ResetPermission(tab_id, permission) => {
                return self.reset_site_permission(tab_id, permission, cx);
            }
            Action::Zoom(shortcut) => return self.shortcut(shortcut, window, cx),
            _ => {}
        }
        self.close_menu(cx);
        match action {
            Action::TogglePin(id) => self.toggle_pin(id, cx),
            Action::ToggleFavorite(id) => self.toggle_favorite(id, cx),
            Action::CopyLink(id) => self.copy_url(id, cx),
            Action::Duplicate(id) => self.duplicate_tab(id, window, cx),
            Action::Reload(id) => self.reload(id, cx),
            Action::Close(id) => self.close_tab(id, cx),
            Action::Delete(id) => self.remove_tab(id, cx),
            Action::Rename(id) => self.start_rename(id, window, cx),
            Action::Theme(id, theme) => self.set_space_theme(id, theme, cx),
            Action::SpaceIcon(id, icon) => self.set_space_icon(id, icon, cx),
            Action::NewSpace => self.new_space(window, cx),
            Action::UnloadSpace(id) => self.unload_space(id, cx),
            Action::NewTab => {
                self.toggle_command_bar(super::command_bar::CommandMode::NewTab, window, cx)
            }
            Action::Reopen(index) => self.reopen_closed(Some(index), window, cx),
            Action::AskDeleteSpace(_) | Action::AskMoveTab(_) | Action::AskClearSiteData(_) => {}
            Action::DeleteSpace(id) => self.delete_space(id, window, cx),
            Action::RevealDownload(id) => self.reveal_download(id),
            Action::CancelDownload(_)
            | Action::RetryDownload(_)
            | Action::ResetPermission(..)
            | Action::Zoom(_) => {}
            Action::MoveTab(id, space_id) => self.move_tab_to_space(id, space_id, cx),
            Action::ToggleBlocking(id) => {
                if let Some(host) = self.state.tab(id).and_then(|tab| host_of(&tab.url)) {
                    self.toggle_blocking(&host, cx);
                }
            }
            Action::ClearSiteData(id) => self.clear_site_data(id, cx),
        }
    }
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
