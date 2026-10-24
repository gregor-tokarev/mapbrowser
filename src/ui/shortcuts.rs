//! Help > Keyboard Shortcuts: every shortcut in one panel over the page
//! card. Like the command bar it is a pop-up window of its own, because the
//! native web views cover anything GPUI draws over the card. Esc, ⌘W, a
//! click elsewhere or its close button close it; the arrow keys, Page Up,
//! Page Down, Space, Home and End scroll it.

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
    ScrollHandle, StatefulInteractiveElement, Styled, Subscription, Window, WindowHandle, actions,
    div, point, prelude::FluentBuilder, px, size,
};

actions!(
    shortcuts,
    [Close, LineUp, LineDown, PageUp, PageDown, ToTop, ToBottom]
);

pub fn init(cx: &mut App) {
    let context = Some("Shortcuts");
    cx.bind_keys([
        KeyBinding::new("escape", Close, context),
        KeyBinding::new("cmd-w", Close, context),
        KeyBinding::new("up", LineUp, context),
        KeyBinding::new("down", LineDown, context),
        KeyBinding::new("pageup", PageUp, context),
        KeyBinding::new("pagedown", PageDown, context),
        KeyBinding::new("shift-space", PageUp, context),
        KeyBinding::new("space", PageDown, context),
        KeyBinding::new("home", ToTop, context),
        KeyBinding::new("end", ToBottom, context),
        KeyBinding::new("cmd-up", ToTop, context),
        KeyBinding::new("cmd-down", ToBottom, context),
    ]);
}

const WIDTH: f32 = 640.;
/// Narrower panels list the groups in one column: in two, the longer
/// descriptions would wrap.
const TWO_COLUMNS_WIDTH: f32 = 600.;
const TITLE_HEIGHT: f32 = 50.;
const HEADER_HEIGHT: f32 = 28.;
/// A row's height, unless its description wraps; it grows then.
const ROW_HEIGHT: f32 = 24.;
const LABEL_LINE: f32 = 18.;
/// How far ↑ and ↓ scroll.
const LINE_SCROLL: f32 = 2. * ROW_HEIGHT;
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
                ("Open it in a new tab instead", &["⌘Return"]),
                ("Close the tab", &["⌘W"]),
                ("Reopen a closed tab", &["⇧⌘T"]),
                ("Go to tab 1–8", &["⌘1–⌘8"]),
                ("Go to the last tab", &["⌘9"]),
                ("Back to recent tabs, hold ⌃", &["⌃Tab"]),
                ("Cancel the switch, hold ⌃", &["⌃Esc"]),
                ("Next or previous tab", &["⌥⌘↓", "⌥⌘↑"]),
                // Chrome's and Safari's keys for the same.
                ("", &["⇧⌘]", "⇧⌘["]),
                ("Pin or unpin the tab", &["⌘D"]),
                ("Copy the URL", &["⇧⌘C"]),
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
        Group {
            title: "Map Browser",
            rows: &[
                ("Search the history", &["⌘Y"]),
                ("Settings", &["⌘,"]),
                ("Hide Map Browser", &["⌘H"]),
                ("Hide the other apps", &["⌥⌘H"]),
                ("Minimize the window", &["⌘M"]),
                ("Quit", &["⌘Q"]),
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
                ("", &["⌘←", "⌘→"]),
                ("Find on the page", &["⌘F"]),
                ("Next or previous match", &["⌘G", "⇧⌘G"]),
                ("Zoom in or out", &["⌘+", "⌘−"]),
                ("Actual size", &["⌘0"]),
                ("Print", &["⌘P"]),
                ("Developer tools", &["⌥⌘I"]),
                ("Exit full screen", &["Esc"]),
            ],
        },
        Group {
            title: "Menus",
            rows: &[
                ("Open the selected tab's menu", &["⇧F10"]),
                ("Move through a menu", &["↑", "↓"]),
                ("First or last item", &["Home", "End"]),
                ("Open or leave a submenu", &["→", "←"]),
                ("Next or previous button", &["Tab", "⇧Tab"]),
                ("Jump to an item by its name", &["A–Z"]),
                ("Choose", &["Return"]),
                ("Delete, when asked", &["⌘⌫"]),
                ("Close a menu or the command bar", &["Esc"]),
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

fn two_columns(panel_width: f32) -> bool {
    panel_width >= TWO_COLUMNS_WIDTH
}

/// The panel's size: as tall as its longer column, or its one column, but
/// inside the card.
fn panel_size(card: Bounds<Pixels>) -> gpui::Size<f32> {
    let (card_width, card_height) = (f32::from(card.size.width), f32::from(card.size.height));
    let width = WIDTH.min(card_width - 2. * PADDING);
    let heights = COLUMNS.iter().map(|groups| column_height(groups));
    let content = if two_columns(width) {
        heights.fold(0., f32::max)
    } else {
        heights.sum()
    };
    gpui::Size {
        width,
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
    /// The list of shortcuts, which scrolls in a short window.
    scroll: ScrollHandle,
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
            scroll: ScrollHandle::new(),
            was_active: window.is_window_active(),
            _subscriptions: subscriptions,
        }
    }

    /// Scrolls the list to `top`, its distance from the list's top, kept
    /// between its ends.
    fn scroll_to(&mut self, top: f32, cx: &mut Context<Self>) {
        let offset = self.scroll.offset();
        let end = f32::from(self.scroll.max_offset().height);
        let y = px(-top.clamp(0., end));
        if y != offset.y {
            self.scroll.set_offset(point(offset.x, y));
            cx.notify();
        }
    }

    /// Scrolls the list by `distance`, down if positive.
    fn scroll_by(&mut self, distance: f32, cx: &mut Context<Self>) {
        let top = -f32::from(self.scroll.offset().y);
        self.scroll_to(top + distance, cx);
    }

    /// A page: what shows of the list, less a line kept in sight.
    fn page(&self) -> f32 {
        (f32::from(self.scroll.bounds().size.height) - LINE_SCROLL).max(LINE_SCROLL)
    }

    fn render_group(&self, group: &Group) -> Div {
        let theme = &self.theme;
        let key = |text: &'static str| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .h(px(19.))
                .px(px(6.))
                .rounded(px(5.))
                .bg(theme.control_hover())
                .text_size(px(12.))
                .text_color(theme.title())
                .whitespace_nowrap()
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
                // A description too long for the row wraps, and the row
                // grows with it; the keys keep their size.
                div()
                    .min_h(px(ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .py(px((ROW_HEIGHT - LABEL_LINE) / 2.))
                            .text_size(px(13.))
                            .line_height(px(LABEL_LINE))
                            .text_color(theme.title())
                            .child(*what),
                    )
                    .children(keys.iter().map(|text| key(text)))
            }))
    }
}

impl Render for ShortcutsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let panel_width = f32::from(window.viewport_size().width) - 2. * MARGIN;
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
        let column = |groups: &mut dyn Iterator<Item = &Group>| {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .children(groups.map(|group| self.render_group(group)))
        };
        let columns = div().flex().gap(px(COLUMN_GAP)).pb(px(PADDING));
        let columns = if two_columns(panel_width) {
            columns.children(COLUMNS.iter().map(|groups| column(&mut groups.iter())))
        } else {
            columns.child(column(&mut COLUMNS.iter().flat_map(|groups| groups.iter())))
        };
        let columns = div()
            .id("shortcuts-columns")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(columns);
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
            .on_action(cx.listener(|this, _: &LineUp, _, cx| this.scroll_by(-LINE_SCROLL, cx)))
            .on_action(cx.listener(|this, _: &LineDown, _, cx| this.scroll_by(LINE_SCROLL, cx)))
            .on_action(cx.listener(|this, _: &PageUp, _, cx| this.scroll_by(-this.page(), cx)))
            .on_action(cx.listener(|this, _: &PageDown, _, cx| this.scroll_by(this.page(), cx)))
            .on_action(cx.listener(|this, _: &ToTop, _, cx| this.scroll_to(0., cx)))
            .on_action(cx.listener(|this, _: &ToBottom, _, cx| this.scroll_to(f32::MAX, cx)))
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
