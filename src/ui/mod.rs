//! The browser window: one theme gradient behind the sidebar and the page
//! card, Arc's layout (arc-design.md §17). Behaviour lives in `app`.

pub mod command_bar;
mod content;
pub mod icons;
pub mod menu;
mod sidebar;
pub mod theme;

use crate::{Dismiss, app::BrowserApp};
use gpui::{
    AnyView, App, AppContext, Bounds, BoxShadow, Context, Div, FontWeight, Hsla,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, Render, SharedString,
    Styled, Window, div, point, px, size,
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
    /// The sidebar's width, unless it is hidden or a page is fullscreen.
    pub fn sidebar_width(&self) -> Option<f32> {
        (!self.state.sidebar_hidden && !self.fullscreen).then(|| self.state.sidebar_width())
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
        let viewport = window.viewport_size();
        let (frame, slide_finished) = self.slide_frame(window);
        let theme = match (frame, &self.slide) {
            (Some(frame), Some(slide)) => {
                Theme::new(slide.from.theme(), self.dark).lerp(&self.theme(), frame.progress)
            }
            _ => self.theme(),
        };
        let card = self.card_bounds(viewport);
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
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.close_menu(cx)))
            // The whole window background is one gradient quad.
            .bg(theme.background())
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

/// Small text tooltips. They are kept narrow, as anything drawn over the
/// card is hidden behind the page on macOS.
pub struct Tooltip {
    text: SharedString,
    background: Hsla,
    color: Hsla,
}

impl Tooltip {
    pub fn view(text: impl Into<SharedString>, theme: &Theme, cx: &mut App) -> AnyView {
        let (text, background, color) = (text.into(), theme.surface(), theme.title());
        cx.new(|_| Self {
            text,
            background,
            color,
        })
        .into()
    }
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .font_family(UI_FONT)
            .mt(px(6.))
            .px(px(8.))
            .py(px(4.))
            .rounded(px(6.))
            .bg(self.background)
            .border_1()
            .border_color(gpui::black().opacity(0.1))
            .shadow(vec![shadow(2., 8., 0.12)])
            .max_w(px(180.))
            .text_size(px(12.))
            .text_color(self.color)
            .child(truncated(self.text.clone()))
    }
}
