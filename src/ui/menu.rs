//! Popover menus for tabs, spaces, the library and "+". They are drawn in
//! GPUI and kept inside the sidebar, where no native web view covers them.

use super::{press_only, shadow, sidebar::space_icons, theme::Theme, truncated};
use crate::{
    app::BrowserApp,
    model::{Pin, SpaceId, THEME_COUNT, TabId},
};
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement,
    Pixels, Point, SharedString, Size, StatefulInteractiveElement, Styled, Window, deferred, div,
    point, prelude::FluentBuilder, px,
};

const PADDING: f32 = 5.;
const ITEM_HEIGHT: f32 = 24.;
const SEPARATOR_HEIGHT: f32 = 11.;
const HEADER_HEIGHT: f32 = 22.;
const SWATCH_ROW_HEIGHT: f32 = 30.;
const ICON_ROW_HEIGHT: f32 = 28.;
const ICONS_PER_ROW: usize = 7;
const CLOSED_LIMIT: usize = 10;
/// Distance the menu keeps from the sidebar's edges.
const EDGE: f32 = 6.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuKind {
    Tab(TabId),
    Space(SpaceId),
    /// The bottom bar's "+".
    Plus,
    /// Recently closed tabs, from the library button.
    Library,
}

pub struct OpenMenu {
    pub kind: MenuKind,
    /// Where it was asked for, in window coordinates. Menus open below it,
    /// or above when there is no room.
    pub anchor: Point<Pixels>,
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
}

enum Entry {
    Item {
        label: SharedString,
        shortcut: &'static str,
        action: Action,
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
}

impl Entry {
    fn item(label: impl Into<SharedString>, shortcut: &'static str, action: Action) -> Self {
        Entry::Item {
            label: label.into(),
            shortcut,
            action,
        }
    }

    fn height(&self) -> f32 {
        match self {
            Entry::Item { .. } | Entry::Closed { .. } | Entry::Empty(_) => ITEM_HEIGHT,
            Entry::Separator => SEPARATOR_HEIGHT,
            Entry::Header(_) => HEADER_HEIGHT,
            Entry::Swatches(..) => SWATCH_ROW_HEIGHT,
            Entry::Icons(..) => {
                ICON_ROW_HEIGHT * (space_icons().len() + 1).div_ceil(ICONS_PER_ROW) as f32
            }
        }
    }
}

/// Top-left corner of a `size` menu for `anchor`, kept `EDGE` inside the
/// sidebar (`width` wide) and the window (`height` tall).
fn place(anchor: Point<Pixels>, menu: Size<f32>, width: f32, height: f32) -> Point<Pixels> {
    let (x, y) = (f32::from(anchor.x), f32::from(anchor.y));
    let left = x.min(width - EDGE - menu.width).max(EDGE);
    let top = if y + menu.height <= height - EDGE {
        y
    } else {
        (y - menu.height).max(EDGE)
    };
    point(px(left), px(top))
}

impl BrowserApp {
    fn menu_entries(&self, kind: MenuKind) -> Vec<Entry> {
        match kind {
            MenuKind::Tab(id) => {
                let Some(tab) = self.state.tab(id) else {
                    return Vec::new();
                };
                let mut entries = vec![
                    Entry::item(
                        if tab.pin == Pin::Pinned {
                            "Unpin Tab"
                        } else {
                            "Pin Tab"
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
                    Entry::item("Reload", "⌘R", Action::Reload(id)),
                    Entry::Separator,
                    Entry::item("Close Tab", "⌘W", Action::Close(id)),
                ];
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
                vec![
                    Entry::item("Rename Space", "", Action::Rename(id)),
                    Entry::Separator,
                    Entry::Header("Theme"),
                    Entry::Swatches(id, space.theme()),
                    Entry::Header("Icon"),
                    Entry::Icons(id, space.icon.clone()),
                    Entry::Separator,
                    Entry::item("New Space", "", Action::NewSpace),
                    Entry::item("Unload Space", "", Action::UnloadSpace(id)),
                ]
            }
            MenuKind::Plus => vec![
                Entry::item("New Tab", "⌘T", Action::NewTab),
                Entry::item("New Space", "", Action::NewSpace),
            ],
            MenuKind::Library => {
                let mut entries = vec![Entry::Header("Recently Closed")];
                entries.extend(self.closed.iter().enumerate().rev().take(CLOSED_LIMIT).map(
                    |(index, closed)| Entry::Closed {
                        index,
                        title: closed.tab.title.clone().into(),
                    },
                ));
                if self.closed.is_empty() {
                    entries.push(Entry::Empty("No recently closed tabs"));
                }
                entries
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
            MenuKind::Tab(_) | MenuKind::Plus => 200.,
            MenuKind::Space(_) | MenuKind::Library => 212.,
        };
        let width = width.min(sidebar - 2. * EDGE);
        let entries = self.menu_entries(menu.kind);
        let height = 2. * PADDING + entries.iter().map(Entry::height).sum::<f32>();
        let origin = place(
            menu.anchor,
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
            .p(px(PADDING))
            .flex()
            .flex_col()
            .rounded(px(8.))
            .bg(theme.surface())
            .border_1()
            .border_color(theme.ink(if theme.dark { 0.14 } else { 0.1 }))
            .shadow(vec![shadow(6., 24., 0.18)])
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .children(
                entries
                    .into_iter()
                    .enumerate()
                    .map(|(index, entry)| self.render_entry(index, entry, theme, cx)),
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
        let item = |id: usize| {
            div()
                .id(("menu-item", id))
                .group("menu-item")
                .flex()
                .items_center()
                .h(px(ITEM_HEIGHT))
                .px(px(9.))
                .rounded(px(5.))
                .text_size(px(13.))
                .text_color(theme.title())
                .hover(move |style| style.bg(focus).text_color(gpui::white()))
        };
        match entry {
            Entry::Item {
                label,
                shortcut,
                action,
            } => {
                item(index)
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
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run_menu_action(action, window, cx)
                    }))
                    .into_any_element()
            }
            Entry::Closed { index, title } => {
                let tab = &self.closed[index].tab;
                item(1000 + index)
                    .gap(px(8.))
                    .child(self.favicon(tab, 14., theme))
                    .child(truncated(title).flex_1().min_w_0())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run_menu_action(Action::Reopen(index), window, cx)
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
        }
    }

    fn run_menu_action(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
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
        // Room below: opens at the anchor.
        assert_eq!(
            place(point(px(20.), px(100.)), menu, 228., 800.),
            point(px(20.), px(100.))
        );
        // Too far right: pulled left to keep the edge.
        assert_eq!(
            place(point(px(150.), px(100.)), menu, 228., 800.),
            point(px(22.), px(100.))
        );
        // No room below: opens above the anchor.
        assert_eq!(
            place(point(px(10.), px(760.)), menu, 228., 800.),
            point(px(10.), px(610.))
        );
        // Never past the top or left edges.
        assert_eq!(
            place(point(px(-5.), px(100.)), menu, 228., 200.),
            point(px(EDGE), px(EDGE))
        );
    }
}
