//! The sidebar: navigation row, then per space the URL pill, favorites,
//! the space title, pinned tabs, "+ New Tab" and Today's tabs, and at the
//! bottom the library, the space switcher and "+". Spaces slide sideways
//! under the fixed navigation row and bottom bar.

use super::{
    CARD_INSET, MaybeTooltip, SlideFrame, TOOLTIP_EDGE, TOOLTIP_PADDING, Tooltip, UI_FONT,
    beside_pointer,
    command_bar::CommandMode,
    drag::{DraggedEdge, DraggedTab, EDGE_HANDLE, TabDrag, TabPreview},
    elide::{Cut, elided},
    icons::{Icon, icon},
    menu::MenuKind,
    page_notice::Leaving,
    placeholder_favicon, press_only, shadow, text_width,
    theme::Theme,
    tooltip_bubble, truncated, wrapped_lines,
};
use crate::{
    app::BrowserApp,
    engine::{Shortcut, site_origin},
    favicons::host_of,
    model::{Pin, Space, SpaceId, Tab, TabId},
    recent::TabSwitch,
};
use gpui::{
    AnyElement, AnyView, App, AppContext, Bounds, BoxShadow, ClickEvent, Context, Div,
    DragMoveEvent, EmptyView, Font, FontWeight, Hsla, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Pixels, Point, Render, ScrollHandle, ScrollWheelEvent,
    SharedString, Size, Stateful, StatefulInteractiveElement, Styled, Window, div, font, img,
    point, prelude::FluentBuilder, px, size,
};
use std::{cell::Cell, collections::HashMap, rc::Rc, time::Instant};

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
const FAVORITE_COLUMNS: usize = 4;
/// Up to this many rows of favorites show whole. More scroll in a grid
/// that shows half a row more, so the tab list below keeps its room.
const FAVORITE_ROWS: usize = 3;
const FAVORITES_SHOWN: f32 =
    FAVORITE_ROWS as f32 * (FAVORITE_HEIGHT + FAVORITE_GAP) + FAVORITE_HEIGHT / 2.;
/// A dragged tab's row or tile stays in place, faded.
const DRAGGED_OPACITY: f32 = 0.4;
/// The favicon of a pinned tab or favorite without a page: it gets no
/// messages or notifications until it loads again.
const ASLEEP_OPACITY: f32 = 0.45;
/// A tab dragged this close to the list's top or bottom edge scrolls it,
/// faster nearer the edge, in points a second.
const AUTOSCROLL_EDGE: f32 = 36.;
const AUTOSCROLL_SLOWEST: f32 = 120.;
const AUTOSCROLL_FASTEST: f32 = 720.;
/// Room between the tab list and a card over its bottom.
const NOTICE_GAP: f32 = 8.;
/// A row's title is this much narrower than the sidebar while its close
/// button shows: the row's margins and padding, the favicon and the button.
const ROW_TITLE_CHROME: f32 = 16. + 9.5 + 16. + 11.5 + 4. + 24.;
const TAB_TOOLTIP_MAX_WIDTH: f32 = 280.;
const TAB_TOOLTIP_LINES: usize = 3;
const SPACE_ICONS: [&str; 12] = [
    "🏠", "💼", "🔬", "🎨", "🎵", "📚", "✈️", "🛒", "🎮", "⭐", "🌱", "☕",
];
/// The space title's text size, and what its row takes beside the name:
/// margins, padding, the icon and its gap, the "…" button.
const TITLE_SIZE: f32 = 13.5;
const TITLE_CHROME: f32 = 2. * 8. + 9. + 4. + 16. + 12. + 24.;
/// The space switcher sits between the library button and "+".
const SWITCHER_INSET: f32 = 40.;
/// A space's button and the gap after it while every space fits. Then the
/// gaps close and the buttons narrow, down to `MIN_SPACE_BUTTON`, before a
/// "…" button lists the spaces that don't fit.
const SPACE_BUTTON: f32 = 28.;
const SPACE_GAP: f32 = 6.;
const MIN_SPACE_BUTTON: f32 = 18.;
const MORE_BUTTON: f32 = 24.;
/// The highlight behind a space's button: the active space, hover, a tab
/// dragged over it.
const SPACE_CHIP: f32 = 24.;

pub fn space_icons() -> &'static [&'static str] {
    &SPACE_ICONS
}

/// Each space's tab list and favorites keep their scroll position, across
/// space switches and while the sidebar is hidden.
#[derive(Default)]
pub struct TabLists {
    lists: HashMap<SpaceId, ListScroll>,
    /// The tab each space last brought into view, and its section: only a
    /// new selection, a new ⌃Tab target or a selected tab moving to another
    /// section (⌘D) scrolls its list.
    revealed: HashMap<SpaceId, (TabId, Pin)>,
    /// A tab dragged near the list's top or bottom edge scrolls it.
    drag: Option<DragScroll>,
    /// The height of the cards over the list's bottom, measured as drawn.
    notice: Rc<Cell<f32>>,
}

#[derive(Clone, Default)]
struct ListScroll {
    tabs: ScrollHandle,
    favorites: ScrollHandle,
}

#[derive(Clone, Copy)]
struct DragScroll {
    /// Points a second, positive downwards.
    speed: f32,
    pointer: Point<Pixels>,
    last: Instant,
}

impl TabLists {
    /// The space's scroll handles, made when it is first shown; those of
    /// deleted spaces go then.
    fn list(&mut self, space_id: SpaceId, spaces: &[Space]) -> ListScroll {
        if !self.lists.contains_key(&space_id) {
            let exists = |id: &SpaceId| spaces.iter().any(|space| space.id == *id);
            self.lists.retain(|id, _| exists(id));
            self.revealed.retain(|id, _| exists(id));
        }
        self.lists.entry(space_id).or_default().clone()
    }
}

/// How fast a tab list scrolls with a tab dragged at `y`: up near its top
/// edge, down near its bottom edge or just past it, faster nearer the edge.
fn autoscroll_speed(y: Pixels, list: Bounds<Pixels>) -> Option<f32> {
    let (y, top, bottom) = (
        f32::from(y),
        f32::from(list.top()),
        f32::from(list.bottom()),
    );
    if bottom - top < 2. * AUTOSCROLL_EDGE {
        return None;
    }
    let depth = if (top..top + AUTOSCROLL_EDGE).contains(&y) {
        y - (top + AUTOSCROLL_EDGE)
    } else if (bottom - AUTOSCROLL_EDGE..bottom + AUTOSCROLL_EDGE).contains(&y) {
        y - (bottom - AUTOSCROLL_EDGE)
    } else {
        return None;
    };
    let nearness = (depth.abs() / AUTOSCROLL_EDGE).min(1.);
    Some(
        depth.signum()
            * (AUTOSCROLL_SLOWEST + (AUTOSCROLL_FASTEST - AUTOSCROLL_SLOWEST) * nearness),
    )
}

/// Scrolls a capped favorites grid of `count` tiles so the one at `index`
/// shows whole.
fn reveal_favorite(grid: &ScrollHandle, index: usize, count: usize) {
    let columns = count.clamp(1, FAVORITE_COLUMNS);
    if count.div_ceil(columns) <= FAVORITE_ROWS {
        return;
    }
    let top = (index / columns) as f32 * (FAVORITE_HEIGHT + FAVORITE_GAP);
    let bottom = top + FAVORITE_HEIGHT;
    let offset = grid.offset();
    let shown = -f32::from(offset.y);
    let scrolled = if top < shown {
        top
    } else if bottom > shown + FAVORITES_SHOWN {
        bottom - FAVORITES_SHOWN
    } else {
        return;
    };
    grid.set_offset(point(offset.x, px(-scrolled)));
}

/// ⌃1–⌃9 go to the first nine spaces.
pub fn space_shortcut(index: usize) -> &'static str {
    const SHORTCUTS: [&str; 9] = ["⌃1", "⌃2", "⌃3", "⌃4", "⌃5", "⌃6", "⌃7", "⌃8", "⌃9"];
    SHORTCUTS.get(index).copied().unwrap_or("")
}

/// How the space switcher shows its spaces.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Switcher {
    /// The width of a space's button, and the gap between two.
    button: f32,
    gap: f32,
    /// How many spaces have a button; "…" lists them all.
    shown: usize,
}

/// The switcher for `count` spaces in `room` points: full-size buttons
/// with shrinking gaps, then narrower buttons side by side, then as many
/// as fit and "…". Every button keeps a usable size and the switcher never
/// runs under its neighbours.
fn switcher(count: usize, room: f32) -> Switcher {
    let n = count.max(1) as f32;
    if n * SPACE_BUTTON <= room {
        let gap = if count > 1 {
            ((room - n * SPACE_BUTTON) / (n - 1.))
                .min(SPACE_GAP)
                .floor()
        } else {
            0.
        };
        return Switcher {
            button: SPACE_BUTTON,
            gap,
            shown: count,
        };
    }
    if n * MIN_SPACE_BUTTON <= room {
        return Switcher {
            button: (room / n).floor(),
            gap: 0.,
            shown: count,
        };
    }
    let room = room - MORE_BUTTON;
    let shown = ((room / MIN_SPACE_BUTTON) as usize).max(1);
    Switcher {
        button: (room / shown as f32).floor().min(SPACE_BUTTON),
        gap: 0.,
        shown,
    }
}

/// A space being dragged along the switcher to another place.
struct DraggedSpace(SpaceId);

/// A dragged space's button under the pointer, lifted.
#[derive(Clone)]
struct SpacePreview {
    icon: Option<String>,
    dot: Hsla,
    theme: Theme,
    width: f32,
}

impl Render for SpacePreview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .font_family(UI_FONT)
            .flex()
            .items_center()
            .justify_center()
            .w(px(self.width))
            .h(px(SPACE_BUTTON))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(SPACE_CHIP))
                    .rounded(px(6.))
                    .bg(self.theme.surface())
                    .border_1()
                    .border_color(self.theme.ink(0.08))
                    .shadow(vec![shadow(3., 10., 0.16)])
                    .child(space_mark(self.icon.clone(), self.dot, true)),
            )
    }
}

/// A space's icon, or a dot in its colour, as the switcher shows it.
fn space_mark(icon: Option<String>, dot: Hsla, active: bool) -> Div {
    let mark = div().flex().items_center().justify_center();
    match icon {
        Some(emoji) => mark
            .text_size(px(14.))
            .opacity(if active { 1. } else { 0.55 })
            .child(emoji),
        None => mark.child(
            div()
                .size(px(if active { 8. } else { 7. }))
                .rounded_full()
                .bg(dot),
        ),
    }
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

/// Sideways swipes over a scrolling list switch spaces instead of
/// scrolling it.
fn vertical_scroll(mut list: Stateful<Div>) -> Stateful<Div> {
    list.style().restrict_scroll_to_axis = Some(true);
    list
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
            // Files from Finder open as tabs.
            .on_drop(
                cx.listener(|this, paths: &gpui::ExternalPaths, _, cx| {
                    this.open_dropped(paths, cx)
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
                    .children(pages)
                    .children(indicator),
            )
            .child(self.render_top_row(width, theme, cx))
            .children(
                self.render_notice(width, theme, cx)
                    .map(|notice| self.measure_notice(notice, cx)),
            )
            .child(self.render_bottom_bar(width, theme, cx))
            .child(render_edge(width, cx))
    }

    /// Follows a tab dragged over the sidebar. Between the navigation row
    /// and the bottom bar it finds the tab's drop position. Near the tab
    /// list's top or bottom edge the list scrolls (`scroll_dragged_tab`).
    fn drag_tab_over(&mut self, event: &DragMoveEvent<DraggedTab>, cx: &mut Context<Self>) {
        let (pointer, sidebar) = (event.event.position, event.bounds);
        let over_list = sidebar.contains(&pointer)
            && pointer.y >= sidebar.top() + px(PAGES_TOP)
            && pointer.y < sidebar.bottom() - px(BOTTOM_BAR);
        let drag = TabDrag {
            tab: event.drag(cx).0,
            target: over_list.then(|| self.drop_zones.borrow().target(pointer)),
        };
        let list = self
            .tab_lists
            .list(self.state.selected_space_id, &self.state.spaces)
            .tabs
            .bounds();
        // Over the space switcher, just past the list's bottom edge, a drop
        // moves the tab to another space: the list stays where it is.
        let over_switcher = self.state.spaces.len() > 1
            && pointer.y >= sidebar.bottom() - px(BOTTOM_BAR)
            && pointer.x >= sidebar.left() + px(SWITCHER_INSET)
            && pointer.x < sidebar.right() - px(SWITCHER_INSET);
        let speed = autoscroll_speed(pointer.y, list)
            .filter(|_| sidebar.contains(&pointer) && !over_switcher);
        let scrolling = self.tab_lists.drag.is_some();
        self.tab_lists.drag = speed.map(|speed| DragScroll {
            speed,
            pointer,
            last: self
                .tab_lists
                .drag
                .map_or_else(Instant::now, |scroll| scroll.last),
        });
        if self.tab_drag != Some(drag) || (speed.is_some() && !scrolling) {
            self.tab_drag = Some(drag);
            cx.notify();
        }
    }

    /// Before each frame: drops a menu whose tab or space is gone, ends a
    /// peek the pointer has left, brings the selected tab (or the ⌃Tab
    /// walk's target) into view, and scrolls a list a tab is held at the
    /// edge of.
    pub(super) fn prepare_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let stale_menu = self.menu.as_ref().is_some_and(|menu| match menu.kind {
            MenuKind::Tab(id)
            | MenuKind::MoveTab(id)
            | MenuKind::Site(id)
            | MenuKind::ClearSiteData(id) => self.state.tab(id).is_none(),
            MenuKind::Space(id) | MenuKind::DeleteSpace(id) => self.state.space(id).is_none(),
            _ => false,
        });
        if stale_menu {
            self.menu = None;
        }
        self.update_peek(window, cx);
        self.reveal_selection();
        self.scroll_dragged_tab(window);
    }

    /// A peeking sidebar hides again once the pointer has left it, unless a
    /// menu, a rename, a drag or the command bar opened from it still needs it.
    fn update_peek(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.peek_ends_at(window.mouse_position().x, cx) {
            self.end_peek();
        }
    }

    fn peek_ends_at(&self, x: Pixels, cx: &App) -> bool {
        self.sidebar_peek
            && self.menu.is_none()
            && self.renaming.is_none()
            && self.renaming_tab.is_none()
            && self.command_bar.is_none()
            && !cx.has_active_drag()
            && f32::from(x) >= self.state.sidebar_width()
    }

    /// The pointer moved while the sidebar peeks. A quick move can leave the
    /// sidebar without one event over it, and then no hover change redraws
    /// to end the peek; this ends it once the pointer is past the sidebar.
    pub(super) fn peek_pointer_moved(&mut self, x: Pixels, cx: &mut Context<Self>) {
        if self.peek_ends_at(x, cx) {
            self.end_peek();
            cx.notify();
        }
    }

    /// Scrolls the active space's list (or favorites) to the selected tab,
    /// or to the tab a ⌃Tab walk has reached, when that changes. Scrolling
    /// by hand leaves it be, and each space keeps where it was.
    fn reveal_selection(&mut self) {
        let space = self.state.active_space();
        let lists = self.tab_lists.list(space.id, &self.state.spaces);
        let target = self
            .tab_switch
            .as_ref()
            .map(TabSwitch::target)
            .or(space.selected_tab_id);
        let Some(tab) = target.and_then(|id| space.tabs.iter().find(|tab| tab.id == id)) else {
            return;
        };
        let shown = (tab.id, tab.pin);
        if self.tab_lists.revealed.insert(space.id, shown) == Some(shown) {
            return;
        }
        let index = space
            .section(tab.pin)
            .position(|other| other.id == tab.id)
            .unwrap_or(0);
        // The list holds the space title, the pinned rows, the divider,
        // "+ New Tab" and the Today rows.
        match tab.pin {
            Pin::Favorite => {
                let count = space.section(Pin::Favorite).count();
                reveal_favorite(&lists.favorites, index, count);
            }
            Pin::Pinned => lists.tabs.scroll_to_item(1 + index),
            Pin::Today => {
                let pinned = space.section(Pin::Pinned).count();
                lists.tabs.scroll_to_item(pinned + 3 + index);
            }
        }
    }

    /// One frame of a dragged tab scrolling the list it is held at the edge
    /// of. Frames are requested only while the list can still move.
    fn scroll_dragged_tab(&mut self, window: &mut Window) {
        if self.tab_drag.is_none() {
            self.tab_lists.drag = None;
        }
        let Some(scroll) = &mut self.tab_lists.drag else {
            return;
        };
        let now = Instant::now();
        // A first or late frame moves it by one frame's worth.
        let elapsed = now.duration_since(scroll.last).as_secs_f32().min(1. / 30.);
        scroll.last = now;
        let (speed, pointer) = (scroll.speed, scroll.pointer);
        let list = self
            .tab_lists
            .list(self.state.selected_space_id, &self.state.spaces)
            .tabs;
        let offset = list.offset();
        let y = (offset.y - px(speed * elapsed)).clamp(-list.max_offset().height, px(0.));
        if y == offset.y {
            return;
        }
        list.set_offset(point(offset.x, y));
        window.request_animation_frame();
        // The rows moved under the still pointer: the last frame's places,
        // shifted back by as much, find its slot among them.
        if let Some(drag) = &mut self.tab_drag
            && drag.target.is_some()
        {
            let moved = point(pointer.x, pointer.y - (y - offset.y));
            drag.target = Some(self.drop_zones.borrow().target(moved));
        }
    }

    /// While the sidebar is hidden, a strip along the window's left edge,
    /// beside the card and the page's view: resting the pointer there shows
    /// the sidebar until the pointer leaves it.
    pub(super) fn render_peek_edge(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        (self.state.sidebar_hidden && self.sidebar_width().is_none() && !self.fullscreen).then(
            || {
                div()
                    .id("sidebar-peek")
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(CARD_INSET))
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        this.point_at_sidebar_edge(*hovered, cx)
                    }))
            },
        )
    }

    /// Notes the notice's height as drawn, which the tab list keeps clear
    /// of while a card stays (`notice_inset`). A new height redraws once.
    fn measure_notice(&self, notice: Div, cx: &mut Context<Self>) -> Div {
        let (height, app) = (self.tab_lists.notice.clone(), cx.weak_entity());
        notice.on_children_prepainted(move |cards, _, cx| {
            let top = cards
                .iter()
                .map(|card| f32::from(card.top()))
                .reduce(f32::min);
            let bottom = cards
                .iter()
                .map(|card| f32::from(card.bottom()))
                .reduce(f32::max);
            let measured = top.zip(bottom).map_or(0., |(top, bottom)| bottom - top);
            if (height.get() - measured).abs() >= 0.5 {
                height.set(measured);
                let app = app.clone();
                // Drawing can't ask for another frame; once it ends, this does.
                cx.defer(move |cx| {
                    let _ = app.update(cx, |_, cx| cx.notify());
                });
            }
        })
    }

    /// How far the tab list ends above the sidebar's bottom bar: clear of a
    /// permission card or a load error, which stay until answered; toasts
    /// go by themselves and may cover it.
    fn notice_inset(&self) -> f32 {
        let card =
            self.permission_shown().is_some() || (self.error.is_some() && self.page_covers_card());
        if card {
            self.tab_lists.notice.get() + NOTICE_GAP
        } else {
            0.
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
        // Right of the sidebar toggle, at 72 to 100. A narrow sidebar
        // tightens the buttons, then leaves out Forward (⌘] still works),
        // so none ever covers the toggle.
        let room = width - 8.5 - (100. + 4.);
        let (gap, forward) = if room >= 3. * 28. + 2. * 6. {
            (6., true)
        } else {
            (2., room >= 3. * 28. + 2. * 2.)
        };
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
                    // A peeking sidebar stays shown; one that shows only for
                    // naming or for a permission card hides again.
                    .maybe_tooltip(tooltip(
                        if self.sidebar_toggle_shows() {
                            "Show Sidebar"
                        } else {
                            "Hide Sidebar"
                        },
                        "⌘S",
                    ))
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.shown_for_naming() {
                            this.end_rename(window, cx);
                        } else {
                            this.toggle_sidebar(cx);
                        }
                    })),
            )
            .child(
                div()
                    .absolute()
                    .right(px(8.5))
                    .top(button_top)
                    .flex()
                    .gap(px(gap))
                    .child(
                        tool_button("back", Icon::ArrowLeft, 22., page.can_go_back, theme)
                            .maybe_tooltip(tooltip("Back", "⌘["))
                            .on_click(cx.listener(|this, _, _, _| this.back())),
                    )
                    .when(forward, |buttons| {
                        buttons.child(
                            tool_button(
                                "forward",
                                Icon::ArrowRight,
                                22.,
                                page.can_go_forward,
                                theme,
                            )
                            .maybe_tooltip(tooltip("Forward", "⌘]"))
                            .on_click(cx.listener(|this, _, _, _| this.forward())),
                        )
                    })
                    .child(if page.loading {
                        tool_button("stop", Icon::Close, 18., true, theme)
                            .maybe_tooltip(tooltip("Stop", "⌘."))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.shortcut(Shortcut::Stop, window, cx)
                            }))
                    } else {
                        tool_button("reload", Icon::Reload, 18., has_tab, theme)
                            .maybe_tooltip(tooltip("Reload", "⌘R"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(id) = this.active_tab_id() {
                                    this.reload(id, cx);
                                }
                            }))
                    }),
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
                        .flex_none()
                        .mt(px(if index == 0 { first_gap } else { ROW_GAP }))
                })
                .collect::<Vec<_>>()
        };
        let pinned_rows = rows(&pinned, 2., cx);
        let today_rows = rows(&today, ROW_GAP, cx);
        // The active page notes where its parts are, for dropping tabs.
        let zones = interactive.then(|| self.drop_zones.clone());
        let has_favorites = !favorites.is_empty();
        let favorites_capped = favorites.len().div_ceil(FAVORITE_COLUMNS) > FAVORITE_ROWS;
        let pinned_count = pinned.len();
        let lists = self
            .tab_lists
            .lists
            .get(&space.id)
            .cloned()
            .unwrap_or_default();
        // The list's parts in order, and the tab each one is.
        let parts_tabs: Vec<Option<TabId>> = std::iter::once(None)
            .chain(pinned.iter().map(|tab| Some(tab.id)))
            .chain([None, None])
            .chain(today.iter().map(|tab| Some(tab.id)))
            .collect();
        // A row that moves under a still pointer (after a close, say) hears
        // of it only once the pointer moves: the list checks once laid out.
        let follow_pointer = (interactive && self.menu.is_none() && self.tab_drag.is_none())
            .then(|| (cx.weak_entity(), self.hovered_tab));
        let list_handle = lists.tabs.clone();
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
                    div()
                        .id(("favorites", space.id))
                        .flex_none()
                        .mt(px(8.))
                        // Many favorites scroll, so the tab list keeps its room.
                        .when(favorites_capped, |grid| {
                            vertical_scroll(
                                grid.max_h(px(FAVORITES_SHOWN))
                                    .overflow_y_scroll()
                                    .track_scroll(&lists.favorites),
                            )
                        })
                        .child(
                            self.render_favorites(
                                &favorites,
                                selected,
                                width,
                                interactive,
                                theme,
                                cx,
                            )
                            .when_some(zones.clone(), |grid, zones| {
                                grid.on_children_prepainted(move |tiles, _, _| {
                                    zones.borrow_mut().favorites = tiles;
                                })
                            }),
                        ),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    // Its parts are its children, so the handle knows where
                    // each row is; none of them shrinks (`flex_none`), so
                    // the list scrolls rather than squeezing them.
                    .flex()
                    .flex_col()
                    .pb(px(8.))
                    .mb(px(if interactive { self.notice_inset() } else { 0. }))
                    .when_some(zones, |list, zones| {
                        list.on_children_prepainted(move |_, window, cx| {
                            // A tracked list gives its handle the parts' places,
                            // as laid out before it scrolled.
                            let scrolled = list_handle.offset();
                            let parts: Vec<Bounds<Pixels>> = (0..list_handle.children_count())
                                .filter_map(|index| list_handle.bounds_for_item(index))
                                .map(|part| Bounds::new(part.origin + scrolled, part.size))
                                .collect();
                            zones.borrow_mut().measure_list(&parts, pinned_count);
                            let Some((app, hovered)) = &follow_pointer else {
                                return;
                            };
                            let pointer = window.mouse_position();
                            let under = (window.is_window_hovered()
                                && list_handle.bounds().contains(&pointer))
                            .then(|| {
                                parts
                                    .iter()
                                    .zip(&parts_tabs)
                                    .find(|(part, _)| part.contains(&pointer))
                                    .and_then(|(_, tab)| *tab)
                            })
                            .flatten();
                            if under != *hovered {
                                let app = app.clone();
                                // Drawing can't ask for another frame; once it
                                // ends, this does.
                                cx.defer(move |cx| {
                                    let _ = app.update(cx, |this, cx| {
                                        if this.hovered_tab != under {
                                            this.hovered_tab = under;
                                            cx.notify();
                                        }
                                    });
                                });
                            }
                        })
                    })
                    .id(("tabs", space.id))
                    .overflow_y_scroll()
                    // Each space's list keeps its place, and can bring a row
                    // into view (`reveal_selection`).
                    .track_scroll(&lists.tabs)
                    .map(vertical_scroll)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_none()
                            .child(self.render_space_title(
                                space,
                                has_favorites,
                                interactive,
                                theme,
                                cx,
                            )),
                    )
                    .children(pinned_rows)
                    .child(
                        self.render_divider(pinned_count == 0, show_clear, width, theme, cx)
                            .flex_none(),
                    )
                    .child(
                        self.render_new_tab(new_tab_selected, interactive, theme, cx)
                            .flex_none(),
                    )
                    .children(today_rows),
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
        // The whole origin, scheme included, on hover: the pill shows the
        // end of a long host.
        let origin = tab.map(|tab| site_origin(&tab.url).unwrap_or_else(|| tab.url.clone()));
        let insecure = tab.is_some_and(|tab| self.page_insecure(tab.id, &tab.url));
        let blocking_off = host
            .as_deref()
            .is_some_and(|host| self.state.is_blocking_disabled(host));
        let site_menu_open = tab.is_some_and(|tab| {
            self.menu.as_ref().is_some_and(|menu| {
                matches!(menu.kind, MenuKind::Site(id) | MenuKind::ClearSiteData(id) if id == tab.id)
            })
        });
        // Buttons show on hover only (Arc), so at rest the host has the
        // whole pill. The shield, which opens the site menu, stays while
        // blocking is off for the site, something was blocked for it this
        // session, or its menu is open.
        let hovered = interactive && self.url_pill_hovered;
        let copy = tab.filter(|_| hovered).map(|tab| tab.id);
        let site = tab
            .filter(|tab| {
                interactive
                    && host.is_some()
                    && (hovered
                        || blocking_off
                        || site_menu_open
                        || self.site_blocked_something(tab.id))
            })
            .map(|tab| tab.id);
        let zoom = tab
            .filter(|_| interactive)
            .and_then(|tab| self.engine.borrow().zoom_percent(tab.id))
            .filter(|&percent| percent != 100);
        // Pressed while ⌘L edits the address, as "+ New Tab" is for ⌘T.
        let editing = interactive
            && self
                .command_bar
                .as_ref()
                .is_some_and(|bar| bar.mode == CommandMode::EditAddress);
        let buttons = copy.is_some() || site.is_some() || zoom.is_some();
        let (control, control_hover) = (theme.control(), theme.control_hover());
        let hover = theme.hover();
        let pill_button = |id: &'static str, glyph: Icon, tooltip, shortcut| {
            press_only(
                div()
                    .id(id)
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(26.))
                    .rounded(px(6.))
                    .hover(move |style| style.bg(hover))
                    .child(icon(glyph, 17., theme.icon())),
            )
            .maybe_tooltip(self.tooltip(tooltip, shortcut, theme, width))
        };
        let open_site_menu = |tab_id: TabId| {
            cx.listener(move |this: &mut Self, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                this.open_site_menu(MenuKind::Site(tab_id), window, cx);
            })
        };
        div()
            .id(("url-pill", space.id))
            .flex()
            .flex_none()
            .items_center()
            .h(px(36.))
            .mx(px(8.))
            .mt(px(URL_PILL_TOP - PAGES_TOP))
            .pl(px(if insecure && interactive { 7. } else { 13. }))
            .pr(px(if buttons { 5. } else { 13. }))
            .gap(px(2.))
            .rounded(px(10.))
            .map(|pill| {
                if editing {
                    pill.bg(theme.selected())
                        .when(!theme.dark, |pill| pill.shadow(selected_shadow()))
                } else if site_menu_open {
                    pill.bg(control_hover)
                } else {
                    pill.bg(control)
                }
            })
            // Plain HTTP: "Not Secure" before the host, as in Safari and
            // Chrome; with little room, the open lock alone.
            .when_some(
                tab.filter(|_| insecure && interactive).map(|tab| tab.id),
                |pill, tab_id| {
                    let muted = theme.muted();
                    pill.child(
                        press_only(
                            div()
                                .id("not-secure")
                                .flex()
                                .flex_none()
                                .items_center()
                                .gap(px(4.))
                                .h(px(24.))
                                .px(px(6.))
                                .mr(px(4.))
                                .rounded(px(6.))
                                .hover(move |style| style.bg(hover))
                                .text_size(px(12.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(muted)
                                .child(icon(Icon::LockOpen, 13., muted))
                                .when(width >= 220., |chip| chip.child("Not Secure")),
                        )
                        .maybe_tooltip(self.tooltip("Connection Isn't Secure", "", theme, width))
                        .on_click(open_site_menu(tab_id)),
                    )
                },
            )
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
                // The host loses its start, never the domain that owns it.
                Some(label) => pill.child(
                    div()
                        .id("url-host")
                        .flex_1()
                        .min_w_0()
                        .when_some(origin.filter(|_| interactive), |host, origin| {
                            host.maybe_tooltip(
                                self.menu
                                    .is_none()
                                    .then(|| Tooltip::build_wrapping(origin, "", theme, width)),
                            )
                        })
                        .child(
                            elided(label.clone(), Cut::Start, 20.)
                                .text_size(px(13.5))
                                .text_color(theme.url_text()),
                        ),
                ),
                None => pill
                    .gap(px(7.))
                    .child(icon(Icon::Search, 13., theme.tertiary()))
                    .child(
                        // Cut short in a narrow sidebar, never past the pill.
                        truncated("Search or Enter URL…")
                            .flex_1()
                            .min_w_0()
                            .text_size(px(13.5))
                            .text_color(theme.tertiary()),
                    ),
            })
            // The page isn't at 100%: a click resets it.
            .when_some(zoom, |pill, percent| {
                pill.child(
                    press_only(
                        div()
                            .id("zoom")
                            .flex()
                            .flex_none()
                            .items_center()
                            .h(px(22.))
                            .px(px(6.))
                            .rounded(px(6.))
                            .bg(control_hover)
                            .hover(move |style| style.bg(hover))
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted())
                            .child(format!("{percent}%")),
                    )
                    .maybe_tooltip(self.tooltip("Actual Size", "⌘0", theme, width))
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.shortcut(Shortcut::ZoomReset, window, cx);
                    })),
                )
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
            .when_some(site, |pill, tab_id| {
                pill.child(
                    pill_button(
                        "site-menu",
                        if blocking_off {
                            Icon::ShieldOff
                        } else {
                            Icon::Shield
                        },
                        "Site Settings",
                        "",
                    )
                    .when(site_menu_open, |button| button.bg(hover))
                    .on_click(open_site_menu(tab_id)),
                )
            })
    }

    /// Opens a tab's site menu, or its Clear Site Data question, below the
    /// address pill, from the shield or the command bar. A hidden sidebar
    /// peeks while the menu is open (`peek_ends_at`); a page in fullscreen
    /// would cover it.
    pub(crate) fn open_site_menu(
        &mut self,
        kind: MenuKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.fullscreen {
            return;
        }
        if self.sidebar_width().is_none() {
            self.sidebar_peek = true;
            self.sync_traffic_lights();
        }
        // Below the pill, level with its left edge.
        let anchor = point(px(8.), px(URL_PILL_TOP + 40.));
        self.open_menu(kind, anchor, window, cx);
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
        let columns = favorites.len().min(FAVORITE_COLUMNS) as f32;
        let tile_size = size(
            px(((width - 16.) - (columns - 1.) * FAVORITE_GAP) / columns),
            px(FAVORITE_HEIGHT),
        );
        let (control, control_hover) = (theme.control(), theme.control_hover());
        // During a ⌃Tab walk only its target is outlined.
        let walking = self.tab_switch.is_some();
        div()
            .flex()
            .flex_none()
            .flex_wrap()
            .gap(px(FAVORITE_GAP))
            .mx(px(8.))
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
                                .when(!walking, |tile| tile.border_2().border_color(theme.focus()))
                                .shadow(selected_shadow())
                        } else if self.has_open_menu(id) {
                            tile.bg(control_hover)
                        } else {
                            tile.bg(control).hover(move |style| style.bg(control_hover))
                        }
                    })
                    .when(self.is_dragged(id), |tile| tile.opacity(DRAGGED_OPACITY))
                    .child(self.tab_icon(tab, 20., theme))
                    .children(
                        self.render_tab_badges(id, 11., width, theme, cx)
                            .map(|badges| badges.absolute().top(px(3.)).right(px(4.))),
                    )
                    .when(interactive, |tile| {
                        // Its title when it opens: a favorite isn't redrawn
                        // when only its title changes.
                        let tooltip = self.menu.is_none().then(|| {
                            let (app, theme) = (cx.weak_entity(), *theme);
                            move |window: &mut Window, cx: &mut App| {
                                let title = app.upgrade().and_then(|app| {
                                    Some(app.read(cx).state.tab(id)?.shown_title().to_owned())
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
                            // A middle click puts it to sleep, like its ⌘W.
                            .on_mouse_up(
                                MouseButton::Middle,
                                cx.listener(move |this, _, window, cx| {
                                    this.end_rename(window, cx);
                                    this.close_tab(id, cx);
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
                    .text_size(px(TITLE_SIZE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted())
            })
            .when(interactive && !renaming, |row| {
                // The full name, when the title ends in "…".
                let width = self.sidebar_width().unwrap_or(self.state.sidebar_width());
                let room = width - TITLE_CHROME;
                let name_font = Font {
                    weight: FontWeight::SEMIBOLD,
                    ..font(UI_FONT)
                };
                let cut = wrapped_lines(&space.name, name_font, TITLE_SIZE, room, cx) > 1;
                let tooltip = (cut && self.menu.is_none())
                    .then(|| Tooltip::build_wrapping(space.name.clone(), "", theme, width));
                press_only(row)
                    .maybe_tooltip(tooltip)
                    // Arc renames on a double click.
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if event.click_count() == 2 {
                            this.start_rename(id, window, cx);
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.open_space_title_menu(id, event.position, window, cx)
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
                                let anchor = event.position() - point(px(12.), px(0.));
                                this.open_space_title_menu(id, anchor, window, cx)
                            },
                        )),
                    )
            })
    }

    fn render_divider(
        &self,
        after_title: bool,
        show_clear: bool,
        width: f32,
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
                    .maybe_tooltip(self.tooltip("Clear Today", "⇧⌘K", theme, width))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.end_rename(window, cx);
                        this.clear_today(cx)
                    })),
                )
            })
    }

    fn render_new_tab(
        &self,
        selected: bool,
        interactive: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
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
        // A crashed page shows on its tab, as Chrome's sad tab does.
        if self.notices.crashed(tab.id) {
            return icon(Icon::Warning, size, theme.muted()).into_any_element();
        }
        let host = host_of(&tab.url);
        match host.as_deref().and_then(|host| self.favicons.get(host)) {
            Some(image) => img(image).size(px(size)).flex_none().into_any_element(),
            None => placeholder_favicon(host.as_deref(), theme).into_any_element(),
        }
    }

    /// A row's or tile's favicon. A pinned tab or favorite without a page
    /// gets no messages or notifications, and its favicon fades to say so.
    fn tab_icon(&self, tab: &Tab, size: f32, theme: &Theme) -> AnyElement {
        let favicon = self.favicon(tab, size, theme);
        if tab.pin == Pin::Today || self.is_loaded(tab.id) {
            return favicon;
        }
        div()
            .flex()
            .flex_none()
            .opacity(ASLEEP_OPACITY)
            .child(favicon)
            .into_any_element()
    }

    fn is_dragged(&self, tab_id: TabId) -> bool {
        self.tab_drag.is_some_and(|drag| drag.tab == tab_id)
    }

    /// The tab whose menu is open stays highlighted, as in Arc.
    pub(crate) fn has_open_menu(&self, tab_id: TabId) -> bool {
        self.menu.as_ref().is_some_and(
            |menu| matches!(menu.kind, MenuKind::Tab(id) | MenuKind::MoveTab(id) if id == tab_id),
        )
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
            title: tab.shown_title().to_owned().into(),
            favicon: host.as_deref().and_then(|host| self.favicons.get(host)),
            host,
            theme: *theme,
            size,
            tile,
            cursor: point(px(0.), px(0.)),
            over_spaces: self.tab_over_spaces.clone(),
        };
        element.on_drag(DraggedTab(tab.id), move |_, cursor, _, cx| {
            preview.over_spaces.set(false);
            cx.new(|_| TabPreview {
                cursor,
                ..preview.clone()
            })
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
        let renaming = interactive && self.renaming_tab == Some(id);
        let show_close = interactive && self.tab_drag.is_none() && !renaming;
        // Titles use the whole row until the pointer is over it.
        let title_room = show_close && self.hovered_tab == Some(id);
        // Pinned tabs and favorites unload ("–") instead of closing.
        let (glyph, close_label) = if tab.pin == Pin::Today {
            (Icon::Close, "Close Tab")
        } else {
            (Icon::Minus, "Unload Tab")
        };
        let badges = self.render_tab_badges(id, 14., width, theme, cx);
        let title_margin = title_room && badges.is_none();
        let row = div()
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
                } else if self.has_open_menu(id) || renaming {
                    row.bg(hover)
                } else {
                    row.hover(move |style| style.bg(hover))
                }
            })
            .when(self.is_dragged(id), |row| row.opacity(DRAGGED_OPACITY))
            .child(self.tab_icon(tab, 16., theme))
            .child(if renaming {
                div()
                    .flex_1()
                    .min_w_0()
                    .ml(px(11.5))
                    .mr(px(6.))
                    // A press on the field takes keyboard focus back from the
                    // page; AppKit doesn't move it to the shell on a click.
                    .capture_any_mouse_down(
                        cx.listener(|this, _, _, _| this.engine.borrow().focus_shell()),
                    )
                    .child(self.tab_name_input.clone())
            } else {
                truncated(tab.shown_title().to_owned())
                    .flex_1()
                    .min_w_0()
                    .ml(px(11.5))
                    .when(title_margin, |title| title.mr(px(24.)))
                    .text_size(px(14.))
                    .text_color(theme.title())
            })
            .children(badges.map(|badges| {
                badges
                    .ml(px(6.))
                    .when(title_room, |badges| badges.mr(px(24.)))
            }))
            .when(interactive && self.is_switch_target(id), |row| {
                row.child(switch_outline(theme))
            });
        if !interactive || renaming {
            return row;
        }
        // The whole title and the host, after the usual delay, for titles
        // the row cuts short.
        let tooltip = self.menu.is_none().then(|| {
            let (app, theme) = (cx.weak_entity(), *theme);
            move |window: &mut Window, cx: &mut App| {
                let tab = app
                    .upgrade()
                    .and_then(|app| app.read(cx).state.tab(id).cloned());
                TabTooltip::build(tab, &theme, width, window, cx)
            }
        });
        let row_size = size(px(width - 16.), px(ROW_HEIGHT));
        let pin = tab.pin;
        self.draggable(press_only(row), tab, row_size, false, theme)
            .maybe_tooltip(tooltip)
            // A double click names the tab, as in Arc.
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                if event.click_count() >= 2 && pin != Pin::Favorite {
                    this.start_tab_rename(id, window, cx);
                } else {
                    this.select_tab(id, window, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.open_tab_menu(id, event.position, window, cx)
                }),
            )
            // A middle click closes it, as in Chrome and Arc.
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(move |this, _, window, cx| {
                    this.end_rename(window, cx);
                    this.close_tab(id, cx);
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
            // Shown by the row's hover in each frame, not by `hovered_tab`:
            // after a close the next row moves under a still pointer, and a
            // second click must close it too.
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
                    .maybe_tooltip(self.tooltip(close_label, "⌘W", theme, width))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.end_rename(window, cx);
                        this.close_tab(id, cx);
                    })),
                )
            })
    }

    /// A permission request, the toast and, while a page view covers the
    /// card, its load error.
    fn render_notice(&self, width: f32, theme: &Theme, cx: &mut Context<Self>) -> Option<Div> {
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
        let permission = self.render_permission_card(width, theme, cx);
        let toasts = self.render_toasts(width, theme, cx);
        if toasts.is_empty() && error.is_none() && permission.is_none() {
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
                .children(permission)
                .children(toasts)
                .when_some(error, |notice, error| {
                    notice.child(
                        card(div())
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().flex_1().text_color(theme.error()).child(error))
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

    /// The space switcher: a button per space, with its icon or a dot in its
    /// colour, the active one raised like a selected tab. Press one to go
    /// there, drag it along to reorder, or drop a tab on it to move the tab.
    fn render_switcher(&self, width: f32, theme: &Theme, cx: &mut Context<Self>) -> Option<Div> {
        let spaces = &self.state.spaces;
        if spaces.len() < 2 {
            return None;
        }
        let layout = switcher(spaces.len(), width - 2. * SWITCHER_INSET);
        let active_index = spaces
            .iter()
            .position(|space| space.id == self.state.selected_space_id)
            .unwrap_or(0);
        // Too many to show: the run of spaces that ends at the active one,
        // or starts at the first.
        let start = active_index.saturating_sub(layout.shown - 1);
        let (hover, selected) = (theme.hover(), theme.selected());
        let buttons = spaces
            .iter()
            .enumerate()
            .skip(start)
            .take(layout.shown)
            .map(|(index, space)| {
                let id = space.id;
                let active = id == self.state.selected_space_id;
                // The space whose menu is open stays highlighted, like a tab.
                let menu_open = self.menu.as_ref().is_some_and(|menu| {
                    matches!(menu.kind, MenuKind::Space(space) | MenuKind::DeleteSpace(space) if space == id)
                });
                let dot = Theme::new(space.theme(), theme.dark).focus();
                let chip = div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(SPACE_CHIP.min(layout.button)))
                    .rounded(px(6.))
                    .map(|chip| {
                        if active {
                            chip.bg(selected)
                                .when(!theme.dark, |chip| chip.shadow(selected_shadow()))
                        } else {
                            chip.when(menu_open, |chip| chip.bg(hover))
                                .group_hover("space-button", move |style| style.bg(hover))
                                // A tab dropped here moves to this space.
                                .group_drag_over::<DraggedTab>("space-button", |style| {
                                    style.bg(dot.opacity(0.35))
                                })
                        }
                    })
                    .child(space_mark(space.icon.clone(), dot, active));
                let preview = SpacePreview {
                    icon: space.icon.clone(),
                    dot,
                    theme: *theme,
                    width: layout.button,
                };
                let tooltip = self.menu.is_none().then(|| {
                    Tooltip::build_wrapping(space.name.clone(), space_shortcut(index), theme, width)
                });
                press_only(
                    div()
                        .id(("space", id))
                        .group("space-button")
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .w(px(layout.button))
                        .h(px(SPACE_BUTTON))
                        .child(chip),
                )
                .maybe_tooltip(tooltip)
                .on_click(cx.listener(move |this, _, window, cx| this.switch_space(id, window, cx)))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        // Above the button, like the library's and "+"'s.
                        let anchor = event.position - point(px(14.), px(18.));
                        this.open_menu(MenuKind::Space(id), anchor, window, cx)
                    }),
                )
                .on_drag(DraggedSpace(id), move |_, _, _, cx| {
                    cx.new(|_| preview.clone())
                })
                // The dragged space takes the place of the one under the
                // pointer, so the order follows the pointer as it moves.
                .on_drag_move(cx.listener(
                    move |this, event: &DragMoveEvent<DraggedSpace>, _, cx| {
                        let (pointer, bounds) = (event.event.position, event.bounds);
                        let over = pointer.x >= bounds.left()
                            && pointer.x < bounds.right()
                            && (pointer.y - bounds.center().y).abs() < px(BOTTOM_BAR);
                        let dragged = event.drag(cx).0;
                        if over && dragged != id {
                            this.move_space(dragged, id, index, cx);
                        }
                    },
                ))
                .when(!active, |button| {
                    button.on_drop(cx.listener(move |this, dragged: &DraggedTab, _, cx| {
                        this.tab_drag = None;
                        this.tab_over_spaces.set(false);
                        this.leave_tab(dragged.0, Leaving::Move(id), cx);
                    }))
                })
            });
        let more = (layout.shown < spaces.len()).then(|| {
            press_only(
                div()
                    .id("all-spaces")
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .w(px(MORE_BUTTON))
                    .h(px(SPACE_BUTTON))
                    .rounded(px(6.))
                    .hover(move |style| style.bg(hover))
                    .child(icon(Icon::Ellipsis, 16., theme.icon())),
            )
            .maybe_tooltip(self.tooltip("All Spaces", "", theme, width))
            .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                let anchor = event.position() - point(px(14.), px(18.));
                this.open_menu(MenuKind::Spaces, anchor, window, cx)
            }))
        });
        Some(
            div()
                .absolute()
                .left(px(SWITCHER_INSET))
                .right(px(SWITCHER_INSET))
                .top(px(10.))
                .h(px(SPACE_BUTTON))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(layout.gap))
                .overflow_hidden()
                .on_drag_move(
                    cx.listener(|this, event: &DragMoveEvent<DraggedTab>, _, cx| {
                        let over = event.bounds.contains(&event.event.position);
                        if this.tab_over_spaces.replace(over) != over {
                            cx.notify();
                        }
                    }),
                )
                .children(buttons)
                .children(more),
        )
    }

    fn render_bottom_bar(&self, width: f32, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let spaces = self.render_switcher(width, theme, cx);
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
                    .maybe_tooltip(self.menu.is_none().then(|| {
                        Tooltip::build_wrapping(
                            "Library — Downloads and Recently Closed",
                            "",
                            theme,
                            width,
                        )
                    }))
                    .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                        let anchor = event.position() - point(px(14.), px(18.));
                        this.open_menu(MenuKind::Library, anchor, window, cx)
                    })),
            )
            .children(self.render_download_badge(theme))
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

/// A tab row's tooltip: the whole title, which the row cuts short, and the
/// host, styled and placed like `Tooltip`.
struct TabTooltip {
    title: SharedString,
    host: SharedString,
    theme: Theme,
    /// The sidebar's width.
    width: f32,
    /// The pointer's x when the tooltip opened.
    pointer: f32,
    /// The lines the title takes, and the width it wraps at.
    lines: usize,
    room: f32,
}

impl TabTooltip {
    /// Measured when it opens, with the row's title as it is then. A title
    /// that fits its row gets an empty tooltip.
    fn build(
        tab: Option<Tab>,
        theme: &Theme,
        width: f32,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyView {
        let Some(tab) = tab else {
            return cx.new(|_| EmptyView).into();
        };
        let title = tab.shown_title().to_owned();
        if text_width(&title, font(UI_FONT), 14., window) <= width - ROW_TITLE_CHROME {
            return cx.new(|_| EmptyView).into();
        }
        // At the tooltip's 12 pt, in the bubble less its padding.
        let room = TAB_TOOLTIP_MAX_WIDTH.min(width - 2. * TOOLTIP_EDGE) - 2. * TOOLTIP_PADDING;
        let lines = wrapped_lines(&title, font(UI_FONT), 12., room, cx).min(TAB_TOOLTIP_LINES);
        let host = host_of(&tab.url).unwrap_or(tab.url);
        let (theme, pointer) = (*theme, f32::from(window.mouse_position().x));
        cx.new(|_| Self {
            title: title.into(),
            host: host.into(),
            theme,
            width,
            pointer,
            lines,
            room,
        })
        .into()
    }
}

impl Render for TabTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        // Padding and border, the title's lines and the host's.
        let height = 2. * 5. + 2. + 16. * self.lines as f32 + 15.;
        let bubble = tooltip_bubble(theme)
            .flex_col()
            .max_w(px(TAB_TOOLTIP_MAX_WIDTH.min(self.width - 2. * TOOLTIP_EDGE)))
            .py(px(5.))
            .child(
                div()
                    // A title that wraps does so where it was measured.
                    .when(self.lines > 1, |title| title.w(px(self.room)))
                    .line_clamp(TAB_TOOLTIP_LINES)
                    .text_ellipsis()
                    .text_size(px(12.))
                    .line_height(px(16.))
                    .text_color(theme.title())
                    .child(self.title.clone()),
            )
            .child(
                truncated(self.host.clone())
                    .text_size(px(11.))
                    .line_height(px(15.))
                    .text_color(theme.tertiary()),
            );
        beside_pointer(bubble, self.pointer, self.width, height)
    }
}
