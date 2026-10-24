//! Toasts: short messages ("Copied URL", "Zoom 110%", "Downloaded …") above
//! the sidebar's bottom bar, with a mark for what they report: ✓ done,
//! ✕ failed, · for news. Long ones lose their middle, so a file's extension
//! and a host's domain stay, and stay up a little longer.
//!
//! While the sidebar is hidden (⌘S) the toast shows over the page card's
//! bottom-left corner, as Arc shows toasts over the page then, above the
//! hovered link's address if that shows. It is a pop-up window of its own,
//! because the native web views cover anything GPUI draws over the card. It
//! takes neither focus nor clicks, and exists only while the toast does:
//! nothing stays open, or draws, at rest.
//!
//! A failed save is the one lasting notice: it stays until a save succeeds.

use super::{
    UI_FONT,
    command_bar::{self, TRANSPARENT},
    elide::{self, Cut},
    icons::{Icon, icon},
    link_status, press_only, shadow,
    theme::Theme,
    truncated,
};
use crate::{app::BrowserApp, model::TabId};
use gpui::{
    AnyElement, AppContext, Bounds, Context, Div, Font, FontWeight, InteractiveElement,
    IntoElement, ParentElement, Pixels, Point, Render, SharedString, StatefulInteractiveElement,
    Styled, Task, Window, WindowHandle, WindowOptions, div, font, point, prelude::FluentBuilder,
    px, size,
};
use std::time::Duration;

/// The toast's distance from the card's left and bottom edges.
const INSET: f32 = 10.;
const HEIGHT: f32 = 34.;
const FONT_SIZE: f32 = 12.5;
const ICON_SIZE: f32 = 14.;
/// What the toast is wider than its text: padding, border, the mark and the
/// gap after it.
const CHROME_WIDTH: f32 = 2. * 12. + 2. + ICON_SIZE + 8.;
/// What a sidebar toast is narrower than the sidebar: its margins, then the
/// same as `CHROME_WIDTH`.
const SIDEBAR_CHROME_WIDTH: f32 = 2. * 8. + CHROME_WIDTH;
/// Transparent room around the toast for its shadow; X11 previews have no
/// compositor, so there the window is exactly the toast.
const MARGIN: f32 = if TRANSPARENT { 14. } else { 0. };
/// How far the toast rises over the card while the link status pill shows
/// below it, with a small gap between them.
const LIFT: f32 = link_status::HEIGHT + link_status::INSET + 4. - INSET;

/// What a toast reports, shown by its mark.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    /// ✓: something was done ("Copied URL").
    Done,
    /// ✕: something didn't work.
    Failed,
    /// ·: news that is neither ("Zoom 110%", "Downloading …").
    Info,
}

pub struct Toast {
    pub kind: ToastKind,
    pub text: SharedString,
    /// Says how to undo what was done ("Closed 3 tabs · ⇧⌘T reopens
    /// them"), with the batch of closed tabs it is about: the sidebar wraps
    /// it to two lines rather than cut the hint, and it goes once they are
    /// back.
    undo: Option<TabId>,
    /// The text as the sidebar shows it, elided to fit `.0` points.
    fitted: Option<(f32, SharedString)>,
    _timer: Task<()>,
}

impl Toast {
    /// The sidebar's text: elided once the sidebar has measured it.
    fn shown(&self) -> SharedString {
        self.fitted
            .as_ref()
            .map_or_else(|| self.text.clone(), |(_, text)| text.clone())
    }
}

/// How long a toast that says how to undo something stays, long enough to
/// read it and act.
const UNDO_DURATION: Duration = Duration::from_secs(4);

/// How long a toast stays: 1.5 s for a few words, longer for long ones and
/// failures, which take longer to read, but at most 5 s.
fn toast_duration(kind: ToastKind, text: &str) -> Duration {
    let extra_chars = text.chars().count().saturating_sub(24) as u64;
    let failed = if kind == ToastKind::Failed { 1500 } else { 0 };
    Duration::from_millis((1500 + 50 * extra_chars + failed).min(5000))
}

/// The toast's mark: a check, a cross, or a dot.
fn mark(kind: ToastKind, theme: &Theme) -> AnyElement {
    match kind {
        ToastKind::Done => icon(Icon::Check, ICON_SIZE, theme.focus()).into_any_element(),
        ToastKind::Failed => icon(Icon::Close, ICON_SIZE, theme.error()).into_any_element(),
        ToastKind::Info => div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(ICON_SIZE))
            .child(div().size(px(5.)).rounded_full().bg(theme.icon()))
            .into_any_element(),
    }
}

/// The width of `text` as toasts draw it.
fn text_width(text: &str, window: &Window) -> f32 {
    let medium = Font {
        weight: FontWeight::MEDIUM,
        ..font(UI_FONT)
    };
    super::text_width(text, medium, FONT_SIZE, window)
}

/// `text`, elided in the middle to fit `room` points.
fn fit(text: &str, room: f32, window: &Window) -> SharedString {
    elide::fit(text, room, Cut::Middle, |text| text_width(text, window)).into()
}

pub struct ToastView {
    kind: ToastKind,
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
            .child(mark(self.kind, theme))
            .child(truncated(self.text.clone()).min_w_0());
        div()
            .size_full()
            .relative()
            .font_family(UI_FONT)
            .when(!TRANSPARENT, |root| root.bg(theme.surface()))
            .child(toast)
    }
}

/// What the toast window shows: a toast, or the failed save's notice.
type Shown = (ToastKind, SharedString);

pub struct ToastWindow {
    handle: WindowHandle<ToastView>,
    shown: Shown,
    /// The card the window was placed over.
    card: Bounds<Pixels>,
    /// Whether it was raised over the link status pill.
    lifted: bool,
    /// The pop-up's NSView, attached to the browser window as a child.
    #[cfg(target_os = "macos")]
    native_view: Option<*mut std::ffi::c_void>,
}

/// The window's top-left corner in the browser window, for `card`, raised
/// over the link status pill if `lifted`.
fn window_origin(card: Bounds<Pixels>, lifted: bool) -> Point<Pixels> {
    let lift = if lifted { LIFT } else { 0. };
    point(
        card.left() + px(INSET - MARGIN),
        card.bottom() - px(INSET + HEIGHT + MARGIN + lift),
    )
}

/// The window's size for a toast saying `text`, already fitted to `card`.
fn window_size(text: &str, card: Bounds<Pixels>, window: &Window) -> gpui::Size<Pixels> {
    let width = (text_width(text, window) + CHROME_WIDTH)
        .ceil()
        .min(f32::from(card.size.width) - 2. * INSET);
    size(px(width + 2. * MARGIN), px(HEIGHT + 2. * MARGIN))
}

impl BrowserApp {
    /// Shows a toast of `kind`, replacing any other. `show_toast` shows a ✓.
    pub fn show_toast_kind(
        &mut self,
        kind: ToastKind,
        text: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let text = text.into();
        let duration = toast_duration(kind, &text);
        self.set_toast(kind, text, duration, None, cx);
    }

    /// Shows a ✓ toast that says how ⇧⌘T reopens the tabs closed together
    /// as `batch`, for `UNDO_DURATION`.
    pub fn show_undo_toast(
        &mut self,
        text: impl Into<SharedString>,
        batch: TabId,
        cx: &mut Context<Self>,
    ) {
        self.set_toast(ToastKind::Done, text.into(), UNDO_DURATION, Some(batch), cx);
    }

    /// The tabs closed as `batch` are back: the toast offering that goes.
    pub(crate) fn dismiss_undo_toast(&mut self, batch: TabId, cx: &mut Context<Self>) {
        if self
            .toast
            .as_ref()
            .is_some_and(|toast| toast.undo == Some(batch))
        {
            self.toast = None;
            cx.notify();
        }
    }

    fn set_toast(
        &mut self,
        kind: ToastKind,
        text: SharedString,
        duration: Duration,
        undo: Option<TabId>,
        cx: &mut Context<Self>,
    ) {
        let timer = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(duration).await;
            let _ = this.update(cx, |this, cx| {
                this.toast = None;
                cx.notify();
            });
        });
        self.toast = Some(Toast {
            kind,
            text,
            undo,
            fitted: None,
            _timer: timer,
        });
        cx.notify();
    }

    /// The failed save's notice, short enough for a toast.
    fn save_notice(&self) -> Option<Shown> {
        let reason = self.save_failure.as_ref()?;
        Some((
            ToastKind::Failed,
            format!("Can't save your tabs: {reason}").into(),
        ))
    }

    /// Fits the toast to the sidebar, `width` wide; measured once per width.
    /// An undo toast wraps instead.
    pub(super) fn fit_toast(&mut self, width: f32, window: &Window) {
        let room = (width - SIDEBAR_CHROME_WIDTH).max(0.);
        if let Some(toast) = &mut self.toast
            && toast.undo.is_none()
            && toast
                .fitted
                .as_ref()
                .is_none_or(|(fitted, _)| *fitted != room)
        {
            toast.fitted = Some((room, fit(&toast.text, room, window)));
        }
    }

    /// The sidebar's toast and, below it, the failed save's notice, above
    /// its bottom bar.
    pub(super) fn render_toasts(
        &self,
        width: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<Div> {
        let card = || {
            div()
                .mx(px(8.))
                .px(px(12.))
                .py(px(8.))
                .rounded(px(10.))
                .bg(theme.surface())
                .border_1()
                .border_color(theme.ink(0.08))
                .shadow(vec![shadow(4., 14., 0.12)])
                .text_size(px(FONT_SIZE))
        };
        let toast = self.toast.as_ref().map(|toast| {
            card()
                .flex()
                .items_center()
                .gap(px(8.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.title())
                .child(mark(toast.kind, theme))
                .child(if toast.undo.is_some() {
                    div()
                        .min_w_0()
                        .line_clamp(2)
                        .text_ellipsis()
                        .child(toast.text.clone())
                } else {
                    truncated(toast.shown()).min_w_0()
                })
        });
        // The reason may be long: the notice wraps, within a known width.
        let text_width = width - SIDEBAR_CHROME_WIDTH;
        let save = self.save_failure.as_ref().map(|reason| {
            let control_hover = theme.control_hover();
            card()
                .flex()
                .gap(px(8.))
                .child(div().pt(px(1.)).child(mark(ToastKind::Failed, theme)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_start()
                        .gap(px(6.))
                        .w(px(text_width.max(0.)))
                        .child(
                            div()
                                .line_height(px(16.))
                                .text_color(theme.title())
                                .child(format!("Map Browser can't save your tabs: {reason}.")),
                        )
                        .child(
                            press_only(
                                div()
                                    .id("save-again")
                                    .px(px(8.))
                                    .py(px(3.))
                                    .rounded(px(5.))
                                    .bg(theme.control())
                                    .hover(move |style| style.bg(control_hover))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.title())
                                    .child("Try Again"),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.save_again(cx))),
                        ),
                )
        });
        toast.into_iter().chain(save).collect()
    }

    /// The toast, or the failed save's notice, if the sidebar that shows
    /// them is hidden, else the progress of running downloads, which its
    /// ring would show. Pages in HTML fullscreen (videos) show none. A
    /// ⌃Tab walk names the tab it has reached instead, which the hidden
    /// sidebar would outline.
    fn toast_over_card(&self) -> Option<Shown> {
        if self.sidebar_width().is_some() || self.fullscreen {
            return None;
        }
        let walk = self
            .tab_switch
            .as_ref()
            .and_then(|switch| self.state.tab(switch.target()))
            .map(|tab| (ToastKind::Info, tab.shown_title().to_owned().into()));
        walk.or_else(|| {
            self.toast
                .as_ref()
                .map(|toast| (toast.kind, toast.text.clone()))
        })
        .or_else(|| self.save_notice())
        .or_else(|| {
            self.downloads_over_card()
                .map(|text| (ToastKind::Info, text))
        })
    }

    /// Called on every frame of the browser window: the pop-up opens,
    /// changes, follows the card or closes with the toast.
    pub(super) fn sync_toast_window(
        &mut self,
        card: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let lifted = self.link_status_shown();
        let current = match (self.toast_over_card(), &self.toast_window) {
            (None, None) => true,
            (Some(shown), Some(open)) => {
                shown == open.shown && card == open.card && lifted == open.lifted
            }
            _ => false,
        };
        if !current {
            cx.defer_in(window, |this, window, cx| {
                this.update_toast_window(window, cx)
            });
        }
    }

    fn update_toast_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((kind, text)) = self.toast_over_card() else {
            return self.close_toast_window(cx);
        };
        let card = self.card_bounds(window.viewport_size());
        let lifted = self.link_status_shown();
        let fitted = fit(
            &text,
            f32::from(card.size.width) - 2. * INSET - CHROME_WIDTH,
            window,
        );
        // Previews can't move a window: they open it again where it goes.
        let moved = self
            .toast_window
            .as_ref()
            .is_some_and(|open| open.card != card || open.lifted != lifted);
        if moved && cfg!(not(target_os = "macos")) {
            self.close_toast_window(cx);
        }
        let Some(open) = &mut self.toast_window else {
            return self.open_toast_window((kind, text), fitted, card, lifted, window, cx);
        };
        if open.shown != (kind, text.clone()) {
            let size = window_size(&fitted, card, window);
            open.shown = (kind, text);
            let _ = open.handle.update(cx, |toast, window, cx| {
                toast.kind = kind;
                toast.text = fitted;
                window.resize(size);
                cx.notify();
            });
        }
        if open.card != card || open.lifted != lifted {
            open.card = card;
            open.lifted = lifted;
            #[cfg(target_os = "macos")]
            if let Some(view) = open.native_view {
                let origin = window_origin(card, lifted);
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
        shown: Shown,
        fitted: SharedString,
        card: Bounds<Pixels>,
        lifted: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let bounds = Bounds::new(
            window.bounds().origin + window_origin(card, lifted),
            window_size(&fitted, card, window),
        );
        let display = window.display(cx).map(|display| display.id());
        let theme = self.theme();
        let options = WindowOptions {
            // The page keeps keyboard focus.
            focus: false,
            ..command_bar::window_options(bounds, display)
        };
        let kind = shown.0;
        let Ok(handle) = cx.open_window(options, |_, cx| {
            cx.new(|_| ToastView {
                kind,
                text: fitted,
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
            shown,
            card,
            lifted,
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
        let origin = window_origin(card, false);
        assert_eq!(f32::from(origin.x) + MARGIN, 10. + INSET);
        assert_eq!(f32::from(origin.y) + MARGIN + HEIGHT, 810. - INSET);
    }
}
