//! Find in page (⌘F): a small bar at the top right of the page card, in a
//! pop-up window of its own like the command bar, because the native web
//! views cover anything GPUI draws in the browser window. It stays open
//! while the page is used, follows the card, and closes when the tab
//! changes or its page goes to another document. As in Chrome, changes
//! within the document keep it: fragments, and pages rewriting their
//! address with the History API (infinite scroll, filters, maps).

use super::{
    UI_FONT,
    command_bar::{self, TRANSPARENT},
    icons::{Icon, icon},
    shadow,
    theme::Theme,
};
use crate::{
    app::BrowserApp,
    input::{InputEvent, InputStyle, TextInput},
    model::TabId,
};
use gpui::{
    App, AppContext, Bounds, Context, Entity, EventEmitter, InteractiveElement, IntoElement,
    KeyBinding, ParentElement, Pixels, Point, Render, SharedString, Stateful,
    StatefulInteractiveElement, Styled, Subscription, Window, WindowHandle, actions, div, point,
    prelude::FluentBuilder, px, size,
};

actions!(find_bar, [FindPrevious]);

const PANEL_WIDTH: f32 = 320.;
const PANEL_HEIGHT: f32 = 40.;
/// The panel's distance from the card's top and right edges.
const INSET: f32 = 10.;
/// Transparent room around the panel for its shadow, kept small: the margin
/// takes clicks meant for the page under it. X11 previews have no
/// compositor, so there the window is exactly the panel.
const MARGIN: f32 = if TRANSPARENT { 14. } else { 0. };

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "shift-enter",
        FindPrevious,
        Some("FindBar"),
    )]);
}

/// The bar window's top-left corner in the browser window, for `card`.
fn window_origin(card: Bounds<Pixels>) -> Point<Pixels> {
    point(
        card.right() - px(INSET + PANEL_WIDTH + MARGIN),
        card.top() + px(INSET - MARGIN),
    )
}

/// Screen bounds of the bar's window over `card` (window coordinates) of a
/// browser window at `origin`.
fn window_bounds(card: Bounds<Pixels>, origin: Point<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        origin + window_origin(card),
        size(
            px(PANEL_WIDTH + 2. * MARGIN),
            px(PANEL_HEIGHT + 2. * MARGIN),
        ),
    )
}

/// "3 of 12" for the selected match, once a search has results.
fn result_label(count: u32, active: u32) -> SharedString {
    if count == 0 {
        "No matches".into()
    } else {
        format!("{} of {count}", active.clamp(1, count)).into()
    }
}

pub enum FindBarEvent {
    /// The text to find changed; empty ends the search.
    Query(String),
    /// The next match, or the previous one unless `forward`.
    Step {
        forward: bool,
    },
    Close,
}

pub struct FindBar {
    input: Entity<TextInput>,
    theme: Theme,
    /// The latest count and selected match, while there is text to find.
    result: Option<(u32, u32)>,
    _subscription: Subscription,
}

impl EventEmitter<FindBarEvent> for FindBar {}

impl FindBar {
    fn new(query: &str, theme: Theme, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            let mut input = TextInput::new(window, cx);
            input.set_placeholder("Find in Page", cx);
            input.set_style(
                InputStyle {
                    text: theme.title(),
                    placeholder: theme.tertiary(),
                    cursor: theme.title(),
                    selection: theme.focus().opacity(0.3),
                    font_size: px(13.5),
                    line_height: px(18.),
                },
                cx,
            );
            input.set_text_and_select_all(query, window, cx);
            input
        });
        let subscription = cx.subscribe(&input, |this, _, event, cx| match event {
            InputEvent::Changed(query) => {
                if query.is_empty() {
                    this.result = None;
                    cx.notify();
                }
                cx.emit(FindBarEvent::Query(query.clone()));
            }
            InputEvent::Submit(_) => cx.emit(FindBarEvent::Step { forward: true }),
            InputEvent::Cancel => cx.emit(FindBarEvent::Close),
        });
        Self {
            input,
            theme,
            result: None,
            _subscription: subscription,
        }
    }

    fn set_result(&mut self, count: u32, active: u32, cx: &mut Context<Self>) {
        if self.result != Some((count, active)) && !self.input.read(cx).text().is_empty() {
            self.result = Some((count, active));
            cx.notify();
        }
    }

    fn render_button(
        &self,
        id: &'static str,
        glyph: Icon,
        enabled: bool,
        event: impl Fn() -> FindBarEvent + 'static,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        let hover = self.theme.hover();
        div()
            .id(id)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(26.))
            .rounded(px(6.))
            .when(enabled, |button| {
                button
                    .hover(move |style| style.bg(hover))
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(event())))
            })
            .child(icon(
                glyph,
                16.,
                if enabled {
                    self.theme.icon()
                } else {
                    self.theme.disabled()
                },
            ))
    }
}

impl Render for FindBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let matches = self.result.is_some_and(|(count, _)| count > 0);
        let panel = div()
            .absolute()
            .left(px(MARGIN))
            .top(px(MARGIN))
            .w(px(PANEL_WIDTH))
            .h(px(PANEL_HEIGHT))
            .flex()
            .items_center()
            .gap(px(2.))
            .pl(px(12.))
            .pr(px(6.))
            .rounded(px(10.))
            .bg(theme.surface())
            .when(TRANSPARENT, |panel| {
                panel.shadow(vec![shadow(4., 14., 0.16), shadow(0., 1., 0.12)])
            })
            // A hairline keeps the panel apart from a page of the same tone.
            .border_1()
            .border_color(theme.ink(0.12))
            .child(icon(Icon::Search, 14., theme.ink(0.45)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .ml(px(8.))
                    .child(self.input.clone()),
            )
            .when_some(self.result, |panel, (count, active)| {
                panel.child(
                    div()
                        .flex_none()
                        .mx(px(6.))
                        .text_size(px(12.))
                        .text_color(theme.tertiary())
                        .child(result_label(count, active)),
                )
            })
            .child(
                div()
                    .flex_none()
                    .w(px(1.))
                    .h(px(18.))
                    .mx(px(2.))
                    .bg(theme.divider()),
            )
            .child(self.render_button(
                "previous",
                Icon::ChevronUp,
                matches,
                || FindBarEvent::Step { forward: false },
                cx,
            ))
            .child(self.render_button(
                "next",
                Icon::ChevronDown,
                matches,
                || FindBarEvent::Step { forward: true },
                cx,
            ))
            .child(self.render_button("close", Icon::Close, true, || FindBarEvent::Close, cx));
        div()
            .size_full()
            .relative()
            .font_family(UI_FONT)
            .key_context("FindBar")
            .on_action(cx.listener(|_, _: &FindPrevious, _, cx| {
                cx.emit(FindBarEvent::Step { forward: false })
            }))
            .when(!TRANSPARENT, |root| root.bg(theme.surface()))
            .child(panel)
    }
}

/// The find bar of the browser window, and the last text searched for,
/// which the bar starts with next time, as in Safari and Chrome.
#[derive(Default)]
pub struct Find {
    bar: Option<FindBarWindow>,
    query: String,
}

struct FindBarWindow {
    tab_id: TabId,
    handle: WindowHandle<FindBar>,
    bar: Entity<FindBar>,
    /// The card the window was placed over.
    card: Bounds<Pixels>,
    /// The pop-up's NSView, attached to the browser window as a child.
    #[cfg(target_os = "macos")]
    native_view: Option<*mut std::ffi::c_void>,
    _subscription: Subscription,
}

impl BrowserApp {
    /// ⌘F: opens the find bar over the active page, or puts the cursor back
    /// in it.
    pub fn find_in_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab_id) = self.active_tab_id().filter(|&id| self.is_loaded(id)) else {
            return;
        };
        if let Some(bar) = self.find.bar.as_ref().filter(|bar| bar.tab_id == tab_id) {
            let input = bar.bar.read(cx).input.clone();
            let _ = bar.handle.update(cx, |_, window, cx| {
                window.activate_window();
                input.update(cx, |input, cx| input.select_all_focus(window, cx));
            });
            return;
        }
        self.close_find_bar(cx);
        let card = self.card_bounds(window.viewport_size());
        let bounds = window_bounds(card, window.bounds().origin);
        let display = window.display(cx).map(|display| display.id());
        let (query, theme) = (self.find.query.clone(), self.theme());
        let mut bar = None;
        let opened = cx.open_window(
            command_bar::window_options(bounds, display),
            |window, cx| {
                let entity = cx.new(|cx| FindBar::new(&query, theme, window, cx));
                bar = Some(entity.clone());
                entity
            },
        );
        let (Ok(handle), Some(bar)) = (opened, bar) else {
            eprintln!("Couldn't open the find bar");
            return;
        };
        #[cfg(target_os = "macos")]
        let native_view = handle
            .update(cx, |_, window, _| crate::native::ns_view(window))
            .ok()
            .flatten();
        #[cfg(target_os = "macos")]
        if let Some(view) = native_view {
            self.engine.borrow().attach_child_window(view, true);
        }
        let subscription = cx.subscribe_in(&bar, window, |this, _, event, window, cx| {
            this.find_bar_event(event, window, cx)
        });
        self.find.bar = Some(FindBarWindow {
            tab_id,
            handle,
            bar,
            card,
            #[cfg(target_os = "macos")]
            native_view,
            _subscription: subscription,
        });
        if !query.is_empty() {
            self.engine.borrow().find(tab_id, &query, true, false);
        }
    }

    /// ⌘G and ⇧⌘G: the next or previous match; the bar opens first if
    /// it is closed.
    pub fn find_again(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        match &self.find.bar {
            Some(bar) if !self.find.query.is_empty() => {
                self.engine
                    .borrow()
                    .find(bar.tab_id, &self.find.query, forward, true);
            }
            Some(_) => {}
            None => self.find_in_page(window, cx),
        }
    }

    pub fn find_result(&mut self, tab_id: TabId, count: u32, active: u32, cx: &mut Context<Self>) {
        if let Some(bar) = self.find.bar.as_ref().filter(|bar| bar.tab_id == tab_id) {
            bar.bar
                .update(cx, |bar, cx| bar.set_result(count, active, cx));
        }
    }

    /// Closes the bar, if open, and clears the page's highlights.
    fn close_find_bar(&mut self, cx: &mut Context<Self>) {
        let Some(bar) = self.find.bar.take() else {
            return;
        };
        if self.is_loaded(bar.tab_id) {
            self.engine.borrow().stop_finding(bar.tab_id);
        }
        #[cfg(target_os = "macos")]
        if let Some(view) = bar.native_view {
            self.engine.borrow().detach_child_window(view);
        }
        let _ = bar.handle.update(cx, |_, window, _| window.remove_window());
    }

    /// The tab's page went to another document (or an error page): the
    /// matches are gone, and so is its bar.
    pub(crate) fn page_replaced(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if self
            .find
            .bar
            .as_ref()
            .is_some_and(|bar| bar.tab_id == tab_id)
        {
            self.close_find_bar(cx);
        }
    }

    /// Called on every frame of the browser window: the bar follows the
    /// card, and goes when its tab is no longer on screen.
    pub(super) fn sync_find_bar(
        &mut self,
        card: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(bar) = &self.find.bar else {
            return;
        };
        if self.active_tab_id() != Some(bar.tab_id) || !self.is_loaded(bar.tab_id) {
            cx.defer_in(window, |this, _, cx| this.close_find_bar(cx));
            return;
        }
        if bar.card == card {
            return;
        }
        #[cfg(target_os = "macos")]
        if let Some(view) = bar.native_view {
            let origin = window_origin(card);
            self.engine
                .borrow()
                .place_child_window(view, f64::from(origin.x), f64::from(origin.y));
        }
        if let Some(bar) = &mut self.find.bar {
            bar.card = card;
        }
    }

    fn find_bar_event(
        &mut self,
        event: &FindBarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_id) = self.find.bar.as_ref().map(|bar| bar.tab_id) else {
            return;
        };
        match event {
            FindBarEvent::Query(query) => {
                self.find.query.clone_from(query);
                if query.is_empty() {
                    self.engine.borrow().stop_finding(tab_id);
                } else {
                    self.engine.borrow().find(tab_id, query, true, false);
                }
            }
            FindBarEvent::Step { forward } => self.find_again(*forward, window, cx),
            FindBarEvent::Close => {
                self.close_find_bar(cx);
                window.activate_window();
                self.focus_page();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_sits_at_the_cards_top_right() {
        let card = Bounds::new(point(px(238.), px(10.)), size(px(1032.), px(800.)));
        let bounds = window_bounds(card, point(px(100.), px(50.)));
        let panel_right = f32::from(bounds.right()) - MARGIN;
        let panel_top = f32::from(bounds.top()) + MARGIN;
        assert_eq!(panel_right, 100. + 238. + 1032. - INSET);
        assert_eq!(panel_top, 50. + 10. + INSET);
        assert_eq!(f32::from(bounds.size.width), PANEL_WIDTH + 2. * MARGIN);
    }

    #[test]
    fn labels_count_matches() {
        assert_eq!(result_label(12, 3), "3 of 12");
        assert_eq!(result_label(2, 0), "1 of 2");
        assert_eq!(result_label(0, 0), "No matches");
    }
}
