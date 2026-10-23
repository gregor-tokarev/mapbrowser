//! The address of the link under the pointer, in a small pill at the page
//! card's bottom-left, as in Arc and Chrome. The page's native views cover
//! anything GPUI draws in the browser window, so the pill is a pop-up window
//! of its own, like the find bar, which the pointer passes through. It is
//! made once and then shown and hidden; while it shows, only its own small
//! window redraws as the address changes, and hidden it costs nothing.

use super::{
    UI_FONT,
    command_bar::{self, TRANSPARENT},
    shadow,
    theme::Theme,
};
use crate::{app::BrowserApp, engine::page::elide_middle, model::TabId};
use gpui::{
    AppContext, Bounds, Context, DisplayId, Entity, IntoElement, ParentElement, Pixels, Point,
    Render, SharedString, Size, Styled, TextRun, Window, WindowHandle, WindowOptions, div, font,
    point, prelude::FluentBuilder, px, size,
};

pub(super) const HEIGHT: f32 = 22.;
/// The pill's distance from the card's left and bottom edges.
pub(super) const INSET: f32 = 6.;
const PADDING: f32 = 8.;
const FONT_SIZE: f32 = 12.;
/// The widest the pill gets; on narrow cards, half the card.
const MAX_WIDTH: f32 = 520.;
/// Transparent room around the pill for its shadow. X11 previews have no
/// compositor, so there the window is exactly the pill.
const MARGIN: f32 = if TRANSPARENT { 6. } else { 0. };

/// The address as the pill shows it: secure links without their scheme.
fn shown_address(address: &str) -> &str {
    address.strip_prefix("https://").unwrap_or(address)
}

/// `text`, or as much of its start and end around a "…" (`elide_middle`)
/// as fits in `max_width`, measured with `width`.
pub(super) fn fit_middle(text: &str, max_width: f32, width: impl Fn(&str) -> f32) -> String {
    if width(text) <= max_width {
        return text.to_owned();
    }
    // The most characters that fit, assuming fewer never take more room.
    let (mut fits, mut too_many) = (1, text.chars().count());
    while too_many - fits > 1 {
        let kept = (fits + too_many) / 2;
        if width(&elide_middle(text, kept)) <= max_width {
            fits = kept;
        } else {
            too_many = kept;
        }
    }
    elide_middle(text, fits)
}

/// The pill window's top-left corner in the browser window, for `card`.
fn window_origin(card: Bounds<Pixels>) -> Point<Pixels> {
    point(
        card.left() + px(INSET - MARGIN),
        card.bottom() - px(INSET + HEIGHT + MARGIN),
    )
}

fn window_options(bounds: Bounds<Pixels>, display_id: Option<DisplayId>) -> WindowOptions {
    WindowOptions {
        focus: false,
        // macOS shows it once it is attached to the browser window.
        show: !cfg!(target_os = "macos"),
        ..command_bar::window_options(bounds, display_id)
    }
}

pub struct StatusPill {
    text: SharedString,
    theme: Theme,
}

impl Render for StatusPill {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let pill = div()
            .absolute()
            .left(px(MARGIN))
            .top(px(MARGIN))
            .right(px(MARGIN))
            .bottom(px(MARGIN))
            .flex()
            .items_center()
            .px(px(PADDING))
            .rounded(px(6.))
            .bg(theme.surface())
            .border_1()
            .border_color(theme.ink(0.1))
            .when(TRANSPARENT, |pill| pill.shadow(vec![shadow(1., 4., 0.1)]))
            .text_size(px(FONT_SIZE))
            .text_color(theme.url_text())
            .whitespace_nowrap()
            .overflow_hidden()
            .child(self.text.clone());
        div()
            .size_full()
            .relative()
            .font_family(UI_FONT)
            .when(!TRANSPARENT, |root| root.bg(theme.surface()))
            .child(pill)
    }
}

/// The hovered link of the active page, and the pill showing it.
#[derive(Default)]
pub struct LinkStatus {
    /// The tab whose link the pill shows, while it shows.
    tab_id: Option<TabId>,
    pill: Option<PillWindow>,
}

struct PillWindow {
    handle: WindowHandle<StatusPill>,
    pill: Entity<StatusPill>,
    size: Size<Pixels>,
    /// The card the window was placed at.
    card: Bounds<Pixels>,
    /// The pop-up's NSView, attached to the browser window while shown.
    #[cfg(target_os = "macos")]
    native_view: Option<*mut std::ffi::c_void>,
}

impl BrowserApp {
    /// Whether the pill shows; a toast over the card stands above it then.
    pub(super) fn link_status_shown(&self) -> bool {
        self.link_status.tab_id.is_some()
    }

    /// The pill came or went: a toast over the card moves out of its way
    /// (`sync_toast_window`), which takes a frame of the browser window.
    fn link_status_moved_toast(&self, cx: &mut Context<Self>) {
        if self.toast_window.is_some() {
            cx.notify();
        }
    }

    /// Whether the pill may show the hovered link of `tab_id`'s page.
    fn shows_link_status(&self, tab_id: TabId) -> bool {
        self.active_tab_id() == Some(tab_id)
            && self.is_loaded(tab_id)
            && !self.fullscreen
            && self.slide.is_none()
    }

    /// A page's hovered link changed; `address` is empty once there is none.
    pub fn show_link_status(
        &mut self,
        tab_id: TabId,
        address: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if address.is_empty() || !self.shows_link_status(tab_id) {
            if self.link_status.tab_id == Some(tab_id) {
                self.hide_link_status(cx);
            }
            return;
        }
        let card = self.card_bounds(window.viewport_size());
        let measure = |text: &str| {
            let run = TextRun {
                len: text.len(),
                font: font(UI_FONT),
                color: gpui::black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let line = window
                .text_system()
                .layout_line(text, px(FONT_SIZE), &[run], None);
            f32::from(line.width)
        };
        let room = (f32::from(card.size.width) / 2.).min(MAX_WIDTH) - 2. * PADDING;
        let text = fit_middle(shown_address(&address), room, measure);
        // A point to spare for rounding, so the text is never clipped.
        let width = (measure(&text) + 2. * PADDING + 1.).ceil();
        let size = size(px(width + 2. * MARGIN), px(HEIGHT + 2. * MARGIN));
        let theme = self.theme();
        let shown = self.link_status.tab_id.is_some();
        self.link_status.tab_id = Some(tab_id);
        if !shown {
            self.link_status_moved_toast(cx);
        }
        let Some(pill) = &mut self.link_status.pill else {
            let bounds = Bounds::new(window.bounds().origin + window_origin(card), size);
            let display = window.display(cx).map(|display| display.id());
            return self.open_link_status(bounds, card, text.into(), theme, display, cx);
        };
        pill.pill.update(cx, |pill, cx| {
            pill.text = text.into();
            pill.theme = theme;
            cx.notify();
        });
        if pill.size != size {
            pill.size = size;
            let _ = pill.handle.update(cx, |_, window, _| window.resize(size));
        }
        #[cfg(target_os = "macos")]
        if let Some(view) = pill.native_view {
            let engine = self.engine.borrow();
            if !shown || pill.card != card {
                let origin = window_origin(card);
                engine.place_child_window(view, f64::from(origin.x), f64::from(origin.y));
            }
            if !shown {
                engine.show_overlay_window(view, true);
            }
        }
        pill.card = card;
    }

    fn open_link_status(
        &mut self,
        bounds: Bounds<Pixels>,
        card: Bounds<Pixels>,
        text: SharedString,
        theme: Theme,
        display: Option<DisplayId>,
        cx: &mut Context<Self>,
    ) {
        let mut pill = None;
        let opened = cx.open_window(window_options(bounds, display), |_, cx| {
            let entity = cx.new(|_| StatusPill { text, theme });
            pill = Some(entity.clone());
            entity
        });
        let (Ok(handle), Some(pill)) = (opened, pill) else {
            self.link_status.tab_id = None;
            return;
        };
        #[cfg(target_os = "macos")]
        let native_view = handle
            .update(cx, |_, window, _| crate::native::ns_view(window))
            .ok()
            .flatten();
        #[cfg(target_os = "macos")]
        if let Some(view) = native_view {
            self.engine.borrow().show_overlay_window(view, true);
        }
        self.link_status.pill = Some(PillWindow {
            handle,
            pill,
            size: bounds.size,
            card,
            #[cfg(target_os = "macos")]
            native_view,
        });
    }

    fn hide_link_status(&mut self, cx: &mut Context<Self>) {
        if self.link_status.tab_id.take().is_none() {
            return;
        }
        self.link_status_moved_toast(cx);
        // macOS keeps the window for next time; previews can't hide one.
        #[cfg(target_os = "macos")]
        if let Some(view) = self
            .link_status
            .pill
            .as_ref()
            .and_then(|pill| pill.native_view)
        {
            self.engine.borrow().show_overlay_window(view, false);
        }
        #[cfg(not(target_os = "macos"))]
        if let Some(pill) = self.link_status.pill.take() {
            let _ = pill
                .handle
                .update(cx, |_, window, _| window.remove_window());
        }
    }

    /// Called on every frame of the browser window: the pill follows the
    /// card, and goes when its page is no longer on screen.
    pub(super) fn sync_link_status(
        &mut self,
        card: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_id) = self.link_status.tab_id else {
            return;
        };
        if !self.shows_link_status(tab_id) {
            cx.defer_in(window, |this, _, cx| this.hide_link_status(cx));
            return;
        }
        let Some(pill) = &mut self.link_status.pill else {
            return;
        };
        if pill.card == card {
            return;
        }
        pill.card = card;
        #[cfg(target_os = "macos")]
        if let Some(view) = pill.native_view {
            let origin = window_origin(card);
            self.engine
                .borrow()
                .place_child_window(view, f64::from(origin.x), f64::from(origin.y));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pill_sits_at_the_cards_bottom_left() {
        let card = Bounds::new(point(px(238.), px(10.)), size(px(1032.), px(800.)));
        let origin = window_origin(card);
        assert_eq!(f32::from(origin.x) + MARGIN, 238. + INSET);
        assert_eq!(f32::from(origin.y) + MARGIN + HEIGHT, 810. - INSET);
    }

    #[test]
    fn long_addresses_keep_their_start_and_end() {
        let width = |text: &str| text.chars().count() as f32;
        assert_eq!(fit_middle("example.com/a", 20., width), "example.com/a");
        assert_eq!(fit_middle("abcdefghij", 5., width), "ab…ij");
        assert_eq!(fit_middle("abcdefghij", 9.5, width), "abcd…ghij");
        assert_eq!(fit_middle("abcdefghij", 0.5, width), "…");
        assert_eq!(shown_address("https://a.test/b"), "a.test/b");
        assert_eq!(shown_address("http://a.test/b"), "http://a.test/b");
        assert_eq!(shown_address("mailto:a@b.test"), "mailto:a@b.test");
    }
}
