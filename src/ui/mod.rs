//! The browser window: one theme gradient behind the sidebar and the page
//! card, Arc's layout (arc-design.md §17). Behaviour lives in `app`.

pub mod command_bar;
mod content;
pub mod downloads;
pub mod drag;
pub mod find_bar;
pub mod icons;
pub mod menu;
pub mod permissions;
pub mod redraw;
mod sidebar;
pub mod swipe;
pub mod theme;
pub mod window_frame;

use crate::{Dismiss, app::BrowserApp};
use drag::DraggedEdge;
use gpui::{
    AnyView, App, AppContext, Bounds, BoxShadow, Context, Div, DragMoveEvent, FontWeight,
    InteractiveElement, IntoElement, ModifiersChangedEvent, MouseButton, ParentElement, Pixels,
    Render, SharedString, StatefulInteractiveElement, Styled, Window, div, point,
    prelude::FluentBuilder, px, size,
};
use theme::Theme;

#[cfg(target_os = "macos")]
pub const UI_FONT: &str = ".AppleSystemUIFont";
/// Previews use Inter, the closest open font to SF Pro.
#[cfg(not(target_os = "macos"))]
pub const UI_FONT: &str = "Inter";

/// The card's distance from the window edges (and the sidebar).
pub const CARD_INSET: f32 = 10.;
pub const CARD_RADIUS: f32 = 7.;
const SLIDE_MS: f32 = 260.;

/// One frame of a space switch.
#[derive(Clone, Copy)]
pub struct SlideFrame {
    /// Eased progress, 0 to 1.
    pub progress: f32,
    pub direction: f32,
}

impl BrowserApp {
    /// The sidebar's width, unless it is hidden or a page is fullscreen. A
    /// hidden sidebar shows while the page on screen asks for a permission:
    /// the question is in it.
    pub fn sidebar_width(&self) -> Option<f32> {
        let shown = !self.state.sidebar_hidden || self.permission_shown().is_some();
        (shown && !self.fullscreen).then(|| self.state.sidebar_width())
    }

    /// The page card in window coordinates; native web views cover exactly it.
    pub fn card_bounds(&self, viewport: gpui::Size<Pixels>) -> Bounds<Pixels> {
        if self.fullscreen {
            return Bounds::new(point(px(0.), px(0.)), viewport);
        }
        let left = self.sidebar_width().unwrap_or(CARD_INSET);
        Bounds::new(
            point(px(left), px(CARD_INSET)),
            size(
                (viewport.width - px(left + CARD_INSET)).max(px(1.)),
                (viewport.height - px(2. * CARD_INSET)).max(px(1.)),
            ),
        )
    }

    /// A sidebar control's tooltip, but none while a menu is open: GPUI
    /// would show a tooltip still waiting to appear over the menu.
    fn tooltip(
        &self,
        text: impl Into<SharedString>,
        shortcut: &'static str,
        theme: &Theme,
        width: f32,
    ) -> Option<impl Fn(&mut Window, &mut App) -> AnyView + 'static> {
        self.menu
            .is_none()
            .then(|| Tooltip::build(text, shortcut, theme, width))
    }

    /// Advances the space switch. Frames are requested only while it runs.
    fn slide_frame(&mut self, window: &mut Window) -> (Option<SlideFrame>, bool) {
        let Some(slide) = &mut self.slide else {
            return (None, false);
        };
        let elapsed_ms = slide.start.elapsed().as_secs_f32() * 1000.;
        slide.frames.push(elapsed_ms);
        let linear = (elapsed_ms / SLIDE_MS).min(1.);
        if linear < 1. {
            window.request_animation_frame();
            return (
                Some(SlideFrame {
                    progress: 1. - (1. - linear).powi(3),
                    direction: slide.direction,
                }),
                false,
            );
        }
        if std::env::var_os("MAPBROWSER_TRACE_ANIMATION").is_some() {
            eprintln!(
                "ANIMATION {}",
                serde_json::json!({
                    "from": slide.from.id,
                    "to": self.state.selected_space_id,
                    "elapsed_ms": elapsed_ms,
                    "frames": slide.frames,
                })
            );
        }
        self.slide = None;
        (None, true)
    }
}

impl Render for BrowserApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A tab drag that ended outside the sidebar leaves nothing behind.
        if !cx.has_active_drag() {
            self.tab_drag = None;
        }
        let viewport = window.viewport_size();
        let (frame, slide_finished) = self.slide_frame(window);
        let theme = match (frame, &self.slide) {
            (Some(frame), Some(slide)) => {
                Theme::new(slide.from.theme(), self.dark).lerp(&self.theme(), frame.progress)
            }
            _ => self.theme(),
        };
        let card = self.card_bounds(viewport);
        self.sync_find_bar(card, window, cx);
        let sidebar = self
            .sidebar_width()
            .map(|width| self.render_sidebar(width, &theme, frame, cx));
        let menu = self
            .menu
            .as_ref()
            .map(|menu| self.render_menu(menu, &theme, viewport, cx));
        div()
            .size_full()
            .relative()
            .font_family(UI_FONT)
            .key_context("Browser")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.dismiss(cx)))
            .on_modifiers_changed(
                cx.listener(|this, event: &ModifiersChangedEvent, window, cx| {
                    if !event.modifiers.control {
                        this.finish_tab_switch(window, cx);
                    }
                }),
            )
            // The sidebar's edge may be dragged anywhere, even with the sidebar hidden.
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DraggedEdge>, _, cx| {
                    let start_width = event.drag(cx).start_width;
                    this.drag_sidebar_edge(f32::from(event.event.position.x), start_width, cx)
                }),
            )
            // The whole window background is one gradient quad.
            .bg(theme.background())
            .children(self.render_card_shadow(card))
            .child(self.render_card(card, &theme, frame, slide_finished, viewport))
            .children(self.render_progress(card, &theme))
            .children(sidebar)
            .children(menu)
    }
}

/// A black drop shadow, specified like CSS's `0 y blur rgba(0,0,0,alpha)`.
/// GPUI takes the blur radius as the Gaussian's sigma, CSS as twice that.
pub fn shadow(y: f32, blur: f32, alpha: f32) -> BoxShadow {
    BoxShadow {
        color: gpui::black().opacity(alpha),
        offset: point(px(0.), px(y)),
        blur_radius: px(blur / 2.),
        spread_radius: px(0.),
    }
}

/// `tooltip`, for tooltips that may be withheld (`BrowserApp::tooltip`).
pub trait MaybeTooltip: StatefulInteractiveElement {
    fn maybe_tooltip(
        self,
        tooltip: Option<impl Fn(&mut Window, &mut App) -> AnyView + 'static>,
    ) -> Self {
        match tooltip {
            Some(tooltip) => self.tooltip(tooltip),
            None => self,
        }
    }
}

impl<E: StatefulInteractiveElement> MaybeTooltip for E {}

/// Keeps a press on a control from reaching the sidebar, where it would
/// start a window drag.
pub fn press_only<E: InteractiveElement>(element: E) -> E {
    element.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// One line of text that ends in "…" when it doesn't fit. GPUI 0.2.2 keeps
/// a text's first measurement unless its wrap width changes, and flex layout
/// first measures without a width limit; a one-line clamp of wrapping text
/// is measured again at its final width.
pub fn truncated(text: impl Into<SharedString>) -> Div {
    div().line_clamp(1).text_ellipsis().child(text.into())
}

/// A rounded square with the host's initial, for pages without a favicon.
pub fn placeholder_favicon(host: Option<&str>, theme: &Theme) -> Div {
    let initial = host
        .and_then(|host| host.chars().find(|ch| ch.is_alphanumeric()))
        .map(|ch| ch.to_uppercase().to_string())
        .unwrap_or_default();
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(16.))
        .rounded(px(4.))
        .bg(theme.ink(if theme.dark { 0.14 } else { 0.12 }))
        .text_size(px(9.))
        .line_height(px(10.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.icon())
        .child(initial)
}

/// Gap between the pointer and a tooltip, a tooltip's height, and the room
/// it keeps from the sidebar's edges.
const TOOLTIP_GAP: f32 = 6.;
const TOOLTIP_HEIGHT: f32 = 26.;
const TOOLTIP_EDGE: f32 = 6.;
const TOOLTIP_MAX_WIDTH: f32 = 180.;

/// Small text tooltips with the control's shortcut, like Arc's. Anything
/// drawn over the card is hidden behind the page on macOS, so they stay
/// inside the sidebar: in its right half they open leftwards, and either
/// way they move along as far as they must to fit.
pub struct Tooltip {
    text: SharedString,
    shortcut: &'static str,
    theme: Theme,
    /// The sidebar's width.
    width: f32,
    /// The pointer's x when the tooltip opened.
    pointer: f32,
}

impl Tooltip {
    /// A tooltip for a control in the sidebar, which is `width` wide.
    pub fn build(
        text: impl Into<SharedString>,
        shortcut: &'static str,
        theme: &Theme,
        width: f32,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        let (text, theme) = (text.into(), *theme);
        move |window, cx| {
            let pointer = f32::from(window.mouse_position().x);
            let text = text.clone();
            cx.new(|_| Self {
                text,
                shortcut,
                theme,
                width,
                pointer,
            })
            .into()
        }
    }
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let span = (self.width - 2. * TOOLTIP_EDGE).max(0.);
        let bubble = div()
            .font_family(UI_FONT)
            .flex()
            .gap(px(6.))
            .h(px(TOOLTIP_HEIGHT))
            .items_center()
            .px(px(8.))
            .rounded(px(6.))
            .bg(theme.surface())
            .border_1()
            .border_color(gpui::black().opacity(0.1))
            .shadow(vec![shadow(2., 8., 0.12)])
            .flex_none()
            .max_w(px(TOOLTIP_MAX_WIDTH.min(span)))
            .text_size(px(12.))
            .text_color(theme.title())
            .child(truncated(self.text.clone()).min_w_0())
            .when(!self.shortcut.is_empty(), |bubble| {
                bubble.child(
                    div()
                        .flex_none()
                        .text_color(theme.tertiary())
                        .child(self.shortcut),
                )
            });
        // GPUI puts a tooltip's top-left 1 pt right of the pointer. With no
        // width of its own GPUI never flips it; the bubble sits in a row
        // across the sidebar instead, beside the pointer, where a spacer
        // that shrinks first keeps it from running past either edge.
        let origin = self.pointer + 1.;
        let spacer = |width: f32| div().min_w_0().w(px(width.max(0.)));
        let row = div()
            .absolute()
            .top(px(TOOLTIP_GAP))
            .left(px(TOOLTIP_EDGE - origin))
            .w(px(span))
            .flex();
        let row = if self.pointer > self.width / 2. {
            row.justify_end()
                .child(bubble)
                .child(spacer(self.width - TOOLTIP_EDGE - self.pointer))
        } else {
            row.child(spacer(origin - TOOLTIP_EDGE)).child(bubble)
        };
        div().w_0().h(px(TOOLTIP_GAP + TOOLTIP_HEIGHT)).child(row)
    }
}
