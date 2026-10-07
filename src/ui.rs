//! The GPUI browser controls and new-tab page. Chromium owns the web page views.

use crate::{
    Back, BrowserApp, CloseTab, FocusAddress, Forward, NewTab, NextSpace, NextTab, PreviousSpace,
    Reload, engine::Shortcut, model::Space,
};
use gpui::{
    Context, Div, Focusable, FontWeight, IntoElement, MouseButton, Render, Stateful, Window,
    canvas, div, prelude::*, px, rgb, rgba,
};

pub const SIDEBAR: f32 = 248.;
pub const TOPBAR: f32 = 56.;
const BG: u32 = 0x141619;
const PANEL: u32 = 0x1b1e22;
const TEXT: u32 = 0xe9ecef;
const MUTED: u32 = 0x8c929c;
const BORDER: u32 = 0x2a2e34;
#[cfg(target_os = "macos")]
const UI_FONT: &str = ".AppleSystemUIFont";
/// Previews use Inter, the closest open font to SF Pro. Without it, GPUI uses
/// its fallback stack (Ubuntu, Noto Sans, DejaVu Sans, ...).
#[cfg(not(target_os = "macos"))]
const UI_FONT: &str = "Inter";

fn icon_button(id: &'static str, symbol: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .size(px(30.))
        .rounded(px(7.))
        .text_size(px(18.))
        .text_color(rgb(MUTED))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(0x272b31)).text_color(rgb(TEXT)))
        .child(symbol)
}

fn keycap(label: &'static str) -> Div {
    div()
        .px(px(6.))
        .py(px(2.))
        .border_1()
        .border_color(rgb(0x343941))
        .rounded(px(4.))
        .text_size(px(11.))
        .text_color(rgb(MUTED))
        .child(label)
}

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

/// Without a page engine (Linux previews), label the page area instead.
fn page_placeholder(url: &str) -> Div {
    div()
        .absolute()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .text_size(px(12.))
        .text_color(rgb(MUTED))
        .child("Web pages are not rendered in this preview")
        .child(div().text_color(rgb(0x656c76)).child(url.to_owned()))
}

impl BrowserApp {
    fn tab_list(&self, space: &Space, offset: f32, cx: &mut Context<Self>) -> Div {
        let accent = self.accent(space.id);
        let space_id = space.id;
        let interactive = space_id == self.state.selected_space_id;
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(offset))
            .w(px(SIDEBAR))
            .flex()
            .flex_col()
            .px(px(12.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(49.))
                    .px(px(8.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(9.))
                            .child(div().size(px(7.)).rounded_full().bg(rgb(accent)))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(accent))
                                    .child(space.name.clone()),
                            ),
                    )
                    .child(
                        icon_button("rename-space", "···")
                            .id(("rename-space", space.id))
                            .when(interactive, |button| {
                                button.on_click(cx.listener(move |this, _, window, cx| {
                                    this.edit_space(Some(space_id), window, cx);
                                }))
                            }),
                    ),
            )
            .child(
                div()
                    .id(("tabs", space.id))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(space.tabs.iter().map(|tab| {
                        let id = tab.id;
                        let selected = id == space.selected_tab_id;
                        let blank = tab.url == "about:blank";
                        let loading = self.loading.contains(&id);
                        div()
                            .id(("tab", id))
                            .flex()
                            .items_center()
                            .gap(px(9.))
                            .h(px(38.))
                            .px(px(9.))
                            .mb(px(3.))
                            .rounded(px(8.))
                            .cursor_pointer()
                            .when(selected, |style| style.bg(rgb(0x2b3035)))
                            .hover(|style| style.bg(rgb(0x272c31)))
                            .when(interactive, |row| {
                                row.on_click(cx.listener(move |this, _, window, cx| {
                                    this.select_tab(id, window, cx);
                                }))
                            })
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .flex_shrink_0()
                                    .size(px(19.))
                                    .rounded(px(5.))
                                    .text_size(px(13.))
                                    .text_color(rgb(if selected { accent } else { MUTED }))
                                    .child(if loading {
                                        "◌"
                                    } else if blank {
                                        "✧"
                                    } else {
                                        "◉"
                                    }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(12.))
                                    .text_color(rgb(if selected { TEXT } else { 0xb2b7be }))
                                    .text_ellipsis()
                                    .child(tab.title.clone()),
                            )
                            .child(
                                div()
                                    .id(("close-tab", id))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .flex_shrink_0()
                                    .size(px(21.))
                                    .rounded(px(5.))
                                    .text_size(px(15.))
                                    .text_color(rgb(if selected { MUTED } else { 0x565d67 }))
                                    .hover(|style| style.bg(rgb(0x3d444d)).text_color(rgb(TEXT)))
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .when(interactive, |button| {
                                        button.on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.close_tab(id, cx);
                                        }))
                                    })
                                    .child("×"),
                            )
                    }))
                    .child(
                        div()
                            .id(("new-tab", space.id))
                            .flex()
                            .items_center()
                            .gap(px(11.))
                            .h(px(38.))
                            .mt(px(3.))
                            .px(px(11.))
                            .rounded(px(8.))
                            .cursor_pointer()
                            .text_color(rgb(MUTED))
                            .text_size(px(12.))
                            .hover(|style| style.bg(rgb(0x24292e)).text_color(rgb(TEXT)))
                            .when(interactive, |button| {
                                button.on_click(
                                    cx.listener(|this, _, window, cx| this.new_tab(window, cx)),
                                )
                            })
                            .child(div().text_size(px(19.)).child("+"))
                            .child("New tab")
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(rgb(0x656c76))
                                    .child("⌘T"),
                            ),
                    ),
            )
    }

    fn welcome(&self, space: &Space, offset: f32, cx: &mut Context<Self>) -> Div {
        let accent = self.accent(space.id);
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(offset))
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .bg(rgb(PANEL))
            .child(
                div()
                    .w(px(460.))
                    .max_w_full()
                    .px(px(36.))
                    .pb(px(32.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .mb(px(20.))
                            .text_size(px(11.))
                            .text_color(rgb(accent))
                            .child(div().size(px(6.)).rounded_full().bg(rgb(accent)))
                            .child(space.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(42.))
                            .line_height(px(50.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(TEXT))
                            .child("A fresh start."),
                    )
                    .child(
                        div()
                            .mt(px(11.))
                            .text_size(px(14.))
                            .line_height(px(22.))
                            .text_color(rgb(MUTED))
                            .child("A little space for what matters now."),
                    )
                    .child(
                        div()
                            .id(("welcome-search", space.id))
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .h(px(52.))
                            .mt(px(32.))
                            .px(px(16.))
                            .border_1()
                            .border_color(rgb(0x363b43))
                            .rounded(px(10.))
                            .bg(rgb(0x22262c))
                            .cursor_pointer()
                            .hover(|style| style.border_color(rgb(accent)).bg(rgb(0x272c32)))
                            .when(space.id == self.state.selected_space_id, |button| {
                                button.on_click(cx.listener(|this, _, window, cx| {
                                    this.shortcut(Shortcut::Address, window, cx);
                                }))
                            })
                            .child(div().text_size(px(20.)).text_color(rgb(accent)).child("⌕"))
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(px(12.))
                                    .text_color(rgb(0xa5abb4))
                                    .child("Search or enter an address"),
                            )
                            .child(keycap("⌘ L")),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .mt(px(23.))
                            .text_size(px(11.))
                            .text_color(rgb(0x707884))
                            .child(keycap("⌃ ⇥"))
                            .child("Next tab")
                            .child(div().w(px(15.)))
                            .child(keycap("⌥ ⌘ →"))
                            .child("Switch space"),
                    ),
            )
    }
}

impl Render for BrowserApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.state.active_space().clone();
        let tab = self.state.active_tab().clone();
        let accent = self.accent(active.id);
        let mut outgoing = None;
        let mut incoming_offset = 0.;
        let mut outgoing_offset = 0.;
        let mut direction = 1.;
        let mut progress = 1.;
        let mut native_transition = None;
        let mut transition_finished = false;
        if let Some(slide) = &mut self.slide {
            let elapsed_ms = slide.start.elapsed().as_secs_f64() * 1000.;
            slide.frames.push(elapsed_ms);
            let linear = (elapsed_ms / 240.).min(1.) as f32;
            progress = 1. - (1. - linear).powi(3);
            direction = if slide.direction < 0 { -1. } else { 1. };
            if linear < 1. {
                native_transition = Some((
                    slide.from.selected_tab_id,
                    active.selected_tab_id,
                    progress,
                    slide.direction,
                ));
                outgoing = Some(slide.from.clone());
                incoming_offset = SIDEBAR * direction * (1. - progress);
                outgoing_offset = -SIDEBAR * direction * progress;
                window.request_animation_frame();
            } else {
                transition_finished = true;
                if std::env::var_os("MAPBROWSER_TRACE_ANIMATION").is_some() {
                    eprintln!(
                        "ANIMATION {}",
                        serde_json::json!({
                            "from": slide.from.id,
                            "to": active.id,
                            "elapsed_ms": elapsed_ms,
                            "frames": slide.frames,
                        })
                    );
                }
                self.slide = None;
            }
        }
        let content_width = (f32::from(window.viewport_size().width) - SIDEBAR - 8.).max(0.);
        let engine = self.engine.clone();
        let selected_tab_id = active.selected_tab_id;
        let is_blank = tab.url == "about:blank";
        let has_outgoing = outgoing.is_some();
        let incoming_welcome = is_blank.then(|| {
            self.welcome(
                &active,
                if has_outgoing {
                    content_width * direction * (1. - progress)
                } else {
                    0.
                },
                cx,
            )
        });
        let outgoing_welcome = outgoing.as_ref().and_then(|space| {
            space
                .tabs
                .iter()
                .find(|tab| tab.id == space.selected_tab_id)
                .filter(|tab| tab.url == "about:blank")
                .map(|_| self.welcome(space, -content_width * direction * progress, cx))
        });

        div()
            .size_full()
            .flex()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .font_family(UI_FONT)
            .key_context("Browser")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &FocusAddress, window, cx| {
                this.shortcut(Shortcut::Address, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewTab, window, cx| {
                this.shortcut(Shortcut::NewTab, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                this.shortcut(Shortcut::CloseTab, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NextSpace, window, cx| {
                this.shortcut(Shortcut::NextSpace, window, cx)
            }))
            .on_action(cx.listener(|this, _: &PreviousSpace, window, cx| {
                this.shortcut(Shortcut::PreviousSpace, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| {
                this.shortcut(Shortcut::NextTab, window, cx)
            }))
            .on_action(cx.listener(|this, _: &Reload, window, cx| {
                this.shortcut(Shortcut::Reload, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &Back, window, cx| this.shortcut(Shortcut::Back, window, cx)),
            )
            .on_action(cx.listener(|this, _: &Forward, window, cx| {
                this.shortcut(Shortcut::Forward, window, cx)
            }))
            .child(
                div()
                    .w(px(SIDEBAR))
                    .h_full()
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .h(px(TOPBAR))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_end()
                            .pr(px(22.))
                            .child(
                                div()
                                    .text_size(px(19.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(0xd4d9de))
                                    .child("map"),
                            ),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .overflow_hidden()
                            .children(
                                outgoing
                                    .as_ref()
                                    .map(|space| self.tab_list(space, outgoing_offset, cx)),
                            )
                            .child(self.tab_list(&active, incoming_offset, cx)),
                    )
                    .when(self.editing_space.is_some(), |sidebar| {
                        sidebar.child(
                            div()
                                .mx(px(12.))
                                .mb(px(12.))
                                .p(px(12.))
                                .rounded(px(10.))
                                .border_1()
                                .border_color(rgb(0x3e464f))
                                .bg(rgb(0x22272d))
                                .child(
                                    div()
                                        .mb(px(10.))
                                        .text_size(px(11.))
                                        .text_color(rgb(accent))
                                        .child(if self.editing_space == Some(None) {
                                            "Create a space"
                                        } else {
                                            "Rename space"
                                        }),
                                )
                                .child(
                                    div()
                                        .px(px(8.))
                                        .py(px(5.))
                                        .rounded(px(6.))
                                        .bg(rgb(BG))
                                        .child(self.space_name.clone()),
                                )
                                .child(
                                    div()
                                        .mt(px(9.))
                                        .text_size(px(10.))
                                        .text_color(rgb(MUTED))
                                        .child("Enter to save · Esc to cancel"),
                                ),
                        )
                    })
                    .child(
                        div()
                            .px(px(16.))
                            .pt(px(14.))
                            .pb(px(18.))
                            .border_t_1()
                            .border_color(rgb(0x24292f))
                            .child(
                                div()
                                    .mb(px(12.))
                                    .text_size(px(10.))
                                    .text_color(rgb(0x676f79))
                                    .child("YOUR SPACES"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap(px(8.))
                                    .children(self.state.spaces.iter().map(|space| {
                                        let id = space.id;
                                        let selected = id == active.id;
                                        let color = self.accent(id);
                                        let initial = space
                                            .name
                                            .chars()
                                            .next()
                                            .unwrap_or('S')
                                            .to_uppercase()
                                            .to_string();
                                        div()
                                            .id(("space", id))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .size(px(35.))
                                            .rounded(px(10.))
                                            .border_1()
                                            .border_color(rgb(if selected {
                                                color
                                            } else {
                                                BORDER
                                            }))
                                            .bg(if selected {
                                                rgba((color << 8) | 0x20)
                                            } else {
                                                rgba(0x20252bff)
                                            })
                                            .text_color(rgb(if selected { color } else { MUTED }))
                                            .text_size(px(12.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .cursor_pointer()
                                            .hover(|style| {
                                                style
                                                    .border_color(rgb(color))
                                                    .text_color(rgb(color))
                                            })
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.switch_space(id, window, cx)
                                            }))
                                            .child(initial)
                                    }))
                                    .child(icon_button("add-space", "+").size(px(35.)).on_click(
                                        cx.listener(|this, _, window, cx| {
                                            this.edit_space(None, window, cx)
                                        }),
                                    )),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .h(px(TOPBAR))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .gap(px(3.))
                            .pr(px(12.))
                            .child(icon_button("back", "←").on_click(cx.listener(
                                |this, _, window, cx| this.shortcut(Shortcut::Back, window, cx),
                            )))
                            .child(icon_button("forward", "→").on_click(cx.listener(
                                |this, _, window, cx| this.shortcut(Shortcut::Forward, window, cx),
                            )))
                            .child(icon_button("reload", "↻").on_click(cx.listener(
                                |this, _, window, cx| this.shortcut(Shortcut::Reload, window, cx),
                            )))
                            .child(
                                div()
                                    .id("address-field")
                                    .capture_any_mouse_down(cx.listener(
                                        |this, event: &gpui::MouseDownEvent, window, cx| {
                                            if event.button == MouseButton::Left
                                                && !this
                                                    .address
                                                    .read(cx)
                                                    .focus_handle(cx)
                                                    .is_focused(window)
                                            {
                                                this.shortcut(Shortcut::Address, window, cx);
                                                cx.stop_propagation();
                                            }
                                        },
                                    ))
                                    .flex_1()
                                    .min_w_0()
                                    .h(px(34.))
                                    .ml(px(8.))
                                    .px(px(12.))
                                    .flex()
                                    .items_center()
                                    .gap(px(9.))
                                    .rounded(px(8.))
                                    .border_1()
                                    .border_color(rgb(0x2c3138))
                                    .bg(rgb(0x20242a))
                                    .child(div().text_color(rgb(accent)).text_size(px(12.)).child(
                                        if self.loading.contains(&tab.id) {
                                            "◌"
                                        } else {
                                            "⌕"
                                        },
                                    ))
                                    .child(self.address.clone()),
                            ),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .mr(px(8.))
                            .mb(px(8.))
                            .rounded(px(10.))
                            .overflow_hidden()
                            .bg(rgb(PANEL))
                            .child(
                                canvas(
                                    move |bounds, _, _| {
                                        let mut engine = engine.borrow_mut();
                                        engine.set_bounds(
                                            f64::from(bounds.origin.x),
                                            f64::from(bounds.origin.y),
                                            f64::from(bounds.size.width),
                                            f64::from(bounds.size.height),
                                        );
                                        // Apply offsets after layout, using the current viewport width.
                                        if let Some((from, to, progress, direction)) =
                                            native_transition
                                        {
                                            engine.set_transition(from, to, progress, direction);
                                        } else if transition_finished {
                                            engine.activate_tab(selected_tab_id);
                                        }
                                    },
                                    |_, _, _, _| {},
                                )
                                .absolute()
                                .size_full(),
                            )
                            .children(outgoing_welcome)
                            .children(incoming_welcome)
                            .when(cfg!(not(target_os = "macos")) && !is_blank, |content| {
                                content.child(page_placeholder(&tab.url))
                            })
                            .when_some(
                                self.error.clone().filter(|_| is_blank),
                                |content, error| {
                                    content.child(
                                        div()
                                            .absolute()
                                            .bottom(px(20.))
                                            .left(px(20.))
                                            .right(px(20.))
                                            .p(px(12.))
                                            .rounded(px(8.))
                                            .bg(rgb(0x3b292a))
                                            .text_size(px(12.))
                                            .text_color(rgb(0xefb3ad))
                                            .child(error),
                                    )
                                },
                            ),
                    )
                    .when_some(
                        self.error.clone().filter(|_| !is_blank),
                        |content, error| {
                            content.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(12.))
                                    .flex_shrink_0()
                                    .h(px(44.))
                                    .mr(px(8.))
                                    .mb(px(8.))
                                    .px(px(12.))
                                    .rounded(px(8.))
                                    .bg(rgb(0x302526))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_size(px(12.))
                                            .text_color(rgb(0xefb3ad))
                                            .text_ellipsis()
                                            .child(error_message(&error)),
                                    )
                                    .child(
                                        div()
                                            .id("reload-after-error")
                                            .flex_shrink_0()
                                            .px(px(10.))
                                            .py(px(5.))
                                            .rounded(px(5.))
                                            .bg(rgb(0x50363a))
                                            .text_size(px(11.))
                                            .text_color(rgb(0xf1c6c3))
                                            .cursor_pointer()
                                            .hover(|style| style.bg(rgb(0x654148)))
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.error = None;
                                                this.shortcut(Shortcut::Reload, window, cx);
                                                cx.notify();
                                            }))
                                            .child("Reload"),
                                    ),
                            )
                        },
                    ),
            )
    }
}
