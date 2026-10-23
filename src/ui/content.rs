//! The page card. Chromium's native views cover it while a page is loaded;
//! GPUI draws its background, empty states and the load progress around it.

use super::{CARD_RADIUS, SlideFrame, shadow, theme::Theme, theme::darkened_argb, truncated};
use crate::app::BrowserApp;
use gpui::{
    Bounds, BoxShadow, Div, ParentElement, Pixels, SharedString, Size, Styled, canvas, div, point,
    prelude::FluentBuilder, px,
};

/// How dark the card's shadow makes the window right outside its top and
/// bottom corners. The native corner caps match it there.
const CAP_SHADOW_TOP: f32 = 0.04;
const CAP_SHADOW_BOTTOM: f32 = 0.07;
/// How far the card's shadow reaches past it: three blur sigmas (5 pt)
/// plus the offset (2 pt), and some room.
const SHADOW_REACH: f32 = 20.;

fn card_shadow() -> Vec<BoxShadow> {
    vec![shadow(2., 10., 0.14), shadow(0., 1., 0.10)]
}

/// Chromium's network error codes in plain words.
fn error_message(error: &str) -> String {
    match error {
        "net::ERR_NAME_NOT_RESOLVED" => "This address couldn't be found.".into(),
        "net::ERR_INTERNET_DISCONNECTED" => "You're offline. Check your connection.".into(),
        "net::ERR_CONNECTION_REFUSED" => "The site refused the connection.".into(),
        "net::ERR_CONNECTION_TIMED_OUT" | "net::ERR_TIMED_OUT" => {
            "The site took too long to respond.".into()
        }
        error if error.starts_with("net::ERR_CERT_") => {
            "The site's certificate couldn't be verified.".into()
        }
        _ => error.to_owned(),
    }
}

impl BrowserApp {
    /// Whether a native page view covers the card, hiding GPUI's drawing.
    pub(super) fn page_covers_card(&self) -> bool {
        cfg!(target_os = "macos") && self.active_tab_id().is_some_and(|id| self.is_loaded(id))
    }

    pub(super) fn error_text(&self) -> Option<SharedString> {
        self.error
            .as_deref()
            .map(|error| error_message(error).into())
    }

    pub(super) fn render_card(
        &mut self,
        card: Bounds<Pixels>,
        theme: &Theme,
        frame: Option<SlideFrame>,
        slide_finished: bool,
        viewport: Size<Pixels>,
    ) -> Div {
        let radius = if self.fullscreen { 0. } else { CARD_RADIUS };
        let active = self.state.active_tab();
        let selected = active.map_or(0, |tab| tab.id);
        let loaded = active.is_some_and(|tab| self.is_loaded(tab.id));
        // Native views slide between the outgoing and incoming space's tabs.
        let transition = frame.zip(self.slide.as_ref()).map(|(frame, slide)| {
            let from = slide.from.selected_tab_id.unwrap_or(0);
            (from, selected, frame.progress, frame.direction as i32)
        });
        let corner = |x: Pixels, y: Pixels, shadow: f32| {
            darkened_argb(theme.background_at(point(x, y), viewport), shadow)
        };
        let caps = [
            corner(card.left(), card.top(), CAP_SHADOW_TOP),
            corner(card.right(), card.top(), CAP_SHADOW_TOP),
            corner(card.left(), card.bottom(), CAP_SHADOW_BOTTOM),
            corner(card.right(), card.bottom(), CAP_SHADOW_BOTTOM),
        ];
        let engine = self.engine.clone();
        let native_frame = canvas(
            move |bounds, _, _| {
                let mut engine = engine.borrow_mut();
                engine.set_bounds(
                    f64::from(bounds.origin.x),
                    f64::from(bounds.origin.y),
                    f64::from(bounds.size.width),
                    f64::from(bounds.size.height),
                );
                engine.set_corner_caps(f64::from(radius), caps);
                if let Some((from, to, progress, direction)) = transition {
                    engine.set_transition(from, to, progress, direction);
                } else if slide_finished {
                    engine.activate_tab(selected);
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        let error = self.error_text().filter(|_| !self.page_covers_card());
        div()
            .absolute()
            .left(card.origin.x)
            .top(card.origin.y)
            .w(card.size.width)
            .h(card.size.height)
            .rounded(px(radius))
            .overflow_hidden()
            .bg(if loaded {
                theme.card()
            } else {
                theme.empty_card()
            })
            .child(native_frame)
            .child(match active {
                // With the sidebar hidden, nothing else on screen says how
                // to get it back.
                None if self.state.sidebar_hidden => hint(
                    theme,
                    "Press ⌘T to open a tab",
                    Some("⌘S shows the sidebar"),
                ),
                None => hint(theme, "Press ⌘T to open a tab", None),
                // The selected tab without a page: closed by ⌘W or its –
                // (pinned tabs and favorites), or by the page itself.
                Some(tab) if !loaded => hint(
                    theme,
                    "This tab is asleep. Press ⌘R or click it to load it.",
                    Some(tab.shown_title()),
                ),
                // Without Chromium (Linux previews) say so instead of a page.
                Some(tab) if cfg!(not(target_os = "macos")) => hint(
                    theme,
                    "Page preview unavailable on this platform",
                    Some(&tab.url),
                ),
                Some(_) => div(),
            })
            .when_some(error, |card, error| {
                card.child(
                    div()
                        .absolute()
                        .left(px(16.))
                        .right(px(16.))
                        .bottom(px(16.))
                        .px(px(14.))
                        .py(px(10.))
                        .rounded(px(8.))
                        .bg(theme.surface())
                        .border_1()
                        .border_color(theme.ink(0.1))
                        .text_size(px(12.5))
                        .text_color(theme.error())
                        .child(error),
                )
            })
    }

    /// The card's shadow, drawn under it in four strips along its edges.
    /// GPUI shades a shadow over its whole area, and the opaque card hides
    /// nearly all of it; clipped to the strips, that part is never shaded.
    /// The strips don't overlap, so no part of the shadow is drawn twice.
    pub(super) fn render_card_shadow(&self, card: Bounds<Pixels>) -> Vec<Div> {
        if self.fullscreen {
            return Vec::new();
        }
        let (reach, radius) = (px(SHADOW_REACH), px(CARD_RADIUS));
        let (left, top, right, bottom) = (card.left(), card.top(), card.right(), card.bottom());
        [
            (
                point(left - reach, top - reach),
                point(right + reach, top + radius),
            ),
            (
                point(left - reach, bottom - radius),
                point(right + reach, bottom + reach),
            ),
            (
                point(left - reach, top + radius),
                point(left + radius, bottom - radius),
            ),
            (
                point(right - radius, top + radius),
                point(right + reach, bottom - radius),
            ),
        ]
        .into_iter()
        .map(|(start, end)| {
            let strip = Bounds::from_corners(start, end);
            div()
                .absolute()
                .left(strip.left())
                .top(strip.top())
                .w(strip.size.width)
                .h(strip.size.height)
                .overflow_hidden()
                .child(
                    div()
                        .absolute()
                        .left(card.left() - strip.left())
                        .top(card.top() - strip.top())
                        .w(card.size.width)
                        .h(card.size.height)
                        .rounded(radius)
                        .shadow(card_shadow()),
                )
        })
        .collect()
    }

    /// A thin bar in the margin above the card while the active page loads.
    /// It moves only when a progress event arrives.
    pub(super) fn render_progress(&self, card: Bounds<Pixels>, theme: &Theme) -> Option<Div> {
        if self.fullscreen {
            return None;
        }
        let page = self.page(self.active_tab_id()?);
        if !page.loading {
            return None;
        }
        let card_width = f32::from(card.size.width);
        let width = 48. + (card_width * 0.5 - 48.).max(0.) * page.progress.clamp(0., 1.) as f32;
        Some(
            div()
                .absolute()
                .top(px(4.))
                .left(card.origin.x + px((card_width - width) / 2.))
                .w(px(width))
                .h(px(2.))
                .rounded(px(1.))
                .bg(theme.focus()),
        )
    }
}

/// A quiet centred note on an empty card.
fn hint(theme: &Theme, text: &'static str, detail: Option<&str>) -> Div {
    div()
        .absolute()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .text_size(px(13.))
        .text_color(theme.tertiary())
        .child(text)
        .when_some(detail.map(str::to_owned), |hint, detail| {
            hint.child(
                truncated(detail)
                    .max_w(px(520.))
                    .text_size(px(12.))
                    .text_color(theme.disabled()),
            )
        })
}
