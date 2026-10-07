//! The sidebar: navigation row, then per space the URL pill, favorites,
//! the space title, pinned tabs, "+ New Tab" and Today's tabs, and at the
//! bottom the library, the space switcher and "+". Spaces slide sideways
//! under the fixed navigation row and bottom bar.

use super::{
    SlideFrame, Tooltip,
    command_bar::CommandMode,
    icons::{Icon, icon},
    menu::MenuKind,
    placeholder_favicon, press_only, shadow,
    theme::Theme,
    truncated,
};
use crate::{
    app::BrowserApp,
    favicons::host_of,
    model::{Pin, Space, Tab},
};
use gpui::{
    AnyElement, BoxShadow, ClickEvent, Context, Div, FontWeight, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, Stateful, StatefulInteractiveElement, Styled, div,
    img, point, prelude::FluentBuilder, px,
};

/// The URL pill's top; the sliding pages start just above it.
const PAGES_TOP: f32 = 44.;
const BOTTOM_BAR: f32 = 48.;
const ROW_HEIGHT: f32 = 36.;
const ROW_GAP: f32 = 5.;
const FAVORITE_HEIGHT: f32 = 47.;
const FAVORITE_GAP: f32 = 9.;
const SPACE_ICONS: [&str; 12] = [
    "🏠", "💼", "🔬", "🎨", "🎵", "📚", "✈️", "🛒", "🎮", "⭐", "🌱", "☕",
];

pub fn space_icons() -> &'static [&'static str] {
    &SPACE_ICONS
}

fn selected_shadow() -> Vec<BoxShadow> {
    vec![shadow(1., 2., 0.12)]
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
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(PAGES_TOP))
                    .bottom(px(BOTTOM_BAR))
                    .overflow_hidden()
                    .children(pages),
            )
            .child(self.render_top_row(theme, cx))
            .children(self.render_notice(theme, cx))
            .child(self.render_bottom_bar(width, theme, cx))
    }

    fn render_top_row(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let page = self
            .active_tab_id()
            .map(|id| self.page(id))
            .unwrap_or_default();
        let has_tab = self.active_tab_id().is_some();
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(48.))
            // macOS draws the real traffic lights here; previews draw stand-ins.
            .when(cfg!(not(target_os = "macos")), |row| {
                row.children([14., 34., 54.].map(|left| {
                    div()
                        .absolute()
                        .left(px(left))
                        .top(px(18.))
                        .size(px(12.))
                        .rounded_full()
                        .bg(theme.ink(if theme.dark { 0.16 } else { 0.12 }))
                }))
            })
            .child(
                tool_button("toggle-sidebar", Icon::Sidebar, 19., true, theme)
                    .absolute()
                    .left(px(72.))
                    .top(px(10.))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
            )
            .child(
                div()
                    .absolute()
                    .right(px(8.5))
                    .top(px(10.))
                    .flex()
                    .gap(px(6.))
                    .child(
                        tool_button("back", Icon::ArrowLeft, 22., page.can_go_back, theme)
                            .on_click(cx.listener(|this, _, _, _| this.back())),
                    )
                    .child(
                        tool_button("forward", Icon::ArrowRight, 22., page.can_go_forward, theme)
                            .on_click(cx.listener(|this, _, _, _| this.forward())),
                    )
                    .child(
                        tool_button("reload", Icon::Reload, 18., has_tab, theme).on_click(
                            cx.listener(|this, _, _, cx| {
                                if let Some(id) = this.active_tab_id() {
                                    this.reload(id, cx);
                                }
                            }),
                        ),
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
        let rows = |tabs: &[&Tab], cx: &mut Context<Self>| {
            div().flex().flex_col().gap(px(ROW_GAP)).children(
                tabs.iter().map(|tab| {
                    self.render_row(tab, Some(tab.id) == selected, interactive, theme, cx)
                }),
            )
        };
        let pinned_rows = (!pinned.is_empty()).then(|| rows(&pinned, cx).mt(px(2.)));
        let today_rows = (!today.is_empty()).then(|| rows(&today, cx).mt(px(ROW_GAP)));
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(offset))
            .w(px(width))
            .flex()
            .flex_col()
            .child(self.render_url_pill(space, interactive, theme, cx))
            .when(!favorites.is_empty(), |page| {
                page.child(self.render_favorites(
                    &favorites,
                    selected,
                    width,
                    interactive,
                    theme,
                    cx,
                ))
            })
            .child(
                div()
                    .id(("tabs", space.id))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .pb(px(8.))
                    .child(self.render_space_title(
                        space,
                        !favorites.is_empty(),
                        interactive,
                        theme,
                        cx,
                    ))
                    .children(pinned_rows)
                    .child(self.render_divider(pinned.is_empty(), show_clear, theme, cx))
                    .child(self.render_new_tab(new_tab_selected, interactive, theme, cx))
                    .children(today_rows),
            )
    }

    fn render_url_pill(
        &self,
        space: &Space,
        interactive: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tab = space.selected_tab();
        let host = tab.and_then(|tab| host_of(&tab.url));
        let label = tab.map(|tab| host.clone().unwrap_or_else(|| tab.url.clone()));
        let blocking_off = host
            .as_deref()
            .is_some_and(|host| self.state.is_blocking_disabled(host));
        let (control, control_hover) = (theme.control(), theme.control_hover());
        let hover = theme.hover();
        let pill_button = |id: &'static str, glyph: Icon, always: bool| {
            press_only(
                div()
                    .id(id)
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(26.))
                    .rounded(px(6.))
                    .hover(move |style| style.bg(hover))
                    .when(!always, |button| {
                        button
                            .invisible()
                            .group_hover("url-pill", |style| style.visible())
                    })
                    .child(icon(glyph, 17., theme.icon())),
            )
        };
        div()
            .id(("url-pill", space.id))
            .group("url-pill")
            .flex()
            .flex_none()
            .items_center()
            .h(px(36.))
            .mx(px(8.))
            .mt(px(46. - PAGES_TOP))
            .pl(px(13.))
            .pr(px(5.))
            .gap(px(2.))
            .rounded(px(10.))
            .bg(control)
            .when(interactive, |pill| {
                press_only(pill)
                    .cursor_pointer()
                    .hover(move |style| style.bg(control_hover))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_command_bar(CommandMode::EditAddress, window, cx)
                    }))
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
            .when_some(tab.filter(|_| interactive).map(|tab| tab.id), |pill, id| {
                pill.child(
                    pill_button("copy-url", Icon::Link, false).on_click(cx.listener(
                        move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.copy_url(id, cx);
                        },
                    )),
                )
            })
            .when_some(host.filter(|_| interactive), |pill, host| {
                pill.child(
                    pill_button(
                        "blocking",
                        if blocking_off {
                            Icon::ShieldOff
                        } else {
                            Icon::Shield
                        },
                        blocking_off,
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
        let tile_width = ((width - 16.) - (columns - 1.) * FAVORITE_GAP) / columns;
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
                    .w(px(tile_width))
                    .h(px(FAVORITE_HEIGHT))
                    .rounded(px(10.))
                    .map(|tile| {
                        if is_selected {
                            tile.bg(gpui::white().opacity(if theme.dark { 0.14 } else { 0.92 }))
                                .border_2()
                                .border_color(theme.focus())
                                .shadow(selected_shadow())
                        } else {
                            tile.bg(control).hover(move |style| style.bg(control_hover))
                        }
                    })
                    .child(self.favicon(tab, 20., theme))
                    .when(interactive, |tile| {
                        press_only(tile)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.select_tab(id, window, cx)
                            }))
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                    this.open_menu(MenuKind::Tab(id), event.position, window, cx)
                                }),
                            )
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
            .when(interactive, |row| {
                press_only(row).on_click(cx.listener(|this, _, window, cx| {
                    this.toggle_command_bar(CommandMode::NewTab, window, cx)
                }))
            })
    }

    pub(super) fn favicon(&self, tab: &Tab, size: f32, theme: &Theme) -> AnyElement {
        let host = host_of(&tab.url);
        match host.as_deref().and_then(|host| self.favicons.get(host)) {
            Some(image) => img(image).size(px(size)).flex_none().into_any_element(),
            None => placeholder_favicon(host.as_deref(), theme).into_any_element(),
        }
    }

    fn render_row(
        &self,
        tab: &Tab,
        selected: bool,
        interactive: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let id = tab.id;
        let hover = theme.hover();
        let button_hover = theme.ink(if theme.dark { 0.10 } else { 0.07 });
        // Pinned tabs and favorites unload ("–") instead of closing.
        let glyph = if tab.pin == Pin::Today {
            Icon::Close
        } else {
            Icon::Minus
        };
        div()
            .id(("tab", id))
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
                } else {
                    row.hover(move |style| style.bg(hover))
                }
            })
            .child(self.favicon(tab, 16., theme))
            .child(
                truncated(tab.title.clone())
                    .flex_1()
                    .min_w_0()
                    .ml(px(11.5))
                    .text_size(px(14.))
                    .text_color(theme.title()),
            )
            .when(interactive, |row| {
                press_only(row)
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.select_tab(id, window, cx)),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.open_menu(MenuKind::Tab(id), event.position, window, cx)
                        }),
                    )
                    // Titles use the whole row until the close button shows.
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
                    .when(self.hovered_tab == Some(id), |row| {
                        row.child(
                            press_only(
                                div()
                                    .id("close")
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .justify_center()
                                    .size(px(20.))
                                    .ml(px(4.))
                                    .rounded(px(5.))
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
                    let (name, tooltip_theme) = (space.name.clone(), *theme);
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
                    .tooltip(move |_, cx| Tooltip::view(name.clone(), &tooltip_theme, cx))
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
                    .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                        let anchor = event.position() - point(px(14.), px(18.));
                        this.open_menu(MenuKind::Plus, anchor, window, cx)
                    })),
            )
    }
}
