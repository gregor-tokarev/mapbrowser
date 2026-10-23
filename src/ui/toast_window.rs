//! The toast ("Copied URL", "Zoom 110%", "Downloaded …") while the sidebar,
//! where toasts show, is hidden (⌘S): over the page card's bottom-left
//! corner, as Arc shows toasts over the page then. It is a pop-up window of
//! its own, because the native web views cover anything GPUI draws over
//! the card. It takes neither focus nor clicks, and exists only while the
//! toast does: nothing stays open, or draws, at rest.

use super::{
    UI_FONT,
    command_bar::{self, TRANSPARENT},
    icons::{Icon, icon},
    shadow,
    theme::Theme,
    truncated,
};
use crate::app::BrowserApp;
use gpui::{
    AppContext, Bounds, Context, Font, FontWeight, IntoElement, ParentElement, Pixels, Point,
    Render, SharedString, Styled, TextRun, Window, WindowHandle, WindowOptions, div, font, point,
    prelude::FluentBuilder, px, size,
};

/// The toast's distance from the card's left and bottom edges.
const INSET: f32 = 10.;
const HEIGHT: f32 = 34.;
const FONT_SIZE: f32 = 12.5;
/// What the toast is wider than its text: padding, border, the check mark
/// and the gap after it.
const CHROME_WIDTH: f32 = 2. * 12. + 2. + 14. + 8.;
/// Transparent room around the toast for its shadow; X11 previews have no
/// compositor, so there the window is exactly the toast.
const MARGIN: f32 = if TRANSPARENT { 14. } else { 0. };

pub struct ToastView {
    text: SharedString,
    theme: Theme,
}

impl Render for ToastView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let toast = div()
            .absolute()
            .left(px(MARGIN))
            .right(px(MARGIN))
            .top(px(MARGIN))
            .h(px(HEIGHT))
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .rounded(px(10.))
            .bg(theme.surface())
            .border_1()
            .border_color(theme.ink(0.08))
            .when(TRANSPARENT, |toast| {
                toast.shadow(vec![shadow(4., 14., 0.12)])
            })
            .text_size(px(FONT_SIZE))
            .line_height(px(16.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.title())
            .child(icon(Icon::Check, 14., theme.focus()).flex_none())
            .child(truncated(self.text.clone()).min_w_0());
        div()
            .size_full()
            .relative()
            .font_family(UI_FONT)
            .when(!TRANSPARENT, |root| root.bg(theme.surface()))
            .child(toast)
    }
}

pub struct ToastWindow {
    handle: WindowHandle<ToastView>,
    text: SharedString,
    /// The card the window was placed over.
    card: Bounds<Pixels>,
    /// The pop-up's NSView, attached to the browser window as a child.
    #[cfg(target_os = "macos")]
    native_view: Option<*mut std::ffi::c_void>,
}

/// The window's top-left corner in the browser window, for `card`.
fn window_origin(card: Bounds<Pixels>) -> Point<Pixels> {
    point(
        card.left() + px(INSET - MARGIN),
        card.bottom() - px(INSET + HEIGHT + MARGIN),
    )
}

/// The window's size for a toast saying `text` over `card`.
fn window_size(text: &str, card: Bounds<Pixels>, window: &Window) -> gpui::Size<Pixels> {
    let run = TextRun {
        len: text.len(),
        font: Font {
            weight: FontWeight::MEDIUM,
            ..font(UI_FONT)
        },
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let text_width = window
        .text_system()
        .layout_line(text, px(FONT_SIZE), &[run], None)
        .width;
    let width = (f32::from(text_width) + CHROME_WIDTH)
        .ceil()
        .min(f32::from(card.size.width) - 2. * INSET);
    size(px(width + 2. * MARGIN), px(HEIGHT + 2. * MARGIN))
}

impl BrowserApp {
    /// The toast, if the sidebar that shows toasts is hidden, else the
    /// progress of running downloads, which its ring would show. Pages in
    /// HTML fullscreen (videos) show none.
    fn toast_over_card(&self) -> Option<SharedString> {
        if self.sidebar_width().is_some() || self.fullscreen {
            return None;
        }
        self.toast
            .as_ref()
            .map(|toast| toast.text.clone())
            .or_else(|| self.downloads_over_card())
    }

    /// Called on every frame of the browser window: the pop-up opens,
    /// changes, follows the card or closes with the toast.
    pub(super) fn sync_toast_window(
        &mut self,
        card: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = match (self.toast_over_card(), &self.toast_window) {
            (None, None) => true,
            (Some(text), Some(shown)) => text == shown.text && card == shown.card,
            _ => false,
        };
        if !current {
            cx.defer_in(window, |this, window, cx| {
                this.update_toast_window(window, cx)
            });
        }
    }

    fn update_toast_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.toast_over_card() else {
            return self.close_toast_window(cx);
        };
        let card = self.card_bounds(window.viewport_size());
        let Some(shown) = &mut self.toast_window else {
            return self.open_toast_window(text, card, window, cx);
        };
        if shown.text != text {
            let size = window_size(&text, card, window);
            shown.text = text.clone();
            let _ = shown.handle.update(cx, |toast, window, cx| {
                toast.text = text;
                window.resize(size);
                cx.notify();
            });
        }
        if shown.card != card {
            shown.card = card;
            #[cfg(target_os = "macos")]
            if let Some(view) = shown.native_view {
                let origin = window_origin(card);
                self.engine.borrow().place_child_window(
                    view,
                    f64::from(origin.x),
                    f64::from(origin.y),
                );
            }
        }
    }

    fn open_toast_window(
        &mut self,
        text: SharedString,
        card: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let bounds = Bounds::new(
            window.bounds().origin + window_origin(card),
            window_size(&text, card, window),
        );
        let display = window.display(cx).map(|display| display.id());
        let theme = self.theme();
        let options = WindowOptions {
            // The page keeps keyboard focus.
            focus: false,
            ..command_bar::window_options(bounds, display)
        };
        let view_text = text.clone();
        let Ok(handle) = cx.open_window(options, |_, cx| {
            cx.new(|_| ToastView {
                text: view_text,
                theme,
            })
        }) else {
            return;
        };
        #[cfg(target_os = "macos")]
        let native_view = handle
            .update(cx, |_, window, _| crate::native::ns_view(window))
            .ok()
            .flatten();
        #[cfg(target_os = "macos")]
        if let Some(view) = native_view {
            self.engine.borrow().attach_child_window(view, false);
            ignore_mouse(view);
        }
        self.toast_window = Some(ToastWindow {
            handle,
            text,
            card,
            #[cfg(target_os = "macos")]
            native_view,
        });
    }

    fn close_toast_window(&mut self, cx: &mut Context<Self>) {
        let Some(toast) = self.toast_window.take() else {
            return;
        };
        #[cfg(target_os = "macos")]
        if let Some(view) = toast.native_view {
            self.engine.borrow().detach_child_window(view);
        }
        let _ = toast
            .handle
            .update(cx, |_, window, _| window.remove_window());
    }
}

/// Clicks on the toast go to the page under it.
#[cfg(target_os = "macos")]
fn ignore_mouse(view: *mut std::ffi::c_void) {
    // SAFETY: `view` is the NSView from the live pop-up's raw window handle,
    // used on the main thread, where GPUI runs.
    let view = unsafe { &*view.cast::<objc2_app_kit::NSView>() };
    if let Some(window) = view.window() {
        window.setIgnoresMouseEvents(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_toast_sits_at_the_cards_bottom_left() {
        let card = Bounds::new(point(px(10.), px(10.)), size(px(1260.), px(800.)));
        let origin = window_origin(card);
        assert_eq!(f32::from(origin.x) + MARGIN, 10. + INSET);
        assert_eq!(f32::from(origin.y) + MARGIN + HEIGHT, 810. - INSET);
    }
}
