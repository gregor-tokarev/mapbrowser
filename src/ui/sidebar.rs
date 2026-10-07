//! The sidebar: navigation row, then per space the URL pill, favorites,
//! the space title, pinned tabs, "+ New Tab" and Today's tabs, and at the
//! bottom the library, the space switcher and "+". Spaces slide sideways
//! under the fixed navigation row and bottom bar.

use super::{
    MaybeTooltip, SlideFrame, Tooltip,
    command_bar::CommandMode,
    drag::{DraggedEdge, DraggedTab, EDGE_HANDLE, TabDrag, TabPreview},
    icons::{Icon, icon},
    menu::MenuKind,
    placeholder_favicon, press_only, shadow,
    theme::Theme,
    truncated,
};
use crate::{
    app::BrowserApp,
    favicons::host_of,
    model::{Pin, Space, Tab, TabId},
};
use gpui::{
    AnyElement, App, AppContext, BoxShadow, ClickEvent, Context, Div, DragMoveEvent, EmptyView,
    FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement,
    Pixels, ScrollWheelEvent, Size, Stateful, StatefulInteractiveElement, Styled, Window, div, img,
    point, prelude::FluentBuilder, px, size,
};

/// The navigation row's centre, level with the traffic lights.
const TOP_ROW_CENTER: f32 = 20.;
const URL_PILL_TOP: f32 = 42.;
/// The sliding pages start just above the URL pill.
const PAGES_TOP: f32 = 40.;
const BOTTOM_BAR: f32 = 48.;
const ROW_HEIGHT: f32 = 36.;
pub(super) const ROW_GAP: f32 = 5.;
const FAVORITE_HEIGHT: f32 = 47.;
pub(super) const FAVORITE_GAP: f32 = 9.;
/// A dragged tab's row or tile stays in place, faded.
const DRAGGED_OPACITY: f32 = 0.4;
const SPACE_ICONS: [&str; 12] = [
    "🏠", "💼", "🔬", "🎨", "🎵", "📚", "✈️", "🛒", "🎮", "⭐", "🌱", "☕",
];

pub fn space_icons() -> &'static [&'static str] {
    &SPACE_ICONS
}

fn selected_shadow() -> Vec<BoxShadow> {
    vec![shadow(1., 2., 0.12)]
}

/// The outline of the tab a ⌃Tab walk has reached, over its row or tile.
fn switch_outline(theme: &Theme) -> Div {
    div()
        .absolute()
        .inset_0()
        .rounded(px(10.))
        .border_2()
        .border_color(theme.focus())
}

/// A 28 pt toolbar button with a rounded hover fill.
fn tool_button(
    id: &'static str,
    glyph: Icon,
    size: f32,
    enabled: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let hover = theme.hover();
    press_only(
        div()
            .id(id)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(28.))
            .rounded(px(6.))
            .when(enabled, |button| button.hover(move |style| style.bg(hover)))
            .child(icon(
                glyph,
                size,
                if enabled {
                    theme.icon()
                } else {
                    theme.disabled()
                },
            )),
    )
}

impl BrowserApp {
    pub(super) fn render_sidebar(
        &self,
        width: f32,
        theme: &Theme,
        frame: Option<SlideFrame>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let active = self.state.active_space();
        let pages: Vec<Div> = match (frame, &self.slide) {
            (Some(frame), Some(slide)) => {
                let shift = width * frame.direction;
                vec![
                    self.render_page(
                        &slide.from,
                        -shift * frame.progress,
                        width,
                        false,
                        theme,
                        cx,
                    ),
                    self.render_page(
                        active,
                        shift * (1. - frame.progress),
                        width,
                        true,
                        theme,
                        cx,
                    ),
                ]
            }
            _ => vec![self.render_page(active, 0., width, true, theme, cx)],
        };
        // Only while a tab is dragged over the list: where it would land.
        let indicator = self.tab_drag.and_then(|drag| drag.target).map(|target| {
            let bounds = self.drop_zones.borrow().indicator(target);
            div()
                .absolute()
                .left(bounds.left())
                .top(bounds.top() - px(PAGES_TOP))
                .w(bounds.size.width)
                .h(bounds.size.height)
                .rounded_full()
                .bg(theme.focus())
        });
        div()
            .id("sidebar")
            .absolute()
            .left_0()
            .top_0()
            .bottom_0()
            .w(px(width))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if this.sidebar_hovered != *hovered {
                    this.sidebar_hovered = *hovered;
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.sidebar_mouse_down(event, window, cx)
                }),
            )
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                this.swipe_spaces(event, window, cx)
            }))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DraggedTab>, _, cx| {
                    this.drag_tab_over(event, cx)
                }),
            )
            .on_drop(cx.listener(|this, dragged: &DraggedTab, window, cx| {
                this.drop_tab(dragged.0, window, cx)
            }))
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(PAGES_TOP))
                    .bottom(px(BOTTOM_BAR))
                    .overflow_hidden()
                    .children(pages)
                    .children(indicator),
            )
            .child(self.render_top_row(width, theme, cx))
            .children(self.render_notice(theme, cx))
            .child(self.render_bottom_bar(width, theme, cx))
            .child(render_edge(width, cx))
    }

    /// Follows a tab dragged over the sidebar. Between the navigation row
    /// and the bottom bar it finds the tab's drop position.
    fn drag_tab_over(&mut self, event: &DragMoveEvent<DraggedTab>, cx: &mut Context<Self>) {
        let (pointer, sidebar) = (event.event.position, event.bounds);
        let over_list = sidebar.contains(&pointer)
            && pointer.y >= sidebar.top() + px(PAGES_TOP)
            && pointer.y < sidebar.bottom() - px(BOTTOM_BAR);
        let drag = TabDrag {
            tab: event.drag(cx).0,
            target: over_list.then(|| self.drop_zones.borrow().target(pointer)),
        };
        if self.tab_drag != Some(drag) {
            self.tab_drag = Some(drag);
            cx.notify();
        }
    }

    fn render_top_row(&self, width: f32, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let page = self
            .active_tab_id()
            .map(|id| self.page(id))
            .unwrap_or_default();
        let has_tab = self.active_tab_id().is_some();
        let tooltip = |text, shortcut| self.tooltip(text, shortcut, theme, width);
        let button_top = px(TOP_ROW_CENTER - 14.);
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(PAGES_TOP))
            // macOS draws the real traffic lights here; previews draw stand-ins.
            .when(cfg!(not(target_os = "macos")), |row| {
                row.children([14., 34., 54.].map(|left| {
                    div()
                        .absolute()
                        .left(px(left))
                        .top(px(TOP_ROW_CENTER - 6.))
                        .size(px(12.))
                        .rounded_full()
                        .bg(theme.ink(if theme.dark { 0.16 } else { 0.12 }))
                }))
            })
            .child(
                tool_button("toggle-sidebar", Icon::Sidebar, 19., true, theme)
                    .absolute()
                    .left(px(72.))
                    .top(button_top)
                    .maybe_tooltip(tooltip("Hide Sidebar", "⌘S"))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
            )
            .child(
                div()
                    .absolute()
                    .right(px(8.5))
                    .top(button_top)
                    .flex()
                    .gap(px(6.))
                    .child(
                        tool_button("back", Icon::ArrowLeft, 22., page.can_go_back, theme)
                            .maybe_tooltip(tooltip("Back", "⌘["))
                            .on_click(cx.listener(|this, _, _, _| this.back())),
                    )
                    .child(
                        tool_button("forward", Icon::ArrowRight, 22., page.can_go_forward, theme)
                            .maybe_tooltip(tooltip("Forward", "⌘]"))
                            .on_click(cx.listener(|this, _, _, _| this.forward())),
                    )
                    .child(
                        tool_button("reload", Icon::Reload, 18., has_tab, theme)
                            .maybe_tooltip(tooltip("Reload", "⌘R"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(id) = this.active_tab_id() {
                                    this.reload(id, cx);
                                }
                            })),
                    ),
            )
    }

    fn render_page(
        &self,
        space: &Space,
        offset: f32,
        width: f32,
        interactive: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let favorites: Vec<&Tab> = space.section(Pin::Favorite).collect();
        let pinned: Vec<&Tab> = space.section(Pin::Pinned).collect();
        let today: Vec<&Tab> = space.section(Pin::Today).collect();
        let selected = space.selected_tab_id;
        let new_tab_selected = interactive
            && self
                .command_bar
                .as_ref()
                .is_some_and(|bar| bar.mode == CommandMode::NewTab);
        let show_clear = interactive && self.sidebar_hovered && !today.is_empty();
        let rows = |tabs: &[&Tab], first_gap: f32, cx: &mut Context<Self>| {
            tabs.iter()
                .enumerate()
                .map(|(index, tab)| {
                    self.render_row(tab, Some(tab.id) == selected, interactive, width, theme, cx)
                        .mt(px(if index == 0 { first_gap } else { ROW_GAP }))
                })
                .collect::<Vec<_>>()
        };
        let pinned_rows = rows(&pinned, 2., cx);
        let today_rows = rows(&today, ROW_GAP, cx);
        // The active page notes where its parts are, for dropping tabs.
        let zones = interactive.then(|| self.drop_zones.clone());
        let has_favorites = !favorites.is_empty();
        let pinned_count = pinned.len();
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(offset))
            .w(px(width))
            .flex()
            .flex_col()
            .when_some(zones.clone(), |page, zones| {
                page.on_children_prepainted(move |parts, _, _| {
                    let mut zones = zones.borrow_mut();
                    zones.list_top = parts.last().map_or(px(0.), |list| list.top());
                    if !has_favorites {
                        zones.favorites.clear();
                    }
                })
            })
            .child(self.render_url_pill(space, interactive, width, theme, cx))
            .when(has_favorites, |page| {
                page.child(
                    self.render_favorites(&favorites, selected, width, interactive, theme, cx)
                        .when_some(zones.clone(), |grid, zones| {
                            grid.on_children_prepainted(move |tiles, _, _| {
                                zones.borrow_mut().favorites = tiles;
                            })
                        }),
                )
            })
            .child(
                div()
                    .id(("tabs", space.id))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    // Sideways swipes switch spaces instead of scrolling the list.
                    .map(|mut list| {
                        list.style().restrict_scroll_to_axis = Some(true);
                        list
                    })
                    .pb(px(8.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .when_some(zones, |list, zones| {
                                list.on_children_prepainted(move |parts, _, _| {
                                    zones.borrow_mut().measure_list(&parts, pinned_count)
                                })
                            })
                            .child(self.render_space_title(
                                space,
                                has_favorites,
                                interactive,
                                theme,
                                cx,
                            ))
                            .children(pinned_rows)
                            .child(self.render_divider(pinned_count == 0, show_clear, theme, cx))
                            .child(self.render_new_tab(new_tab_selected, interactive, theme, cx))
                            .children(today_rows),
                    ),
            )
    }

    fn render_url_pill(
        &self,
        space: &Space,
        interactive: bool,
        width: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tab = space.selected_tab();
        let host = tab.and_then(|tab| host_of(&tab.url));
        let label = tab.map(|tab| host.clone().unwrap_or_else(|| tab.url.clone()));
        let blocking_off = host
            .as_deref()
            .is_some_and(|host| self.state.is_blocking_disabled(host));
        // Buttons show on hover only (Arc), so at rest the host has the
        // whole pill; the shield stays while blocking is off for the site.
        let hovered = interactive && self.url_pill_hovered;
        let copy = tab.filter(|_| hovered).map(|tab| tab.id);
        let shield = host.filter(|_| interactive && (hovered || blocking_off));
        let buttons = copy.is_some() || shield.is_some();
        let (control, control_hover) = (theme.control(), theme.control_hover());
        let hover = theme.hover();
        let pill_button = |id: &'static str, glyph: Icon, tooltip, shortcut| {
            press_only(
                div()
                    .id(id)
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(26.))
                    .rounded(px(6.))
                    .hover(move |style| style.bg(hover))
                    .child(icon(glyph, 17., theme.icon())),
            )
            .maybe_tooltip(self.tooltip(tooltip, shortcut, theme, width))
        };
        div()
            .id(("url-pill", space.id))
            .flex()
            .flex_none()
            .items_center()
            .h(px(36.))
            .mx(px(8.))
            .mt(px(URL_PILL_TOP - PAGES_TOP))
            .pl(px(13.))
            .pr(px(if buttons { 5. } else { 13. }))
            .gap(px(2.))
            .rounded(px(10.))
            .bg(control)
            .when(interactive, |pill| {
                pill.cursor_pointer()
                    .hover(move |style| style.bg(control_hover))
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        if this.url_pill_hovered != *hovered {
                            this.url_pill_hovered = *hovered;
                            cx.notify();
                        }
                    }))
                    // On the press, like "+ New Tab".
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.toggle_command_bar(CommandMode::EditAddress, window, cx)
                        }),
                    )
            })
            .map(|pill| match &label {
                Some(label) => pill.child(
                    truncated(label.clone())
                        .flex_1()
                        .min_w_0()
                        .text_size(px(13.5))
                        .text_color(theme.url_text()),
                ),
                None => pill
                    .gap(px(7.))
                    .child(icon(Icon::Search, 13., theme.tertiary()))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(13.5))
                            .text_color(theme.tertiary())
                            .child("Search or Enter URL…"),
                    ),
            })
            .when_some(copy, |pill, id| {
                pill.child(
                    pill_button("copy-url", Icon::Link, "Copy URL", "⇧⌘C").on_click(cx.listener(
                        move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.copy_url(id, cx);
                        },
                    )),
                )
            })
            .when_some(shield, |pill, host| {
                pill.child(
                    pill_button(
                        "blocking",
                        if blocking_off {
                            Icon::ShieldOff
                        } else {
                            Icon::Shield
                        },
                        if blocking_off {
                            "Content Blocking Off"
                        } else {
                            "Content Blocking On"
                        },
                        "",
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_blocking(&host, cx);
                    })),
                )
            })
    }

    fn render_favorites(
        &self,
        favorites: &[&Tab],
        selected: Option<u64>,
        width: f32,
        interactive: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let columns = favorites.len().min(4) as f32;
        let tile_size = size(
            px(((width - 16.) - (columns - 1.) * FAVORITE_GAP) / columns),
            px(FAVORITE_HEIGHT),
        );
        let (control, control_hover) = (theme.control(), theme.control_hover());
        div()
            .flex()
            .flex_none()
            .flex_wrap()
            .gap(px(FAVORITE_GAP))
            .mx(px(8.))
            .mt(px(8.))
            .children(favorites.iter().map(|tab| {
                let id = tab.id;
                let is_selected = Some(id) == selected;
                div()
                    .id(("favorite", id))
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(tile_size.width)
                    .h(tile_size.height)
                    .rounded(px(10.))
                    .relative()
                    .map(|tile| {
                        if is_selected {
                            tile.bg(gpui::white().opacity(if theme.dark { 0.14 } else { 0.92 }))
                                .border_2()
                                .border_color(theme.focus())
                                .shadow(selected_shadow())
                        } else if self.has_open_menu(id) {
                            tile.bg(control_hover)
                        } else {
                            tile.bg(control).hover(move |style| style.bg(control_hover))
                        }
                    })
                    .when(self.is_dragged(id), |tile| tile.opacity(DRAGGED_OPACITY))
                    .child(self.favicon(tab, 20., theme))
                    .when(interactive, |tile| {
                        // Its title when it opens: a favorite isn't redrawn
                        // when only its title changes.
                        let tooltip = self.menu.is_none().then(|| {
                            let (app, theme) = (cx.weak_entity(), *theme);
                            move |window: &mut Window, cx: &mut App| {
                                let title = app.upgrade().and_then(|app| {
                                    Some(app.read(cx).state.tab(id)?.title.clone())
                                });
                                Tooltip::build(title.unwrap_or_default(), "", &theme, width)(
                                    window, cx,
                                )
                            }
                        });
                        self.draggable(press_only(tile), tab, tile_size, true, theme)
                            .maybe_tooltip(tooltip)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.select_tab(id, window, cx)
                            }))
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                    this.open_tab_menu(id, event.position, window, cx)
                                }),
                            )
                            .when(self.is_switch_target(id), |tile| {
                                tile.child(switch_outline(theme))
                            })
                    })
            }))
    }

    fn render_space_title(
        &self,
        space: &Space,
        after_favorites: bool,
        interactive: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let id = space.id;
        let renaming = interactive && self.renaming == Some(id);
        let hover = theme.hover();
        div()
            .id(("space-title", id))
            .group("space-title")
            .flex()
            .items_center()
            .h(px(34.))
            .mx(px(8.))
            .mt(px(if after_favorites { 9. } else { 8. }))
            .pl(px(9.))
            .pr(px(4.))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(16.))
                    .map(|slot| match &space.icon {
                        Some(emoji) => slot.text_size(px(14.)).child(emoji.clone()),
                        None => slot.child(div().size(px(7.)).rounded_full().bg(theme.focus())),
                    }),
            )
            .child(if renaming {
                div()
                    .flex_1()
                    .min_w_0()
                    .ml(px(12.))
                    .font_weight(FontWeight::SEMIBOLD)
                    // A press on the field takes keyboard focus back from the
                    // page; AppKit doesn't move it to the shell on a click.
                    .capture_any_mouse_down(
                        cx.listener(|this, _, _, _| this.engine.borrow().focus_shell()),
                    )
                    .child(self.rename_input.clone())
            } else {
                truncated(space.name.clone())
                    .flex_1()
                    .min_w_0()
                    .ml(px(12.))
                    .text_size(px(13.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted())
            })
            .when(interactive && !renaming, |row| {
                press_only(row)
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.open_menu(MenuKind::Space(id), event.position, window, cx)
                        }),
                    )
                    .child(
                        press_only(
                            div()
                                .id("space-menu")
                                .flex()
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .size(px(24.))
                                .rounded(px(6.))
                                .invisible()
                                .group_hover("space-title", |style| style.visible())
                                .hover(move |style| style.bg(hover))
                                .child(icon(Icon::Ellipsis, 18., theme.icon())),
                        )
                        .on_click(cx.listener(
                            move |this, event: &ClickEvent, window, cx| {
                                let anchor = event.position() + point(px(-12.), px(14.));
                                this.open_menu(MenuKind::Space(id), anchor, window, cx)
                            },
                        )),
                    )
            })
    }

    fn render_divider(
        &self,
        after_title: bool,
        show_clear: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let hover = theme.hover();
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(12.))
            .mx(px(18.))
            .mt(px(if after_title { 9. } else { ROW_GAP }))
            .child(div().flex_1().h(px(0.5)).bg(theme.divider()))
            .when(show_clear, |divider| {
                divider.child(
                    press_only(
                        div()
                            .id("clear-today")
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .h(px(20.))
                            .px(px(5.))
                            .rounded(px(5.))
                            .hover(move |style| style.bg(hover))
                            .text_size(px(11.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.tertiary())
                            .child(icon(Icon::Broom, 12., theme.tertiary()))
                            .child("Clear"),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.clear_today(cx))),
                )
            })
    }

    fn render_new_tab(
        &self,
        selected: bool,
        interactive: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let hover = theme.hover();
        let (glyph, label) = if selected {
            (theme.title(), theme.title())
        } else {
            (
                theme.ink(if theme.dark { 0.55 } else { 0.52 }),
                theme.tertiary(),
            )
        };
        div()
            .id("new-tab")
            .flex()
            .items_center()
            .h(px(ROW_HEIGHT))
            .mx(px(8.))
            .mt(px(ROW_GAP))
            .pl(px(9.5))
            .rounded(px(10.))
            .map(|row| {
                if selected {
                    row.bg(theme.selected())
                        .when(!theme.dark, |row| row.shadow(selected_shadow()))
                } else {
                    row.hover(move |style| style.bg(hover))
                }
            })
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(16.))
                    .child(icon(Icon::Plus, 19., glyph)),
            )
            .child(
                div()
                    .ml(px(11.5))
                    .text_size(px(14.))
                    .text_color(label)
                    .child("New Tab"),
            )
            // Toggles on the press, while the bar it closes is still open:
            // the press takes key status from the bar, which closes itself
            // before a click could tell.
            .when(interactive, |row| {
                row.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.toggle_command_bar(CommandMode::NewTab, window, cx)
                    }),
                )
            })
    }

    pub(super) fn favicon(&self, tab: &Tab, size: f32, theme: &Theme) -> AnyElement {
        let host = host_of(&tab.url);
        match host.as_deref().and_then(|host| self.favicons.get(host)) {
            Some(image) => img(image).size(px(size)).flex_none().into_any_element(),
            None => placeholder_favicon(host.as_deref(), theme).into_any_element(),
        }
    }

    fn is_dragged(&self, tab_id: TabId) -> bool {
        self.tab_drag.is_some_and(|drag| drag.tab == tab_id)
    }

    /// The tab whose menu is open stays highlighted, as in Arc.
    fn has_open_menu(&self, tab_id: TabId) -> bool {
        self.menu
            .as_ref()
            .is_some_and(|menu| menu.kind == MenuKind::Tab(tab_id))
    }

    fn is_switch_target(&self, tab_id: TabId) -> bool {
        self.tab_switch
            .as_ref()
            .is_some_and(|switch| switch.target() == tab_id)
    }

    /// Lets `element`, a row or tile of `tab`, be dragged to another place
    /// in the sidebar. A copy of it follows the pointer.
    fn draggable(
        &self,
        element: Stateful<Div>,
        tab: &Tab,
        size: Size<Pixels>,
        tile: bool,
        theme: &Theme,
    ) -> Stateful<Div> {
        let host = host_of(&tab.url);
        let preview = TabPreview {
            title: tab.title.clone().into(),
            favicon: host.as_deref().and_then(|host| self.favicons.get(host)),
            host,
            theme: *theme,
            size,
            tile,
        };
        element.on_drag(DraggedTab(tab.id), move |_, _, _, cx| {
            cx.new(|_| preview.clone())
        })
    }

    fn render_row(
        &self,
        tab: &Tab,
        selected: bool,
        interactive: bool,
        width: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let id = tab.id;
        let hover = theme.hover();
        let button_hover = theme.ink(if theme.dark { 0.10 } else { 0.07 });
        let show_close = interactive && self.tab_drag.is_none();
        // Titles use the whole row until the pointer is over it.
        let title_room = show_close && self.hovered_tab == Some(id);
        // Pinned tabs and favorites unload ("–") instead of closing.
        let glyph = if tab.pin == Pin::Today {
            Icon::Close
        } else {
            Icon::Minus
        };
        div()
            .id(("tab", id))
            .group("tab-row")
            .relative()
            .flex()
            .items_center()
            .h(px(ROW_HEIGHT))
            .mx(px(8.))
            .pl(px(9.5))
            .pr(px(4.))
            .rounded(px(10.))
            .map(|row| {
                if selected {
                    row.bg(theme.selected())
                        .when(!theme.dark, |row| row.shadow(selected_shadow()))
                } else if self.has_open_menu(id) {
                    row.bg(hover)
                } else {
                    row.hover(move |style| style.bg(hover))
                }
            })
            .when(self.is_dragged(id), |row| row.opacity(DRAGGED_OPACITY))
            .child(self.favicon(tab, 16., theme))
            .child(
                truncated(tab.title.clone())
                    .flex_1()
                    .min_w_0()
                    .ml(px(11.5))
                    .when(title_room, |title| title.mr(px(24.)))
                    .text_size(px(14.))
                    .text_color(theme.title()),
            )
            .when(interactive && self.is_switch_target(id), |row| {
                row.child(switch_outline(theme))
            })
            .when(interactive, |row| {
                let row_size = size(px(width - 16.), px(ROW_HEIGHT));
                self.draggable(press_only(row), tab, row_size, false, theme)
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.select_tab(id, window, cx)),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.open_tab_menu(id, event.position, window, cx)
                        }),
                    )
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        let hovered_tab = if *hovered {
                            Some(id)
                        } else {
                            this.hovered_tab.filter(|&tab| tab != id)
                        };
                        if this.hovered_tab != hovered_tab {
                            this.hovered_tab = hovered_tab;
                            cx.notify();
                        }
                    }))
                    // Shown by the row's hover in each frame, not by
                    // `hovered_tab`: after a close the next row moves under a
                    // still pointer, and a second click must close it too.
                    .when(show_close, |row| {
                        row.child(
                            press_only(
                                div()
                                    .id("close")
                                    .absolute()
                                    .right(px(4.))
                                    .top(px((ROW_HEIGHT - 20.) / 2.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(20.))
                                    .rounded(px(5.))
                                    .invisible()
                                    .group_hover("tab-row", |style| style.visible())
                                    .hover(move |style| style.bg(button_hover))
                                    .child(icon(glyph, 18., theme.icon())),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.close_tab(id, cx);
                                },
                            )),
                        )
                    })
            })
    }

    /// The toast and, while a page view covers the card, its load error.
    fn render_notice(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<Div> {
        let card = |div: Div| {
            div.mx(px(8.))
                .px(px(12.))
                .py(px(8.))
                .rounded(px(10.))
                .bg(theme.surface())
                .border_1()
                .border_color(theme.ink(0.08))
                .shadow(vec![shadow(4., 14., 0.12)])
                .text_size(px(12.5))
        };
        let error = self.error_text().filter(|_| self.page_covers_card());
        if self.toast.is_none() && error.is_none() {
            return None;
        }
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(BOTTOM_BAR + 4.))
                .flex()
                .flex_col()
                .gap(px(6.))
                .when_some(self.toast.as_ref(), |notice, toast| {
                    notice.child(
                        card(div())
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.title())
                            .child(icon(Icon::Check, 14., theme.focus()))
                            .child(toast.text.clone()),
                    )
                })
                .when_some(error, |notice, error| {
                    notice.child(
                        card(div())
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().flex_1().text_color(gpui::rgb(0xc4322b)).child(error))
                            .child(
                                press_only(
                                    div()
                                        .id("reload-after-error")
                                        .px(px(8.))
                                        .py(px(3.))
                                        .rounded(px(5.))
                                        .bg(theme.control())
                                        .hover({
                                            let hover = theme.control_hover();
                                            move |style| style.bg(hover)
                                        })
                                        .text_color(theme.title())
                                        .child("Reload"),
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        if let Some(id) = this.active_tab_id() {
                                            this.reload(id, cx);
                                        }
                                    },
                                )),
                            ),
                    )
                }),
        )
    }

    fn render_bottom_bar(&self, width: f32, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let hover = theme.hover();
        let spaces = (self.state.spaces.len() > 1).then(|| {
            div()
                .absolute()
                .left(px(40.))
                .right(px(40.))
                .top_0()
                .bottom_0()
                .flex()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .children(self.state.spaces.iter().map(|space| {
                    let id = space.id;
                    let active = id == self.state.selected_space_id;
                    press_only(
                        div()
                            .id(("space", id))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(28.))
                            .rounded(px(6.))
                            .hover(move |style| style.bg(hover))
                            .map(|item| match &space.icon {
                                Some(emoji) => item
                                    .text_size(px(14.))
                                    .opacity(if active { 1. } else { 0.4 })
                                    .child(emoji.clone()),
                                None => {
                                    item.child(div().size(px(7.)).rounded_full().bg(if active {
                                        theme.icon()
                                    } else {
                                        theme.disabled()
                                    }))
                                }
                            }),
                    )
                    .maybe_tooltip(self.tooltip(space.name.clone(), "", theme, width))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.switch_space(id, window, cx)),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.open_menu(MenuKind::Space(id), event.position, window, cx)
                        }),
                    )
                }))
        });
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .h(px(BOTTOM_BAR))
            .child(
                tool_button("library", Icon::Archive, 18., true, theme)
                    .absolute()
                    .left(px(9.5))
                    .top(px(10.))
                    .maybe_tooltip(self.tooltip("Recently Closed", "", theme, width))
                    .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                        let anchor = event.position() - point(px(14.), px(18.));
                        this.open_menu(MenuKind::Library, anchor, window, cx)
                    })),
            )
            .children(spaces)
            .child(
                tool_button("plus", Icon::Plus, 23., true, theme)
                    .absolute()
                    .left(px(width - 24.5 - 14.))
                    .top(px(10.))
                    .maybe_tooltip(self.tooltip("New Tab or Space", "", theme, width))
                    .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                        let anchor = event.position() - point(px(14.), px(18.));
                        this.open_menu(MenuKind::Plus, anchor, window, cx)
                    })),
            )
    }
}

/// The invisible handle on the sidebar's right edge: drag it to resize the
/// sidebar, double-click it for the standard width.
fn render_edge(width: f32, cx: &mut Context<BrowserApp>) -> impl IntoElement + use<> {
    press_only(
        div()
            .id("sidebar-edge")
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .w(px(EDGE_HANDLE))
            .cursor_ew_resize(),
    )
    .on_drag(DraggedEdge { start_width: width }, |_, _, _, cx| {
        cx.new(|_| EmptyView)
    })
    .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
        if event.click_count() >= 2 {
            this.reset_sidebar_width(cx);
        }
    }))
}
