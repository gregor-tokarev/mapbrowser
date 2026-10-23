//! Help > Keyboard Shortcuts: every shortcut in one panel over the page
//! card. Like the command bar it is a pop-up window of its own, because the
//! native web views cover anything GPUI draws over the card. Esc, ⌘W, a
//! click elsewhere or its close button close it.

use super::{
    UI_FONT,
    command_bar::{self, TRANSPARENT},
    icons::{Icon, icon},
    shadow,
    theme::Theme,
};
use crate::app::BrowserApp;
use gpui::{
    App, AppContext, Bounds, Context, Div, EventEmitter, FocusHandle, FontWeight,
    InteractiveElement, IntoElement, KeyBinding, ParentElement, Pixels, Point, Render,
    StatefulInteractiveElement, Styled, Subscription, Window, WindowHandle, actions, div, point,
    prelude::FluentBuilder, px, size,
};

actions!(shortcuts, [Close]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", Close, Some("Shortcuts")),
        KeyBinding::new("cmd-w", Close, Some("Shortcuts")),
    ]);
}

const WIDTH: f32 = 640.;
const TITLE_HEIGHT: f32 = 50.;
const HEADER_HEIGHT: f32 = 28.;
const ROW_HEIGHT: f32 = 24.;
const PADDING: f32 = 20.;
const COLUMN_GAP: f32 = 28.;
/// Transparent room around the panel for its shadow; X11 previews have no
/// compositor, so there the window is exactly the panel.
const MARGIN: f32 = if TRANSPARENT { 40. } else { 0. };

/// A titled group of shortcuts: what each does, and its keys.
struct Group {
    title: &'static str,
    rows: &'static [(&'static str, &'static [&'static str])],
}

const COLUMNS: [&[Group]; 2] = [
    &[
        Group {
            title: "Tabs",
            rows: &[
                ("Open a tab or search", &["⌘T"]),
                ("Edit the address", &["⌘L"]),
                ("Close the tab", &["⌘W"]),
                ("Reopen a closed tab", &["⇧⌘T"]),
                ("Search the history", &["⌘Y"]),
                ("Go to tab 1–8", &["⌘1–⌘8"]),
                ("Go to the last tab", &["⌘9"]),
                ("Back to recent tabs, hold ⌃", &["⌃Tab"]),
                ("Cancel the switch, hold ⌃", &["⌃Esc"]),
                ("Next or previous tab", &["⌥⌘↓", "⌥⌘↑"]),
                ("Pin or unpin the tab", &["⌘D"]),
                ("Copy the address", &["⇧⌘C"]),
                ("Clear Today", &["⇧⌘K"]),
            ],
        },
        Group {
            title: "Spaces",
            rows: &[
                ("Go to space 1–9", &["⌃1–⌃9"]),
                ("Next or previous space", &["⌥⌘→", "⌥⌘←"]),
                ("Hide or show the sidebar", &["⌘S"]),
            ],
        },
    ],
    &[
        Group {
            title: "Pages",
            rows: &[
                ("Reload", &["⌘R"]),
                ("Stop loading", &["⌘."]),
                ("Back or forward", &["⌘[", "⌘]"]),
                ("Find on the page", &["⌘F"]),
                ("Next or previous match", &["⌘G", "⇧⌘G"]),
                ("Zoom in or out", &["⌘+", "⌘−"]),
                ("Actual size", &["⌘0"]),
                ("Print", &["⌘P"]),
                ("Developer tools", &["⌥⌘I"]),
            ],
        },
        Group {
            title: "Menus",
            rows: &[
                ("Move through a menu", &["↑", "↓"]),
                ("Choose", &["Return"]),
                ("Close a menu or the command bar", &["Esc"]),
            ],
        },
        Group {
            title: "Map Browser",
            rows: &[
                ("Settings", &["⌘,"]),
                ("Minimise the window", &["⌘M"]),
                ("Quit", &["⌘Q"]),
            ],
        },
    ],
];

fn column_height(groups: &[Group]) -> f32 {
    groups
        .iter()
        .map(|group| HEADER_HEIGHT + group.rows.len() as f32 * ROW_HEIGHT)
        .sum()
}

/// The panel's size: as tall as its longer column, but inside the card.
fn panel_size(card: Bounds<Pixels>) -> gpui::Size<f32> {
    let content = COLUMNS
        .iter()
        .map(|groups| column_height(groups))
        .fold(0., f32::max);
    let (card_width, card_height) = (f32::from(card.size.width), f32::from(card.size.height));
    gpui::Size {
        width: WIDTH.min(card_width - 2. * PADDING),
        height: (TITLE_HEIGHT + content + PADDING).min(card_height - 2. * PADDING),
    }
}

/// Screen bounds of the panel's window, centred over `card` (in window
/// coordinates) for a window at `origin`.
fn window_bounds(card: Bounds<Pixels>, origin: Point<Pixels>) -> Bounds<Pixels> {
    let panel = panel_size(card);
    let left = f32::from(card.origin.x) + (f32::from(card.size.width) - panel.width) / 2.;
    let top = f32::from(card.origin.y) + (f32::from(card.size.height) - panel.height) / 2.;
    Bounds::new(
        origin + point(px(left - MARGIN), px(top - MARGIN)),
        size(
            px(panel.width + 2. * MARGIN),
            px(panel.height + 2. * MARGIN),
        ),
    )
}

pub enum ShortcutsEvent {
    /// `refocus` is false when another window or application took focus.
    Close { refocus: bool },
}

pub struct ShortcutsPanel {
    theme: Theme,
    focus_handle: FocusHandle,
    was_active: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ShortcutsEvent> for ShortcutsPanel {}

impl ShortcutsPanel {
    fn new(theme: Theme, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle);
        let subscriptions = vec![cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.was_active = true;
            } else if this.was_active {
                cx.emit(ShortcutsEvent::Close { refocus: false });
            }
        })];
        Self {
            theme,
            focus_handle,
            was_active: window.is_window_active(),
            _subscriptions: subscriptions,
        }
    }

    fn render_group(&self, group: &Group) -> Div {
        let theme = &self.theme;
        let key = |text: &'static str| {
            div()
                .flex()
                .items_center()
                .h(px(19.))
                .px(px(6.))
                .rounded(px(5.))
                .bg(theme.control_hover())
                .text_size(px(12.))
                .text_color(theme.title())
                .child(text)
        };
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(HEADER_HEIGHT))
                    .flex()
                    .items_end()
                    .pb(px(5.))
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.tertiary())
                    .child(group.title),
            )
            .children(group.rows.iter().map(|(what, keys)| {
                div()
                    .h(px(ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(13.))
                            .text_color(theme.title())
                            .child(*what),
                    )
                    .children(keys.iter().map(|text| key(text)))
            }))
    }
}

impl Render for ShortcutsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let hover = theme.hover();
        let title = div()
            .h(px(TITLE_HEIGHT))
            .flex()
            .items_center()
            .child(
                div()
                    .flex_1()
                    .text_size(px(15.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.title())
                    .child("Keyboard Shortcuts"),
            )
            .child(
                div()
                    .id("close-shortcuts")
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(26.))
                    .rounded(px(6.))
                    .hover(move |style| style.bg(hover))
                    .child(icon(Icon::Close, 16., theme.icon()))
                    .on_click(
                        cx.listener(|_, _, _, cx| cx.emit(ShortcutsEvent::Close { refocus: true })),
                    ),
            );
        let columns =
            div()
                .id("shortcuts-columns")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(div().flex().gap(px(COLUMN_GAP)).pb(px(PADDING)).children(
                    COLUMNS.iter().map(|groups| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .children(groups.iter().map(|group| self.render_group(group)))
                    }),
                ));
        let panel = div()
            .absolute()
            .left(px(MARGIN))
            .top(px(MARGIN))
            .right(px(MARGIN))
            .bottom(px(MARGIN))
            .flex()
            .flex_col()
            .px(px(PADDING))
            .rounded(px(12.))
            .bg(theme.surface())
            .border_1()
            .border_color(theme.ink(if theme.dark { 0.14 } else { 0.1 }))
            .when(TRANSPARENT, |panel| {
                panel.shadow(vec![shadow(16., 48., 0.25)])
            })
            .child(title)
            .child(columns);
        div()
            .size_full()
            .relative()
            .font_family(UI_FONT)
            .key_context("Shortcuts")
            .track_focus(&self.focus_handle)
            .on_action(
                cx.listener(|_, _: &Close, _, cx| cx.emit(ShortcutsEvent::Close { refocus: true })),
            )
            .when(!TRANSPARENT, |root| root.bg(theme.surface()))
            .child(panel)
    }
}

pub struct ShortcutsWindow {
    handle: WindowHandle<ShortcutsPanel>,
    /// The pop-up's NSView, attached to the browser window as a child.
    #[cfg(target_os = "macos")]
    native_view: Option<*mut std::ffi::c_void>,
    _subscription: Subscription,
}

impl BrowserApp {
    /// Help > Keyboard Shortcuts opens the panel, or closes it if open.
    pub fn show_shortcuts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shortcuts.is_some() {
            return self.close_shortcuts(true, window, cx);
        }
        // The panel taking key status would close the bar too, but only later.
        self.close_command_bar(cx);
        let card = self.card_bounds(window.viewport_size());
        let bounds = window_bounds(card, window.bounds().origin);
        let display = window.display(cx).map(|display| display.id());
        let theme = self.theme();
        let mut panel = None;
        let opened = cx.open_window(
            command_bar::window_options(bounds, display),
            |window, cx| {
                let entity = cx.new(|cx| ShortcutsPanel::new(theme, window, cx));
                panel = Some(entity.clone());
                entity
            },
        );
        let (Ok(handle), Some(panel)) = (opened, panel) else {
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
        let subscription = cx.subscribe_in(&panel, window, |this, _, event, window, cx| {
            let ShortcutsEvent::Close { refocus } = event;
            this.close_shortcuts(*refocus, window, cx);
        });
        self.shortcuts = Some(ShortcutsWindow {
            handle,
            #[cfg(target_os = "macos")]
            native_view,
            _subscription: subscription,
        });
    }

    pub(crate) fn close_shortcuts(
        &mut self,
        refocus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(shortcuts) = self.shortcuts.take() else {
            return;
        };
        #[cfg(target_os = "macos")]
        if let Some(view) = shortcuts.native_view {
            self.engine.borrow().detach_child_window(view);
        }
        let _ = shortcuts
            .handle
            .update(cx, |_, window, _| window.remove_window());
        if refocus {
            window.activate_window();
            self.focus_page(window, cx);
        }
    }
}
