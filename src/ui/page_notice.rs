//! What the page card shows in place of a tab's page: a sign-in a site or
//! proxy asks for (HTTP authentication), a page that crashed or stopped
//! responding, a certificate that didn't verify, a page asking before it is
//! left, and a load that failed. Each belongs to its tab and waits there
//! while other tabs are shown; the tab's next document ends those about the
//! last one. The page's native view would cover anything GPUI draws on the
//! card, so the engine hides it while one shows (`Engine::set_page_covered`).
//!
//! Closing, unloading or moving a single tab goes through `leave_tab`, so a
//! page with unsaved changes can ask first, as in Chrome. Bulk actions
//! (Clear, Unload Space, deleting a space) and quitting don't ask.

use super::{
    MaybeTooltip,
    fullscreen_hint::FullscreenHint,
    icons::{Icon, icon},
    press_only,
    theme::Theme,
    toast_window::ToastKind,
};
use crate::{
    app::BrowserApp,
    engine::{CrashReason, certificate_continue_allowed, is_insecure},
    input::{InputEvent, InputStyle, TextInput},
    model::{SpaceId, TabId},
};
use gpui::{
    App, AppContext, Context, Div, Entity, FocusHandle, Focusable, FontWeight, InteractiveElement,
    KeyBinding, KeyDownEvent, ParentElement, SharedString, Stateful, StatefulInteractiveElement,
    Styled, Subscription, Window, actions, div, http_client::Url, prelude::FluentBuilder, px,
    relative,
};
use std::collections::{HashMap, HashSet};

actions!(page_notice, [Confirm, Cancel]);

/// The card's column of text and buttons.
const COLUMN_WIDTH: f32 = 420.;
const ICON_TILE: f32 = 40.;
const FIELD_HEIGHT: f32 = 32.;

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", Confirm, Some("PageNotice")),
        KeyBinding::new("escape", Cancel, Some("PageNotice")),
    ]);
}

/// Who asks for a sign-in, and how the password would travel.
#[derive(Clone, PartialEq, Eq)]
struct SignInSite {
    host: String,
    port: u16,
    realm: String,
    proxy: bool,
    /// Whether the password would be sent encrypted (https).
    secure: bool,
    /// How it would be sent: "basic", "digest"…
    auth_scheme: String,
}

impl SignInSite {
    /// "host", or "host:port" for a proxy or off the scheme's usual port.
    fn label(&self) -> String {
        let usual = if self.secure { 443 } else { 80 };
        if self.port == 0 || (!self.proxy && self.port == usual) {
            self.host.clone()
        } else if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// A username and password a site or proxy asks for, with fields of its
/// own: what was typed stays with the request while other tabs show, and
/// goes, password and all, when the request ends.
struct SignIn {
    id: u64,
    tab_id: TabId,
    site: SignInSite,
    username: Entity<TextInput>,
    password: Entity<TextInput>,
    /// Return in either field signs in; Esc cancels.
    _subscriptions: [Subscription; 2],
}

impl SignIn {
    /// Requests that one answer settles: Chromium asks once per load that
    /// needs the sign-in, and they come together. Only the same tab's, for
    /// the same scheme, host, port, realm and kind of sign-in, so a
    /// password typed for https is never sent over http.
    fn same_as(&self, other: &SignIn) -> bool {
        self.tab_id == other.tab_id && self.site == other.site
    }

    fn focused(&self, window: &Window, cx: &App) -> bool {
        [&self.username, &self.password]
            .iter()
            .any(|field| field.focus_handle(cx).is_focused(window))
    }
}

/// The fields' look in the card's theme.
fn field_style(theme: &Theme) -> InputStyle {
    InputStyle {
        text: theme.title(),
        placeholder: theme.tertiary(),
        cursor: theme.focus(),
        selection: theme.focus().opacity(0.35),
        font_size: px(13.5),
        line_height: px(18.),
    }
}

struct Certificate {
    id: u64,
    url: String,
    code: i32,
}

struct Leave {
    id: u64,
    reload: bool,
}

struct LoadFailure {
    url: String,
    code: i32,
    /// Chromium's name for the error, such as "ERR_NAME_NOT_RESOLVED".
    name: String,
}

/// One tab's states. Sign-ins wait in `PageNotices::sign_ins`.
#[derive(Default)]
struct TabNotices {
    crashed: Option<CrashReason>,
    unresponsive: bool,
    /// Reload once the page that stopped responding has been ended.
    reload_when_ended: bool,
    certificate: Option<Certificate>,
    /// "Back to Safety": once the refused load has failed, go back.
    back_after_failure: bool,
    leave: Option<Leave>,
    failure: Option<LoadFailure>,
    /// The site whose certificate error the user continued past, until the
    /// tab leaves it. For the URL pill to show.
    unsafe_host: Option<String>,
}

/// What happens to a tab once its page lets go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Leaving {
    Close,
    Unload,
    Move(SpaceId),
}

/// What the card shows for a tab, the most pressing first.
enum Shown<'a> {
    Leave(&'a Leave),
    Crashed(CrashReason),
    Unresponsive,
    Certificate(&'a Certificate),
    SignIn(&'a SignIn),
    Failed(&'a LoadFailure),
}

pub struct PageNotices {
    tabs: HashMap<TabId, TabNotices>,
    /// Unanswered sign-ins, oldest first.
    sign_ins: Vec<SignIn>,
    /// Tabs whose page was asked to let go, and what happens then.
    leaving: HashMap<TabId, Leaving>,
    /// Tabs whose blocked pop-up was reported, until their next document.
    popups_reported: HashSet<TabId>,
    /// The last pop-up each tab's page had blocked that a tab could show,
    /// until its next document: "Open" on the notice opens it.
    blocked_popups: HashMap<TabId, String>,
    /// The username last signed in with, for the tab and site, until the
    /// tab shows another document: a wrong password brings the same
    /// sign-in back, which then starts with it.
    retry_username: Option<(TabId, SignInSite, String)>,
    /// The card, so that Return and Esc answer it.
    focus_handle: FocusHandle,
    /// "Press Esc to exit full screen", for a few seconds.
    pub(super) fullscreen_hint: Option<FullscreenHint>,
}

impl PageNotices {
    pub fn new(cx: &mut Context<BrowserApp>) -> Self {
        Self {
            tabs: HashMap::new(),
            sign_ins: Vec::new(),
            leaving: HashMap::new(),
            popups_reported: HashSet::new(),
            blocked_popups: HashMap::new(),
            retry_username: None,
            focus_handle: cx.focus_handle(),
            fullscreen_hint: None,
        }
    }

    fn shown(&self, tab_id: TabId) -> Option<Shown<'_>> {
        let sign_in = || {
            self.sign_ins
                .iter()
                .find(|request| request.tab_id == tab_id)
        };
        let Some(notices) = self.tabs.get(&tab_id) else {
            return sign_in().map(Shown::SignIn);
        };
        if let Some(leave) = &notices.leave {
            Some(Shown::Leave(leave))
        } else if let Some(reason) = notices.crashed {
            Some(Shown::Crashed(reason))
        } else if notices.unresponsive {
            Some(Shown::Unresponsive)
        } else if let Some(certificate) = &notices.certificate {
            Some(Shown::Certificate(certificate))
        } else if let Some(request) = sign_in() {
            Some(Shown::SignIn(request))
        } else {
            notices.failure.as_ref().map(Shown::Failed)
        }
    }

    /// Whether the card shows something in place of the tab's page.
    pub fn covers(&self, tab_id: TabId) -> bool {
        self.shown(tab_id).is_some()
    }

    /// Whether the tab's page crashed; its row shows it.
    pub fn crashed(&self, tab_id: TabId) -> bool {
        self.tabs
            .get(&tab_id)
            .is_some_and(|notices| notices.crashed.is_some())
    }

    /// Whether the user continued past a certificate error on the site the
    /// tab shows.
    pub fn certificate_bypassed(&self, tab_id: TabId) -> bool {
        self.tabs
            .get(&tab_id)
            .is_some_and(|notices| notices.unsafe_host.is_some())
    }

    fn tab(&mut self, tab_id: TabId) -> &mut TabNotices {
        self.tabs.entry(tab_id).or_default()
    }
}

/// The host of `url` as typed, "www." included: security text names the
/// exact site.
fn site(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| url.to_owned())
}

/// A failed load in plain words: an icon, a title and what happened.
fn failure_text(code: i32, host: &str) -> (Icon, &'static str, String) {
    let unreachable = "This site can't be reached";
    let broken = "This page isn't working";
    match code {
        -106 => (
            Icon::WifiOff,
            "You're offline",
            "Check your Wi-Fi or network connection, then try again.".into(),
        ),
        -105 | -137 => (
            Icon::Globe,
            "This site can't be found",
            format!(
                "The address of {host} couldn't be found. Check it for typos, or try again later."
            ),
        ),
        -102 => (
            Icon::Globe,
            unreachable,
            format!("{host} refused to connect."),
        ),
        -7 | -118 => (
            Icon::Globe,
            unreachable,
            format!("{host} took too long to respond."),
        ),
        -100 | -101 => (
            Icon::Globe,
            unreachable,
            format!("The connection to {host} was interrupted."),
        ),
        -104 | -109 => (
            Icon::Globe,
            unreachable,
            format!("Map Browser couldn't connect to {host}."),
        ),
        -21 => (
            Icon::WifiOff,
            "Your network changed",
            "The connection was interrupted while the network changed.".into(),
        ),
        -111 | -130 => (
            Icon::Globe,
            "The proxy isn't responding",
            "Check the proxy settings in System Settings, then try again.".into(),
        ),
        -107 | -113 => (
            Icon::ShieldAlert,
            "This site can't provide a secure connection",
            format!("{host} sent a response Map Browser couldn't use securely."),
        ),
        -299..=-200 => (
            Icon::ShieldAlert,
            "Your connection isn't private",
            format!("The certificate of {host} couldn't be verified."),
        ),
        -310 => (
            Icon::CircleAlert,
            broken,
            format!("{host} redirected too many times. Clearing its cookies may help."),
        ),
        -324 => (
            Icon::CircleAlert,
            broken,
            format!("{host} didn't send any data."),
        ),
        -320 => (
            Icon::CircleAlert,
            broken,
            format!("{host} sent a response Map Browser couldn't read."),
        ),
        -6 => (
            Icon::File,
            "This file can't be found",
            "It may have been moved, renamed or deleted.".into(),
        ),
        -10 => (
            Icon::File,
            "This file can't be opened",
            "Map Browser isn't allowed to read it.".into(),
        ),
        -300 => (
            Icon::CircleAlert,
            "This address isn't valid",
            "Check it for typos.".into(),
        ),
        -301 | -302 => (
            Icon::CircleAlert,
            "This address can't be opened",
            "Map Browser can't show this kind of address.".into(),
        ),
        _ => (
            Icon::CircleAlert,
            "This page couldn't load",
            format!("Something went wrong while loading {host}."),
        ),
    }
}

/// Why a certificate didn't verify, in plain words.
fn certificate_text(code: i32) -> &'static str {
    match code {
        -200 => "Its certificate belongs to another site.",
        -201 => {
            "Its certificate has expired or isn't valid yet. If your Mac's date and time are \
             wrong, correct them in System Settings."
        }
        -202 => "Its certificate isn't from an authority your Mac trusts.",
        -206 => "Its certificate was revoked by whoever issued it.",
        _ => "Its certificate couldn't be verified.",
    }
}

/// Chromium's name for a certificate error, shown as a detail.
fn certificate_error_name(code: i32) -> String {
    match code {
        -200 => "ERR_CERT_COMMON_NAME_INVALID".into(),
        -201 => "ERR_CERT_DATE_INVALID".into(),
        -202 => "ERR_CERT_AUTHORITY_INVALID".into(),
        -203 => "ERR_CERT_CONTAINS_ERRORS".into(),
        -206 => "ERR_CERT_REVOKED".into(),
        -207 => "ERR_CERT_INVALID".into(),
        -208 => "ERR_CERT_WEAK_SIGNATURE_ALGORITHM".into(),
        -213 => "ERR_CERT_VALIDITY_TOO_LONG".into(),
        code => format!("Error {code}"),
    }
}

/// The card's buttons: the default one filled with the space's colour.
fn button(
    id: &'static str,
    label: impl Into<SharedString>,
    primary: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let (fill, hover) = if primary {
        (theme.focus(), theme.focus().opacity(0.88))
    } else {
        (theme.control(), theme.control_hover())
    };
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .h(px(30.))
        .px(px(14.))
        .rounded(px(7.))
        .bg(fill)
        .hover(move |style| style.bg(hover))
        .text_size(px(13.))
        .font_weight(if primary {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::MEDIUM
        })
        .text_color(if primary {
            gpui::white()
        } else {
            theme.title()
        })
        .child(label.into())
}

impl BrowserApp {
    /// Whether others could read or change the tab's page: it came over
    /// plain HTTP, or the user continued past its certificate error. The
    /// URL pill and the site menu say so, as Chrome's address bar does.
    pub(crate) fn page_insecure(&self, tab_id: TabId, url: &str) -> bool {
        is_insecure(url) || self.notices.certificate_bypassed(tab_id)
    }

    /// The active tab's page state over the card, if it has one.
    pub(super) fn render_page_notice(
        &self,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let tab_id = self.active_tab_id()?;
        let shown = self.notices.shown(tab_id)?;
        let tab_url = self
            .state
            .tab(tab_id)
            .map(|tab| tab.url.clone())
            .unwrap_or_default();
        let column = div()
            .w(px(COLUMN_WIDTH))
            .max_w(relative(1.))
            .px(px(24.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .text_size(px(13.5))
            .line_height(px(20.))
            .text_color(theme.muted());
        let tile = |glyph: Icon, color: gpui::Hsla| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(ICON_TILE))
                .mb(px(4.))
                .rounded(px(10.))
                .bg(color.opacity(0.14))
                .child(icon(glyph, 22., color))
        };
        let title = |text: SharedString| {
            div()
                .text_size(px(19.))
                .line_height(px(25.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.title())
                .child(text)
        };
        let detail = |text: String| {
            div()
                .text_size(px(12.))
                .line_height(px(16.))
                .text_color(theme.tertiary())
                .child(text)
        };
        let buttons = || div().flex().gap(px(8.)).mt(px(10.));
        let content = match shown {
            Shown::Leave(leave) => {
                let id = leave.id;
                let (heading, action) = if leave.reload {
                    ("Reload site?", "Reload")
                } else {
                    ("Leave site?", "Leave")
                };
                column
                    .child(tile(Icon::CircleAlert, theme.focus()))
                    .child(title(heading.into()))
                    .child(div().child("Changes you made may not be saved."))
                    .child(
                        buttons()
                            .child(button("leave", action, true, theme).on_click(cx.listener(
                                move |this, _, window, cx| this.answer_leave(id, true, window, cx),
                            )))
                            .child(button("stay", "Cancel", false, theme).on_click(cx.listener(
                                move |this, _, window, cx| this.answer_leave(id, false, window, cx),
                            ))),
                    )
            }
            Shown::Crashed(reason) => {
                let host = site(&tab_url);
                let (heading, body) = match reason {
                    CrashReason::OutOfMemory => (
                        "This page ran out of memory",
                        format!(
                            "{host} needed more memory than it could get. Reload it to start it again."
                        ),
                    ),
                    CrashReason::Crashed => (
                        "This page crashed",
                        format!(
                            "Something went wrong while showing {host}. Reload it to start it again."
                        ),
                    ),
                };
                column
                    .child(tile(Icon::Warning, theme.error()))
                    .child(title(heading.into()))
                    .child(div().child(body))
                    .child(buttons().child(
                        button("reload-crashed", "Reload", true, theme).on_click(cx.listener(
                            move |this, _, window, cx| this.reload_page(tab_id, window, cx),
                        )),
                    ))
            }
            Shown::Unresponsive => {
                let host = site(&tab_url);
                column
                    .child(tile(Icon::Hourglass, theme.focus()))
                    .child(title("This page isn't responding".into()))
                    .child(div().child(format!(
                        "{host} may be busy. Wait for it, or reload it to start it again."
                    )))
                    .child(
                        buttons()
                            .child(button("wait", "Wait", true, theme).on_click(cx.listener(
                                move |this, _, window, cx| this.wait_for_page(tab_id, window, cx),
                            )))
                            .child(
                                button("reload-unresponsive", "Reload", false, theme).on_click(
                                    cx.listener(move |this, _, window, cx| {
                                        this.reload_page(tab_id, window, cx)
                                    }),
                                ),
                            ),
                    )
            }
            Shown::Certificate(certificate) => {
                let (id, host) = (certificate.id, site(&certificate.url));
                let continue_hover = theme.title();
                column
                    .child(tile(Icon::ShieldAlert, theme.error()))
                    .child(title(
                        format!("Your connection to {host} isn't private").into(),
                    ))
                    .child(div().child(format!(
                        "{} Someone may be trying to steal what you send it, such as passwords, \
                         messages or card numbers.",
                        certificate_text(certificate.code)
                    )))
                    .child(detail(certificate_error_name(certificate.code)))
                    .child(buttons().child(
                        button("back-to-safety", "Back to Safety", true, theme).on_click(
                            cx.listener(move |this, _, window, cx| {
                                this.answer_certificate(id, false, window, cx)
                            }),
                        ),
                    ))
                    .when(
                        certificate_continue_allowed(certificate.code, &certificate.url),
                        |column| {
                            column.child(
                                div()
                                    .id("continue-unsafe")
                                    .mt(px(6.))
                                    .text_size(px(12.5))
                                    .text_color(theme.tertiary())
                                    .underline()
                                    .hover(move |style| style.text_color(continue_hover))
                                    .child(format!("Continue to {host} (unsafe)"))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.answer_certificate(id, true, window, cx)
                                    })),
                            )
                        },
                    )
            }
            Shown::SignIn(request) => self.render_sign_in(request, column, theme, window, cx),
            Shown::Failed(failure) => {
                let (glyph, heading, body) = failure_text(failure.code, &site(&failure.url));
                column
                    .child(tile(glyph, theme.focus()))
                    .child(title(heading.into()))
                    .child(div().child(body))
                    .child(detail(failure.name.clone()))
                    .child(
                        buttons().child(button("try-again", "Try Again", true, theme).on_click(
                            cx.listener(move |this, _, window, cx| {
                                this.reload_page(tab_id, window, cx)
                            }),
                        )),
                    )
            }
        };
        Some(
            div()
                .absolute()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .key_context("PageNotice")
                .track_focus(&self.notices.focus_handle)
                .on_action(cx.listener(|this, _: &Confirm, window, cx| {
                    this.answer_page_notice(true, window, cx)
                }))
                .on_action(cx.listener(|this, _: &Cancel, window, cx| {
                    this.answer_page_notice(false, window, cx)
                }))
                .child(content),
        )
    }

    fn render_sign_in(
        &self,
        request: &SignIn,
        column: Div,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let site = &request.site;
        let host = site.label();
        let body = match (site.proxy, site.realm.is_empty()) {
            (true, _) => format!("The proxy {host} asks for a username and password."),
            (false, true) => format!("{host} asks for a username and password."),
            (false, false) => format!(
                "{host} asks for a username and password for “{}”.",
                site.realm
            ),
        };
        let focused = |input: &Entity<TextInput>| input.focus_handle(cx).is_focused(window);
        let fields = [
            (&request.username, focused(&request.username)),
            (&request.password, focused(&request.password)),
        ];
        let tab_fields = (request.username.clone(), request.password.clone());
        let field = |(input, focused): (&Entity<TextInput>, bool)| {
            div()
                .flex()
                .items_center()
                .h(px(FIELD_HEIGHT))
                .px(px(10.))
                .rounded(px(7.))
                .bg(theme.surface())
                .border_1()
                .border_color(if focused {
                    theme.focus()
                } else {
                    theme.ink(0.14)
                })
                .child(input.clone())
        };
        let [username, password] = fields;
        column
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(ICON_TILE))
                    .mb(px(4.))
                    .rounded(px(10.))
                    .bg(theme.focus().opacity(0.14))
                    .child(icon(Icon::Lock, 20., theme.focus())),
            )
            .child(
                div()
                    .text_size(px(19.))
                    .line_height(px(25.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.title())
                    .child("Sign in"),
            )
            .child(div().child(body))
            .when(!site.secure && !site.proxy, |column| {
                column.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_color(theme.error())
                        .child(icon(Icon::Warning, 14., theme.error()))
                        .child("Your connection to this site isn't private"),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .mt(px(6.))
                    // Tab and ⇧Tab move between the fields.
                    .on_key_down(cx.listener(move |_, event: &KeyDownEvent, window, cx| {
                        let keystroke = &event.keystroke;
                        if keystroke.key == "tab"
                            && !keystroke.modifiers.platform
                            && !keystroke.modifiers.control
                            && !keystroke.modifiers.alt
                        {
                            let (username, password) = &tab_fields;
                            let next = if username.focus_handle(cx).is_focused(window) {
                                password.clone()
                            } else {
                                username.clone()
                            };
                            next.update(cx, |input, cx| input.select_all_focus(window, cx));
                            cx.stop_propagation();
                        }
                    }))
                    .child(field(username))
                    .child(field(password)),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .mt(px(10.))
                    .child(
                        button("sign-in", "Sign In", true, theme)
                            .on_click(cx.listener(|this, _, window, cx| this.sign_in(window, cx))),
                    )
                    .child(button("cancel-sign-in", "Cancel", false, theme).on_click(
                        cx.listener(|this, _, window, cx| this.cancel_sign_in(window, cx)),
                    )),
            )
    }

    /// Return (`confirm`) or Esc on the card: the default button, or the
    /// way back.
    fn answer_page_notice(&mut self, confirm: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab_id) = self.active_tab_id() else {
            return;
        };
        enum Answer {
            Leave(u64),
            Reload,
            Wait,
            Certificate(u64),
            SignIn,
        }
        let answer = match self.notices.shown(tab_id) {
            Some(Shown::Leave(leave)) => Answer::Leave(leave.id),
            Some(Shown::Crashed(_) | Shown::Failed(_)) if confirm => Answer::Reload,
            Some(Shown::Unresponsive) => Answer::Wait,
            Some(Shown::Certificate(certificate)) => Answer::Certificate(certificate.id),
            Some(Shown::SignIn(_)) => Answer::SignIn,
            _ => return cx.propagate(),
        };
        match answer {
            Answer::Leave(id) => self.answer_leave(id, confirm, window, cx),
            Answer::Reload => self.reload_page(tab_id, window, cx),
            Answer::Wait => self.wait_for_page(tab_id, window, cx),
            Answer::Certificate(id) => self.answer_certificate(id, false, window, cx),
            Answer::SignIn if confirm => self.sign_in(window, cx),
            Answer::SignIn => self.cancel_sign_in(window, cx),
        }
    }

    /// After the active tab's notices change: its page shows, or doesn't,
    /// and a new question takes keyboard focus.
    fn notices_changed(&mut self, tab_id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let covered = self.is_loaded(tab_id) && self.notices.covers(tab_id);
        self.engine.borrow_mut().set_page_covered(tab_id, covered);
        if self.active_tab_id() == Some(tab_id) {
            if covered {
                self.focus_page_notice(window, cx);
            } else if self.notices.focus_handle.is_focused(window)
                || self
                    .notices
                    .sign_ins
                    .iter()
                    .any(|request| request.focused(window, cx))
            {
                // The page takes keyboard focus back (`activate_tab`).
                window.focus(&self.focus_handle);
            }
            cx.notify();
        }
    }

    /// Gives the active tab's notice keyboard focus: a sign-in's username,
    /// or its password once a username is there, else the card, so Return
    /// and Esc answer it. A rename or menu in progress keeps it, and so
    /// does a sign-in field being typed in.
    pub(crate) fn focus_page_notice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.renaming.is_some() || self.menu.is_some() {
            return;
        }
        let Some(tab_id) = self.active_tab_id() else {
            return;
        };
        let (username, password) = match self.notices.shown(tab_id) {
            None => return,
            Some(Shown::SignIn(request)) if request.focused(window, cx) => return,
            Some(Shown::SignIn(request)) => (request.username.clone(), request.password.clone()),
            Some(_) => return window.focus(&self.notices.focus_handle),
        };
        let style = field_style(&self.theme());
        for input in [&username, &password] {
            input.update(cx, |input, cx| input.set_style(style.clone(), cx));
        }
        let field = if username.read(cx).text().is_empty() {
            username
        } else {
            password
        };
        field.update(cx, |input, cx| input.select_all_focus(window, cx));
    }

    /// Moves keyboard focus off the fields of sign-ins about to go, to the
    /// window, before they are dropped; the next notice or the page takes
    /// it from there (`notices_changed`).
    fn release_sign_ins(&self, requests: &[SignIn], window: &mut Window, cx: &App) {
        if requests.iter().any(|request| request.focused(window, cx)) {
            window.focus(&self.focus_handle);
        }
    }

    // Engine events.

    /// A site or proxy asks the tab's page for a sign-in. It gets fields of
    /// its own, empty unless the same site asks the tab again after a
    /// wrong password: then the username stays.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sign_in_requested(
        &mut self,
        id: u64,
        tab_id: TabId,
        host: String,
        port: u16,
        realm: String,
        proxy: bool,
        secure: bool,
        auth_scheme: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(space) = self
            .state
            .space_of_tab(tab_id)
            .and_then(|space_id| self.state.space(space_id))
        else {
            return self.engine.borrow().respond_sign_in(id, None);
        };
        let style = field_style(&Theme::new(space.theme(), self.dark));
        let site = SignInSite {
            host,
            port,
            realm,
            proxy,
            secure,
            auth_scheme,
        };
        let retry = self
            .notices
            .retry_username
            .as_ref()
            .filter(|(retry_tab, retry_site, _)| *retry_tab == tab_id && *retry_site == site)
            .map(|(_, _, username)| username.clone())
            .unwrap_or_default();
        let field =
            |placeholder: &str, text: &str, masked: bool, window: &mut Window, cx: &mut App| {
                cx.new(|cx| {
                    let mut input = TextInput::new(window, cx);
                    input.set_placeholder(placeholder, cx);
                    input.set_masked(masked, cx);
                    input.set_style(style.clone(), cx);
                    input.set_text(text, cx);
                    input
                })
            };
        let username = field("Username", &retry, false, window, cx);
        let password = field("Password", "", true, window, cx);
        let _subscriptions = [&username, &password].map(|field| {
            cx.subscribe_in(field, window, |this, _, event, window, cx| match event {
                InputEvent::Submit(_) => this.sign_in(window, cx),
                InputEvent::Cancel => this.cancel_sign_in(window, cx),
                InputEvent::Changed(_) => {}
            })
        });
        self.notices.sign_ins.push(SignIn {
            id,
            tab_id,
            site,
            username,
            password,
            _subscriptions,
        });
        self.notices_changed(tab_id, window, cx);
    }

    pub(crate) fn certificate_error(
        &mut self,
        id: u64,
        tab_id: TabId,
        url: String,
        code: i32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(old) =
            self.notices
                .tab(tab_id)
                .certificate
                .replace(Certificate { id, url, code })
        {
            self.engine.borrow().respond_certificate(old.id, false);
        }
        self.notices_changed(tab_id, window, cx);
    }

    /// The page asks before it is left. A tab the user is closing comes
    /// forward with the question; a page in the background that navigates
    /// itself asks once its tab is shown.
    pub(crate) fn leave_requested(
        &mut self,
        id: u64,
        tab_id: TabId,
        closing: bool,
        reload: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.tab(tab_id).is_none() {
            return self.engine.borrow_mut().respond_leave(id, true);
        }
        if let Some(old) = self.notices.tab(tab_id).leave.replace(Leave { id, reload }) {
            self.engine.borrow_mut().respond_leave(old.id, false);
        }
        if closing && self.active_tab_id() != Some(tab_id) {
            self.select_tab(tab_id, window, cx);
        }
        self.notices_changed(tab_id, window, cx);
    }

    /// A request ended without an answer.
    pub(crate) fn page_request_dismissed(
        &mut self,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut changed = None;
        if let Some(index) = self
            .notices
            .sign_ins
            .iter()
            .position(|request| request.id == id)
        {
            // What was typed for it goes with it.
            let request = self.notices.sign_ins.remove(index);
            self.release_sign_ins(std::slice::from_ref(&request), window, cx);
            changed = Some(request.tab_id);
        }
        for (&tab_id, notices) in &mut self.notices.tabs {
            if notices
                .certificate
                .as_ref()
                .is_some_and(|request| request.id == id)
            {
                notices.certificate = None;
                changed = Some(tab_id);
            }
            if notices
                .leave
                .as_ref()
                .is_some_and(|request| request.id == id)
            {
                notices.leave = None;
                changed = Some(tab_id);
            }
        }
        if let Some(tab_id) = changed {
            self.notices_changed(tab_id, window, cx);
        }
    }

    pub(crate) fn page_crashed(
        &mut self,
        tab_id: TabId,
        reason: CrashReason,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_loaded(tab_id) {
            return;
        }
        let notices = self.notices.tab(tab_id);
        if std::mem::take(&mut notices.reload_when_ended) {
            // "Reload" on a page that stopped responding ended it; now it
            // starts again.
            notices.unresponsive = false;
            self.engine.borrow().reload(tab_id);
        } else {
            // Its questions went with it; the engine dismisses them.
            *notices = TabNotices {
                crashed: Some(reason),
                unsafe_host: notices.unsafe_host.take(),
                ..TabNotices::default()
            };
        }
        self.notices_changed(tab_id, window, cx);
    }

    pub(crate) fn page_responsive(
        &mut self,
        tab_id: TabId,
        responsive: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_loaded(tab_id) {
            return;
        }
        let notices = self.notices.tab(tab_id);
        notices.unresponsive = !responsive;
        if responsive {
            notices.reload_when_ended = false;
        }
        self.notices_changed(tab_id, window, cx);
    }

    /// A main-frame load failed: the card explains it until the tab shows
    /// a page again.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn page_load_failed(
        &mut self,
        tab_id: TabId,
        url: String,
        code: i32,
        name: String,
        can_go_back: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let notices = self.notices.tab(tab_id);
        notices.certificate = None;
        if std::mem::take(&mut notices.back_after_failure) {
            if can_go_back {
                // Back to Safety: the refused page leaves for the one before.
                self.engine.borrow().back(tab_id);
            } else {
                // Nothing before it: the tab was opened for the refused
                // page. It closes, or unloads if pinned, as Chrome leaves
                // for a new tab, rather than show an error whose Try Again
                // brings the warning back.
                self.close_tab(tab_id, cx);
            }
            return;
        }
        let name = name.trim_start_matches("net::").to_owned();
        notices.failure = Some(LoadFailure { url, code, name });
        self.notices_changed(tab_id, window, cx);
    }

    /// The tab shows another document: notices about the last one end.
    pub(crate) fn page_document_changed(
        &mut self,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.notices.popups_reported.remove(&tab_id);
        self.notices.blocked_popups.remove(&tab_id);
        self.forget_retry_username(tab_id);
        let covered = self.notices.covers(tab_id);
        if let Some(notices) = self.notices.tabs.get_mut(&tab_id) {
            notices.failure = None;
            notices.back_after_failure = false;
            notices.crashed = None;
            notices.unresponsive = false;
            notices.reload_when_ended = false;
        }
        if covered != self.notices.covers(tab_id) {
            self.notices_changed(tab_id, window, cx);
        }
    }

    /// The tab's address changed: a certificate error continued past
    /// counts only on its site.
    pub(crate) fn page_address_changed(&mut self, tab_id: TabId, url: &str) {
        if let Some(notices) = self.notices.tabs.get_mut(&tab_id)
            && notices
                .unsafe_host
                .as_deref()
                .is_some_and(|host| host != site(url))
        {
            notices.unsafe_host = None;
        }
    }

    /// Reports a blocked pop-up once per page, for the tab on screen, with
    /// "Open" for the last one a tab can show (`url`, else empty).
    pub(crate) fn popup_blocked(&mut self, tab_id: TabId, url: String, cx: &mut Context<Self>) {
        if !url.is_empty() {
            self.notices.blocked_popups.insert(tab_id, url);
        }
        if self.active_tab_id() != Some(tab_id) || !self.notices.popups_reported.insert(tab_id) {
            return;
        }
        // News, not a failure: the page went on without it.
        if self.notices.blocked_popups.contains_key(&tab_id) {
            self.show_action_toast(
                ToastKind::Info,
                "Pop-up blocked",
                "Open",
                move |this, cx| this.open_blocked_popup(tab_id, cx),
                cx,
            );
        } else {
            self.show_toast_kind(ToastKind::Info, "Pop-up blocked", cx);
        }
    }

    /// "Open" on the pop-up-blocked notice: the page's last blocked pop-up
    /// opens as a tab after it, in its space.
    fn open_blocked_popup(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        self.toast = None;
        if let Some(url) = self.notices.blocked_popups.remove(&tab_id)
            && self.state.tab(tab_id).is_some()
        {
            self.open_from_page(tab_id, &url, false, cx);
        }
        cx.notify();
    }

    /// Stop (⌘. or the × in Reload's place) ends what the tab's load waits
    /// on, as in Chrome: its sign-ins are cancelled and a certificate
    /// warning is refused, so the load fails there and its card says why.
    pub(crate) fn stop_page_questions(
        &mut self,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sign_ins: Vec<SignIn> = self
            .notices
            .sign_ins
            .extract_if(.., |request| request.tab_id == tab_id)
            .collect();
        let certificate = self
            .notices
            .tabs
            .get_mut(&tab_id)
            .and_then(|notices| notices.certificate.take());
        if sign_ins.is_empty() && certificate.is_none() {
            return;
        }
        self.release_sign_ins(&sign_ins, window, cx);
        for request in sign_ins {
            self.engine.borrow().respond_sign_in(request.id, None);
        }
        if let Some(certificate) = certificate {
            self.engine
                .borrow()
                .respond_certificate(certificate.id, false);
        }
        self.notices_changed(tab_id, window, cx);
    }

    /// The tab shows another document, or closes: a wrong password can no
    /// longer bring its sign-in back.
    fn forget_retry_username(&mut self, tab_id: TabId) {
        if self
            .notices
            .retry_username
            .as_ref()
            .is_some_and(|(retry_tab, ..)| *retry_tab == tab_id)
        {
            self.notices.retry_username = None;
        }
    }

    /// The tab's page closed or unloaded: its questions are answered the
    /// way that lets it go, and its notices end.
    pub(crate) fn end_page_notices(&mut self, tab_id: TabId) {
        let engine = self.engine.clone();
        let mut engine = engine.borrow_mut();
        for request in self
            .notices
            .sign_ins
            .extract_if(.., |request| request.tab_id == tab_id)
        {
            engine.respond_sign_in(request.id, None);
        }
        if let Some(notices) = self.notices.tabs.remove(&tab_id) {
            if let Some(certificate) = notices.certificate {
                engine.respond_certificate(certificate.id, false);
            }
            if let Some(leave) = notices.leave {
                engine.respond_leave(leave.id, true);
            }
        }
        self.notices.leaving.remove(&tab_id);
        self.notices.popups_reported.remove(&tab_id);
        self.notices.blocked_popups.remove(&tab_id);
        engine.set_page_covered(tab_id, false);
        drop(engine);
        self.forget_retry_username(tab_id);
    }

    // Answers.

    fn shown_sign_in(&self) -> Option<&SignIn> {
        let tab_id = self.active_tab_id()?;
        match self.notices.shown(tab_id)? {
            Shown::SignIn(request) => Some(request),
            _ => None,
        }
    }

    /// Signs in with the fields' username and password. Other requests for
    /// the same sign-in get the same answer.
    fn sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shown) = self.shown_sign_in() else {
            return;
        };
        let username = shown.username.read(cx).text().to_owned();
        let password = shown.password.read(cx).text().to_owned();
        self.answer_sign_in(Some((&username, &password)), window, cx);
    }

    fn cancel_sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.answer_sign_in(None, window, cx);
    }

    /// Answers the sign-in shown, and the tab's others for the same site.
    /// Their fields go with them.
    fn answer_sign_in(
        &mut self,
        credentials: Option<(&str, &str)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(shown) = self.shown_sign_in() else {
            return;
        };
        let (shown_id, tab_id) = (shown.id, shown.tab_id);
        let index = self
            .notices
            .sign_ins
            .iter()
            .position(|request| request.id == shown_id)
            .expect("the sign-in shown");
        let shown = self.notices.sign_ins.remove(index);
        let mut answered: Vec<SignIn> = self
            .notices
            .sign_ins
            .extract_if(.., |request| request.same_as(&shown))
            .collect();
        match credentials {
            Some((username, _)) => {
                self.notices.retry_username =
                    Some((tab_id, shown.site.clone(), username.to_owned()));
            }
            None => self.forget_retry_username(tab_id),
        }
        answered.insert(0, shown);
        self.release_sign_ins(&answered, window, cx);
        for request in answered {
            self.engine
                .borrow()
                .respond_sign_in(request.id, credentials);
        }
        self.notices_changed(tab_id, window, cx);
    }

    fn answer_certificate(
        &mut self,
        id: u64,
        proceed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((&tab_id, notices)) = self.notices.tabs.iter_mut().find(|(_, notices)| {
            notices
                .certificate
                .as_ref()
                .is_some_and(|request| request.id == id)
        }) else {
            return;
        };
        let certificate = notices.certificate.take().expect("the certificate shown");
        // The engine refuses Continue where the card doesn't offer it.
        let proceed = proceed && certificate_continue_allowed(certificate.code, &certificate.url);
        if proceed {
            notices.unsafe_host = Some(site(&certificate.url));
        } else {
            // The load fails; then the tab goes back if it can.
            notices.back_after_failure = true;
        }
        self.engine.borrow().respond_certificate(id, proceed);
        self.notices_changed(tab_id, window, cx);
    }

    fn answer_leave(&mut self, id: u64, leave: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((&tab_id, notices)) = self.notices.tabs.iter_mut().find(|(_, notices)| {
            notices
                .leave
                .as_ref()
                .is_some_and(|request| request.id == id)
        }) else {
            return;
        };
        notices.leave = None;
        if !leave {
            // Staying keeps the tab where it is.
            self.notices.leaving.remove(&tab_id);
        }
        self.engine.borrow_mut().respond_leave(id, leave);
        self.notices_changed(tab_id, window, cx);
    }

    fn wait_for_page(&mut self, tab_id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        self.notices.tab(tab_id).unresponsive = false;
        self.engine.borrow().respond_unresponsive(tab_id, true);
        self.notices_changed(tab_id, window, cx);
    }

    /// Reload on the card: a crashed page starts again, one that stopped
    /// responding is ended first, and a failed load is tried again.
    fn reload_page(&mut self, tab_id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        self.reload(tab_id, cx);
        self.notices_changed(tab_id, window, cx);
    }

    /// For `BrowserApp::reload`: what reloading means for the tab's
    /// notices. False when the reload already happened here.
    pub(crate) fn reload_notices(&mut self, tab_id: TabId) -> bool {
        let Some(notices) = self.notices.tabs.get_mut(&tab_id) else {
            return true;
        };
        if notices.unresponsive {
            // A frozen page can't unload; end it, then reload.
            notices.reload_when_ended = true;
            self.engine.borrow().respond_unresponsive(tab_id, false);
            return false;
        }
        let crashed = notices.crashed.take().is_some();
        if crashed {
            let covered = self.notices.covers(tab_id);
            self.engine
                .borrow_mut()
                .set_page_covered(tab_id, covered && self.is_loaded(tab_id));
        }
        true
    }

    /// For a tab's row or tile: a red camera or microphone while its page
    /// uses them, and a speaker while it plays sound or is muted, which
    /// mutes or unmutes it. `width` is the sidebar's, for the tooltip.
    pub(super) fn render_tab_badges(
        &self,
        tab_id: TabId,
        size: f32,
        width: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        if !self.is_loaded(tab_id) {
            return None;
        }
        let page = self.page(tab_id);
        let capture = if page.camera {
            Some(Icon::Camera)
        } else if page.microphone {
            Some(Icon::Mic)
        } else {
            None
        };
        let sound = (page.audible || page.muted).then_some(page.muted);
        if capture.is_none() && sound.is_none() {
            return None;
        }
        let hover = theme.ink(if theme.dark { 0.10 } else { 0.07 });
        let speaker = sound.map(|muted| {
            let (glyph, label) = if muted {
                (Icon::SpeakerOff, "Unmute Tab")
            } else {
                (Icon::Speaker, "Mute Tab")
            };
            press_only(
                div()
                    .id(("mute", tab_id))
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(size + 6.))
                    .rounded(px(5.))
                    .hover(move |style| style.bg(hover))
                    .child(icon(glyph, size, theme.icon())),
            )
            .maybe_tooltip(self.tooltip(label, "", theme, width))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.toggle_mute(tab_id, cx);
            }))
        });
        Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(2.))
                .children(capture.map(|glyph| icon(glyph, size, theme.error())))
                .children(speaker),
        )
    }

    // Leaving pages.

    /// Closes, unloads or moves a tab, once its page lets go: a page with
    /// unsaved changes asks first ("Leave site?"), and the tab stays until
    /// the user leaves. Asking again while it asks doesn't wait.
    pub fn leave_tab(&mut self, tab_id: TabId, leaving: Leaving, cx: &mut Context<Self>) {
        let asked = self.notices.leaving.remove(&tab_id).is_some();
        // A tab still downloading keeps its browser (`drop_browser`).
        let ask = !asked && self.is_loaded(tab_id) && !self.downloads.running_in(tab_id);
        if ask && !self.engine.borrow_mut().close_tab_asking(tab_id) {
            self.notices.leaving.insert(tab_id, leaving);
            return;
        }
        self.finish_leaving(tab_id, leaving, cx);
    }

    /// The tab's page closed. True if it was leaving: the tab closed,
    /// unloaded or moved as asked.
    pub(crate) fn page_left(&mut self, tab_id: TabId, cx: &mut Context<Self>) -> bool {
        let Some(leaving) = self.notices.leaving.remove(&tab_id) else {
            return false;
        };
        self.finish_leaving(tab_id, leaving, cx);
        true
    }

    fn finish_leaving(&mut self, tab_id: TabId, leaving: Leaving, cx: &mut Context<Self>) {
        match leaving {
            Leaving::Close => self.remove_tab(tab_id, cx),
            Leaving::Unload => self.unload_tab(tab_id, cx),
            Leaving::Move(space_id) => self.move_tab_to_space(tab_id, space_id, cx),
        }
    }
}
