//! "Press Esc to exit full screen": a short note at the top of the screen
//! when a page goes fullscreen, naming the site, as Chrome shows it, so a
//! page can't take over the whole screen unnoticed. Like the toast over the
//! card it is a pop-up window of its own, because the page's native view
//! covers anything GPUI draws there. It takes neither focus nor clicks, and
//! exists only for the few seconds it shows.

use super::{
    UI_FONT,
    command_bar::{self, TRANSPARENT},
    elide::{self, Cut},
    shadow,
    theme::Theme,
};
use crate::{app::BrowserApp, model::TabId};
use gpui::{
    AppContext, Bounds, Context, Font, FontWeight, IntoElement, ParentElement, Pixels, Point,
    Render, SharedString, Styled, Task, TextRun, Window, WindowHandle, WindowOptions, div, font,
    http_client::Url, point, prelude::FluentBuilder, px, size,
};
use std::time::Duration;

/// Long enough to read once the window has finished going fullscreen.
const DURATION: Duration = Duration::from_secs(4);
/// The note's distance from the top of the screen.
const TOP: f32 = 18.;
const HEIGHT: f32 = 36.;
const FONT_SIZE: f32 = 13.;
const KEY: &str = "Esc";
/// What the note is wider than its text: padding, border and the key cap's
/// padding.
const CHROME_WIDTH: f32 = 2. * 16. + 2. + 2. * 6.;
/// Transparent room around the note for its shadow; X11 previews have no
/// compositor, so there the window is exactly the note.
const MARGIN: f32 = if TRANSPARENT { 14. } else { 0. };

pub struct FullscreenHint {
    host: SharedString,
    window: Option<HintWindow>,
    _timer: Task<()>,
}

struct HintWindow {
    handle: WindowHandle<HintView>,
    /// The card the window was placed over.
    card: Bounds<Pixels>,
    /// The pop-up's NSView, attached to the browser window as a child.
    #[cfg(target_os = "macos")]
    native_view: Option<*mut std::ffi::c_void>,
}

pub struct HintView {
    host: SharedString,
    theme: Theme,
}

/// "example.com · Press Esc to exit full screen", the key in a key cap.
fn text_parts(host: &str) -> [String; 2] {
    [
        format!("{host}  ·  Press "),
        " to exit full screen".to_owned(),
    ]
}

impl Render for HintView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let [before, after] = text_parts(&self.host);
        let note = div()
            .absolute()
            .left(px(MARGIN))
            .right(px(MARGIN))
            .top(px(MARGIN))
            .h(px(HEIGHT))
            .flex()
            .items_center()
            .justify_center()
            .px(px(16.))
            .rounded(px(HEIGHT / 2.))
            .bg(theme.surface())
            .border_1()
            .border_color(theme.ink(0.1))
            .when(TRANSPARENT, |note| note.shadow(vec![shadow(4., 14., 0.16)]))
            .text_size(px(FONT_SIZE))
            .line_height(px(18.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.title())
            .whitespace_nowrap()
            .child(before)
            .child(
                div()
                    .px(px(6.))
                    .rounded(px(5.))
                    .bg(theme.control_hover())
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(KEY),
            )
            .child(after);
        div()
            .size_full()
            .relative()
            .font_family(UI_FONT)
            .when(!TRANSPARENT, |root| root.bg(theme.surface()))
            .child(note)
    }
}

/// The width of `text` as the note draws it.
fn measure(text: &str, weight: FontWeight, window: &Window) -> f32 {
    let run = TextRun {
        len: text.len(),
        font: Font {
            weight,
            ..font(UI_FONT)
        },
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    f32::from(
        window
            .text_system()
            .layout_line(text, px(FONT_SIZE), &[run], None)
            .width,
    )
}

/// The width of the note's text for `host`, the key cap's padding aside.
fn text_width(host: &str, window: &Window) -> f32 {
    let [before, after] = text_parts(host);
    measure(&before, FontWeight::MEDIUM, window)
        + measure(KEY, FontWeight::SEMIBOLD, window)
        + measure(&after, FontWeight::MEDIUM, window)
}

/// `host` as the note on `card` shows it: a host too long for the line
/// loses its start, so the domain that owns it and the way out stay.
fn fitted_host(host: &str, card: Bounds<Pixels>, window: &Window) -> String {
    let room = f32::from(card.size.width) - 2. * TOP - CHROME_WIDTH - text_width("", window);
    elide::fit(host, room, Cut::Start, |host| {
        measure(host, FontWeight::MEDIUM, window)
    })
}

/// The note's width for `host`, already fitted to `card`.
fn note_width(host: &str, card: Bounds<Pixels>, window: &Window) -> f32 {
    (text_width(host, window) + CHROME_WIDTH)
        .ceil()
        .min(f32::from(card.size.width) - 2. * TOP)
}

/// The window's top-left corner in the browser window: centred at the top
/// of `card`, for a note `width` wide.
fn window_origin(card: Bounds<Pixels>, width: f32) -> Point<Pixels> {
    point(
        card.left() + (card.size.width - px(width)) / 2. - px(MARGIN),
        card.top() + px(TOP - MARGIN),
    )
}

impl BrowserApp {
    /// The tab's page went fullscreen: its site and the way out show for a
    /// few seconds.
    pub(crate) fn show_fullscreen_hint(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let Some(url) = self.state.tab(tab_id).map(|tab| tab.url.clone()) else {
            return;
        };
        let host = Url::parse(&url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or(url);
        let timer = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DURATION).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(hint) = this.notices.fullscreen_hint.take() {
                    this.close_hint_window(hint.window, cx);
                }
            });
        });
        // A note still showing (a page leaving and entering fullscreen
        // again) is replaced.
        let window = self
            .notices
            .fullscreen_hint
            .take()
            .and_then(|hint| hint.window);
        self.notices.fullscreen_hint = Some(FullscreenHint {
            host: host.into(),
            window,
            _timer: timer,
        });
        if let Some(window) = self
            .notices
            .fullscreen_hint
            .as_mut()
            .and_then(|hint| hint.window.as_mut())
        {
            // Placed again, with the new text, on the next frame.
            window.card = Bounds::default();
        }
        cx.notify();
    }

    /// Called on every frame of the browser window: the note opens, follows
    /// the card as the window goes fullscreen, or closes.
    pub(super) fn sync_fullscreen_hint(
        &mut self,
        card: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = match &self.notices.fullscreen_hint {
            None => true,
            Some(hint) if !self.fullscreen => hint.window.is_none(),
            Some(hint) => hint.window.as_ref().is_some_and(|shown| shown.card == card),
        };
        if !current {
            cx.defer_in(window, |this, window, cx| {
                this.update_fullscreen_hint(window, cx)
            });
        }
    }

    fn update_fullscreen_hint(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.fullscreen {
            // Leaving fullscreen ends the note.
            if let Some(hint) = self.notices.fullscreen_hint.take() {
                self.close_hint_window(hint.window, cx);
            }
            return;
        }
        let card = self.card_bounds(window.viewport_size());
        let theme = self.theme();
        let Some((host, opened)) = self
            .notices
            .fullscreen_hint
            .as_ref()
            .map(|hint| (hint.host.clone(), hint.window.is_some()))
        else {
            return;
        };
        let host: SharedString = fitted_host(&host, card, window).into();
        let width = note_width(&host, card, window);
        if !opened {
            let shown = self.open_hint_window(host, card, width, theme, window, cx);
            if let Some(hint) = &mut self.notices.fullscreen_hint {
                hint.window = shown;
            }
            return;
        }
        let Some(shown) = self
            .notices
            .fullscreen_hint
            .as_mut()
            .and_then(|hint| hint.window.as_mut())
        else {
            return;
        };
        if shown.card != card {
            shown.card = card;
            let _ = shown.handle.update(cx, |view, window, cx| {
                view.host = host;
                window.resize(size(px(width + 2. * MARGIN), px(HEIGHT + 2. * MARGIN)));
                cx.notify();
            });
            #[cfg(target_os = "macos")]
            if let Some(view) = shown.native_view {
                let origin = window_origin(card, width);
                self.engine.borrow().place_child_window(
                    view,
                    f64::from(origin.x),
                    f64::from(origin.y),
                );
            }
        }
    }

    fn open_hint_window(
        &self,
        host: SharedString,
        card: Bounds<Pixels>,
        width: f32,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<HintWindow> {
        let bounds = Bounds::new(
            window.bounds().origin + window_origin(card, width),
            size(px(width + 2. * MARGIN), px(HEIGHT + 2. * MARGIN)),
        );
        let display = window.display(cx).map(|display| display.id());
        let options = WindowOptions {
            // The page keeps keyboard focus.
            focus: false,
            ..command_bar::window_options(bounds, display)
        };
        let handle = cx
            .open_window(options, |_, cx| cx.new(|_| HintView { host, theme }))
            .ok()?;
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
        Some(HintWindow {
            handle,
            card,
            #[cfg(target_os = "macos")]
            native_view,
        })
    }

    fn close_hint_window(&self, shown: Option<HintWindow>, cx: &mut Context<Self>) {
        let Some(shown) = shown else {
            return;
        };
        #[cfg(target_os = "macos")]
        if let Some(view) = shown.native_view {
            self.engine.borrow().detach_child_window(view);
        }
        let _ = shown
            .handle
            .update(cx, |_, window, _| window.remove_window());
    }
}

/// Clicks on the note go to the page under it.
#[cfg(target_os = "macos")]
fn ignore_mouse(view: *mut std::ffi::c_void) {
    // SAFETY: `view` is the NSView from the live pop-up's raw window handle,
    // used on the main thread, where GPUI runs.
    let view = unsafe { &*view.cast::<objc2_app_kit::NSView>() };
    if let Some(window) = view.window() {
        window.setIgnoresMouseEvents(true);
    }
}
