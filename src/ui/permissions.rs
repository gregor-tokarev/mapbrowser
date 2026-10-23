//! Permission requests: a card in the sidebar, above the bottom bar, asks
//! whether a site may use the camera, microphone, location and so on. Only
//! the active tab's requests show; the others wait until their tab does.
//! Answers hold for the rest of the session, per space, site and
//! permission (Chromium also keeps prompt answers, such as location, in
//! the space's profile; camera and microphone it never keeps). The site
//! menu beside the address lists both and resets them.
//!
//! The card cuts a long host from its start, so the domain that owns it
//! always shows. Block, Allow and × are reached with Tab and pressed with
//! Return or Space. None of them takes keyboard focus by itself, so typing
//! can't answer a request that appears under the fingers.

use super::{
    Tooltip, UI_FONT,
    elide::{Cut, elided},
    icons::{Icon, icon},
    press_only, shadow, text_width,
    theme::Theme,
};
use crate::{
    app::BrowserApp,
    engine::{Permission, PermissionDecision, site_origin},
    favicons::host_of,
    model::{SpaceId, TabId},
};
use gpui::{
    Context, Div, Font, FontWeight, InteractiveElement, KeyDownEvent, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, font, px,
};
use std::collections::HashMap;

const CARD_PADDING: f32 = 12.;
const ICON_TILE: f32 = 28.;
const ICON_GAP: f32 = 10.;
const CLOSE_BUTTON: f32 = 20.;

pub struct PermissionRequest {
    pub id: u64,
    pub tab_id: TabId,
    space_id: SpaceId,
    origin: String,
    permissions: Vec<Permission>,
}

#[derive(Default)]
pub struct PermissionPrompts {
    /// Unanswered requests, oldest first.
    queue: Vec<PermissionRequest>,
    /// This session's answers, allowed or blocked, by space, site
    /// (`site_key`) and permission.
    decisions: HashMap<(SpaceId, String, Permission), bool>,
}

/// Screen sharing is asked every time, as in Chrome: what is on screen
/// changes.
fn remembered(permission: Permission) -> bool {
    permission != Permission::Screen
}

/// Chromium keeps the answers to everything but the camera, microphone and
/// screen in the space's profile, also after quitting.
fn kept_by_profile(permission: Permission) -> bool {
    !matches!(
        permission,
        Permission::Camera | Permission::Microphone | Permission::Screen
    )
}

/// The site an origin or address belongs to, as answers are filed:
/// requests name origins with a trailing slash, tabs whole addresses.
fn site_key(origin: &str) -> String {
    site_origin(origin).unwrap_or_else(|| origin.trim_end_matches('/').to_owned())
}

impl PermissionPrompts {
    /// The session's answer to all of `permissions`: blocked if any is,
    /// allowed if all are.
    fn decided(&self, space_id: SpaceId, origin: &str, permissions: &[Permission]) -> Option<bool> {
        let site = site_key(origin);
        let mut allowed = true;
        for &permission in permissions {
            match remembered(permission)
                .then(|| self.decisions.get(&(space_id, site.clone(), permission)))
                .flatten()
            {
                Some(false) => return Some(false),
                Some(true) => {}
                None => allowed = false,
            }
        }
        allowed.then_some(true)
    }

    fn remember(&mut self, request: &PermissionRequest, allow: bool) {
        let site = site_key(&request.origin);
        for &permission in request.permissions.iter().filter(|&&p| remembered(p)) {
            self.decisions
                .insert((request.space_id, site.clone(), permission), allow);
        }
    }

    /// This session's answers for `site` in a space.
    fn site_decisions(&self, space_id: SpaceId, site: &str) -> Vec<(Permission, bool)> {
        self.decisions
            .iter()
            .filter(|((space, key, _), _)| *space == space_id && key == site)
            .map(|(&(_, _, permission), &allow)| (permission, allow))
            .collect()
    }

    fn forget(&mut self, space_id: SpaceId, site: &str, permission: Permission) {
        self.decisions
            .remove(&(space_id, site.to_owned(), permission));
    }

    /// Queued requests the session's answers now settle.
    fn take_decided(&mut self) -> Vec<(u64, bool)> {
        let settled: Vec<(u64, bool)> = self
            .queue
            .iter()
            .filter_map(|request| {
                let allow =
                    self.decided(request.space_id, &request.origin, &request.permissions)?;
                Some((request.id, allow))
            })
            .collect();
        self.queue
            .retain(|request| !settled.iter().any(|&(id, _)| id == request.id));
        settled
    }

    fn remove(&mut self, id: u64) -> Option<PermissionRequest> {
        let index = self.queue.iter().position(|request| request.id == id)?;
        Some(self.queue.remove(index))
    }

    /// The request the card shows: the oldest of the tab on screen.
    pub fn shown(&self, tab_id: Option<TabId>) -> Option<&PermissionRequest> {
        let tab_id = tab_id?;
        self.queue.iter().find(|request| request.tab_id == tab_id)
    }
}

/// What a site wants to do with `permission`, after "wants to".
fn phrase(permission: Permission) -> &'static str {
    match permission {
        Permission::Camera => "use your camera",
        Permission::Microphone => "use your microphone",
        Permission::Screen => "share your screen",
        Permission::CapturedSurfaceControl => "scroll and zoom what you share",
        Permission::Location => "know your location",
        Permission::Notifications => "show notifications",
        Permission::Clipboard => "see text and images you copy",
        Permission::Midi => "use your MIDI devices",
        Permission::Downloads => "download multiple files",
        Permission::StorageAccess => "use its cookies and site data",
        Permission::DiskQuota => "store more data on this Mac",
        Permission::LocalFonts => "use the fonts on this Mac",
        Permission::WindowManagement => "manage windows on all your displays",
        Permission::IdleDetection => "know when you're using this Mac",
        Permission::PointerLock => "lock and hide your pointer",
        Permission::KeyboardLock => "capture your keyboard shortcuts",
        Permission::LocalNetwork => "find and connect to devices on your local network",
        Permission::LoopbackNetwork => "connect to apps and services on this Mac",
        Permission::Sensors => "use your motion sensors",
        Permission::ProtectedMedia => "play protected content",
        Permission::FileSystem => "keep editing files you opened with it",
        Permission::ProtocolHandler => "open certain kinds of links",
        Permission::AppInstallation => "install itself as an app",
        Permission::IdentityProvider => "sign you in to other sites",
        Permission::VirtualReality => "use your virtual reality devices",
        Permission::AugmentedReality => "create a 3D map of your surroundings",
        Permission::HandTracking => "track your hands",
        Permission::Other => "use other features of this Mac",
    }
}

/// The site menu's name for `permission`.
pub fn permission_name(permission: Permission) -> &'static str {
    match permission {
        Permission::Camera => "Camera",
        Permission::Microphone => "Microphone",
        Permission::Screen => "Screen Sharing",
        Permission::CapturedSurfaceControl => "Shared Tab Control",
        Permission::Location => "Location",
        Permission::Notifications => "Notifications",
        Permission::Clipboard => "Clipboard",
        Permission::Midi => "MIDI Devices",
        Permission::Downloads => "Automatic Downloads",
        Permission::StorageAccess => "Embedded Site Data",
        Permission::DiskQuota => "Extra Storage",
        Permission::LocalFonts => "Fonts",
        Permission::WindowManagement => "Window Management",
        Permission::IdleDetection => "Idle Detection",
        Permission::PointerLock => "Pointer Lock",
        Permission::KeyboardLock => "Keyboard Lock",
        Permission::LocalNetwork => "Local Network",
        Permission::LoopbackNetwork => "Apps on This Mac",
        Permission::Sensors => "Motion Sensors",
        Permission::ProtectedMedia => "Protected Content",
        Permission::FileSystem => "File Editing",
        Permission::ProtocolHandler => "Link Handling",
        Permission::AppInstallation => "App Installation",
        Permission::IdentityProvider => "Sign-In Provider",
        Permission::VirtualReality => "Virtual Reality",
        Permission::AugmentedReality => "Augmented Reality",
        Permission::HandTracking => "Hand Tracking",
        Permission::Other => "Other Features",
    }
}

/// "use your camera and microphone", "know your location"…
fn request_text(permissions: &[Permission]) -> String {
    let both = [Permission::Camera, Permission::Microphone]
        .iter()
        .all(|permission| permissions.contains(permission));
    let phrases: Vec<&str> = permissions
        .iter()
        .filter_map(|&permission| {
            Some(match permission {
                Permission::Camera if both => "use your camera and microphone",
                Permission::Microphone if both => return None,
                permission => phrase(permission),
            })
        })
        .collect();
    let text = match phrases.split_last() {
        Some((last, [])) => last.to_string(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    };
    format!("wants to {text}")
}

pub fn permission_icon(permission: Permission) -> Icon {
    match permission {
        Permission::Camera => Icon::Camera,
        Permission::Microphone => Icon::Mic,
        Permission::Screen | Permission::CapturedSurfaceControl | Permission::WindowManagement => {
            Icon::Monitor
        }
        Permission::Location => Icon::MapPin,
        Permission::Notifications => Icon::Bell,
        Permission::Clipboard => Icon::Clipboard,
        Permission::Downloads => Icon::ArrowDown,
        Permission::LocalNetwork | Permission::LoopbackNetwork => Icon::Globe,
        Permission::FileSystem => Icon::File,
        Permission::ProtocolHandler => Icon::Link,
        _ => Icon::Shield,
    }
}

fn request_icon(permissions: &[Permission]) -> Icon {
    permissions
        .first()
        .map_or(Icon::Shield, |&permission| permission_icon(permission))
}

/// How long an answer to `permissions` holds, in the space called `space`.
fn answer_lasts(permissions: &[Permission], space: &str) -> String {
    if !permissions.iter().any(|&permission| remembered(permission)) {
        "You'll be asked again next time.".to_owned()
    } else if permissions
        .iter()
        .any(|&permission| kept_by_profile(permission))
    {
        format!("Your answer is kept for this site in {space}.")
    } else {
        format!("Your answer holds in {space} until you quit.")
    }
}

/// Return or Space on a focused button, without modifiers.
fn presses(event: &KeyDownEvent) -> bool {
    matches!(event.keystroke.key.as_str(), "enter" | "space")
        && !event.keystroke.modifiers.modified()
}

impl BrowserApp {
    pub(crate) fn permission_requested(
        &mut self,
        id: u64,
        tab_id: TabId,
        origin: String,
        permissions: Vec<Permission>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(space_id) = self.state.space_of_tab(tab_id) else {
            self.respond_permission(id, PermissionDecision::Dismiss);
            return;
        };
        if let Some(allow) = self.permissions.decided(space_id, &origin, &permissions) {
            self.respond_permission(id, decision(allow));
            return;
        }
        self.permissions.queue.push(PermissionRequest {
            id,
            tab_id,
            space_id,
            origin,
            permissions,
        });
        if self.permission_shown().is_some_and(|shown| shown.id == id) {
            // Esc reaches the card through the shell's key context. Only
            // GPUI's focus moves: a page keeps its keyboard focus.
            if self.renaming.is_none() && self.menu.is_none() {
                window.focus(&self.focus_handle);
            }
            cx.notify();
        }
    }

    /// The request ended without an answer: the page went away.
    pub(crate) fn permission_dismissed(&mut self, id: u64, cx: &mut Context<Self>) {
        let shown = self.permission_shown().is_some_and(|shown| shown.id == id);
        if self.permissions.remove(id).is_some() && shown {
            cx.notify();
        }
    }

    pub fn answer_permission(
        &mut self,
        id: u64,
        answer: PermissionDecision,
        cx: &mut Context<Self>,
    ) {
        let Some(request) = self.permissions.remove(id) else {
            return;
        };
        self.respond_permission(id, answer);
        if answer != PermissionDecision::Dismiss {
            self.permissions
                .remember(&request, answer == PermissionDecision::Allow);
            for (id, allow) in self.permissions.take_decided() {
                self.respond_permission(id, decision(allow));
            }
        }
        cx.notify();
    }

    /// An answer from the card's buttons. The button had keyboard focus if
    /// it was pressed with the keyboard: it goes back to the page.
    fn answer_from_card(
        &mut self,
        id: u64,
        answer: PermissionDecision,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.answer_permission(id, answer, cx);
        window.focus(&self.focus_handle);
        self.focus_page(window, cx);
    }

    fn respond_permission(&self, id: u64, answer: PermissionDecision) {
        self.engine.borrow().respond_permission(id, answer);
    }

    pub fn permission_shown(&self) -> Option<&PermissionRequest> {
        self.permissions.shown(self.active_tab_id())
    }

    /// Esc: closes the open menu, else dismisses the permission card, else
    /// leaves page fullscreen (on macOS the key monitor sees Esc first).
    pub fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.is_some() {
            self.close_menu(window, cx);
        } else if let Some(id) = self.permission_shown().map(|request| request.id) {
            self.answer_permission(id, PermissionDecision::Dismiss, cx);
        } else if self.fullscreen {
            self.exit_page_fullscreen();
        }
    }

    /// Dismisses every request of the tab on screen, for hiding the sidebar
    /// they hold open: the sites can ask again.
    pub(crate) fn dismiss_shown_permissions(&mut self, cx: &mut Context<Self>) {
        while let Some(id) = self.permission_shown().map(|request| request.id) {
            self.answer_permission(id, PermissionDecision::Dismiss, cx);
        }
    }

    /// Tab and ⇧Tab move between the card's buttons while it shows. An
    /// open menu keeps them for its own choices.
    pub(crate) fn tab_through_card(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let modifiers = event.keystroke.modifiers;
        if event.keystroke.key != "tab"
            || modifiers.control
            || modifiers.alt
            || modifiers.platform
            || self.menu.is_some()
            || self.permission_shown().is_none()
        {
            return;
        }
        if modifiers.shift {
            window.focus_prev();
        } else {
            window.focus_next();
        }
        cx.stop_propagation();
    }

    /// The tab's site ("https://host") and space, for the site menu.
    fn site_of_tab(&self, tab_id: TabId) -> Option<(SpaceId, String)> {
        let space_id = self.state.space_of_tab(tab_id)?;
        let site = site_origin(&self.state.tab(tab_id)?.url)?;
        Some((space_id, site))
    }

    /// The answers the tab's site has in its space, allowed or blocked:
    /// this session's and those the space's profile keeps.
    pub(crate) fn site_permissions(&self, tab_id: TabId) -> Vec<(Permission, bool)> {
        let Some((space_id, site)) = self.site_of_tab(tab_id) else {
            return Vec::new();
        };
        let mut answers = self.permissions.site_decisions(space_id, &site);
        for (permission, allow) in self.engine.borrow().site_permissions(space_id, &site) {
            if !answers.iter().any(|&(known, _)| known == permission) {
                answers.push((permission, allow));
            }
        }
        answers.sort();
        answers
    }

    /// Whether the tab's site was blocked from something this session,
    /// which the address shows.
    pub(crate) fn site_blocked_something(&self, tab_id: TabId) -> bool {
        self.site_of_tab(tab_id).is_some_and(|(space_id, site)| {
            self.permissions
                .site_decisions(space_id, &site)
                .iter()
                .any(|&(_, allow)| !allow)
        })
    }

    /// Forgets the answer for `permission` of the tab's site, this
    /// session's and the profile's: the site asks again next time.
    pub(crate) fn reset_site_permission(
        &mut self,
        tab_id: TabId,
        permission: Permission,
        cx: &mut Context<Self>,
    ) {
        let Some((space_id, site)) = self.site_of_tab(tab_id) else {
            return;
        };
        self.permissions.forget(space_id, &site, permission);
        self.engine
            .borrow_mut()
            .reset_site_permission(space_id, &site, permission);
        cx.notify();
    }

    /// Deletes the cookies and stored data of the tab's site in its space;
    /// the page reloads without them.
    pub(crate) fn clear_site_data(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let Some((space_id, site)) = self.site_of_tab(tab_id) else {
            return;
        };
        self.error = None;
        self.engine
            .borrow_mut()
            .clear_site_data(space_id, tab_id, &site);
        let host = host_of(&site).unwrap_or(site);
        self.show_toast(format!("Cleared data for {host}"), cx);
    }

    /// "meet.google.com wants to use your camera and microphone", with
    /// Block and Allow, for a sidebar `width` wide.
    pub(super) fn render_permission_card(
        &self,
        width: f32,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let request = self.permission_shown()?;
        let id = request.id;
        let host = host_of(&request.origin).unwrap_or_else(|| request.origin.clone());
        let origin = site_key(&request.origin);
        let space = self
            .state
            .space(request.space_id)
            .map_or_else(|| "this space".to_owned(), |space| space.name.clone());
        // The text wraps within a known width: the card's, less its padding
        // and the icon. The close button sits in the corner, beside the host.
        let column = width - 16. - 2. * CARD_PADDING - ICON_TILE - ICON_GAP;
        let host_room = column - CLOSE_BUTTON;
        // A host too long for its line splits in two: as much of its start
        // as fits, cut from the left, above the domain that owns it
        // ("attacker.example"), which is always shown whole.
        let bold = Font {
            weight: FontWeight::SEMIBOLD,
            ..font(UI_FONT)
        };
        let (first_line, domain) = match psl::domain_str(&host) {
            Some(domain)
                if domain.len() < host.len()
                    && text_width(&host, bold, 13., window) > host_room =>
            {
                let domain = domain.to_owned();
                (host[..host.len() - domain.len()].to_owned(), Some(domain))
            }
            _ => (host, None),
        };
        let button = |label: &'static str, answer: PermissionDecision, index: isize| {
            let primary = answer == PermissionDecision::Allow;
            let (fill, hover) = if primary {
                (theme.focus(), theme.focus().opacity(0.88))
            } else {
                (theme.control(), theme.control_hover())
            };
            // A ring that shows on the button's own colour.
            let ring = if primary {
                theme.title()
            } else {
                theme.focus()
            };
            press_only(
                div()
                    .id(label)
                    .tab_index(index)
                    .flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .h(px(28.))
                    .rounded(px(7.))
                    .border_2()
                    .border_color(gpui::transparent_black())
                    .focus(move |style| style.border_color(ring))
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
                    .child(label),
            )
            .on_click(
                cx.listener(move |this, _, window, cx| {
                    this.answer_from_card(id, answer, window, cx)
                }),
            )
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                if presses(event) {
                    cx.stop_propagation();
                    this.answer_from_card(id, answer, window, cx);
                }
            }))
        };
        let close_hover = theme.hover();
        let close_ring = theme.focus();
        Some(
            div()
                .relative()
                .mx(px(8.))
                .p(px(CARD_PADDING))
                .flex()
                .flex_col()
                .gap(px(10.))
                .rounded(px(10.))
                .bg(theme.surface())
                .border_1()
                .border_color(theme.ink(0.08))
                .shadow(vec![shadow(4., 14., 0.12)])
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(ICON_GAP))
                        .child(
                            div()
                                .flex()
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .size(px(ICON_TILE))
                                .rounded(px(7.))
                                .bg(theme.focus().opacity(0.16))
                                .child(icon(
                                    request_icon(&request.permissions),
                                    16.,
                                    theme.focus(),
                                )),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .w(px(column.max(40.)))
                                .text_size(px(13.))
                                .line_height(px(17.))
                                // The whole origin on hover: the lines show
                                // the end of a long host.
                                .child(
                                    div()
                                        .id("permission-host")
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.title())
                                        .tooltip(Tooltip::build_wrapping(origin, "", theme, width))
                                        .child(
                                            elided(first_line, Cut::Start, 17.)
                                                .mr(px(CLOSE_BUTTON)),
                                        )
                                        .children(domain),
                                )
                                .child(
                                    div()
                                        .text_color(theme.muted())
                                        .child(request_text(&request.permissions)),
                                )
                                .child(
                                    div()
                                        .mt(px(4.))
                                        .text_size(px(11.5))
                                        .line_height(px(15.))
                                        .text_color(theme.tertiary())
                                        .child(answer_lasts(&request.permissions, &space)),
                                ),
                        ),
                )
                .child(
                    press_only(
                        div()
                            .id("permission-close")
                            .tab_index(2)
                            .absolute()
                            .top(px(8.))
                            .right(px(8.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(CLOSE_BUTTON))
                            .rounded(px(5.))
                            .border_2()
                            .border_color(gpui::transparent_black())
                            .focus(move |style| style.border_color(close_ring))
                            .hover(move |style| style.bg(close_hover))
                            .child(icon(Icon::Close, 15., theme.icon())),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.answer_from_card(id, PermissionDecision::Dismiss, window, cx)
                    }))
                    .on_key_down(cx.listener(
                        move |this, event: &KeyDownEvent, window, cx| {
                            if presses(event) {
                                cx.stop_propagation();
                                this.answer_from_card(id, PermissionDecision::Dismiss, window, cx);
                            }
                        },
                    )),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(button("Block", PermissionDecision::Block, 0))
                        .child(button("Allow", PermissionDecision::Allow, 1)),
                )
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation()),
        )
    }
}

fn decision(allow: bool) -> PermissionDecision {
    if allow {
        PermissionDecision::Allow
    } else {
        PermissionDecision::Block
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Permission::*;

    fn request(
        id: u64,
        space_id: SpaceId,
        origin: &str,
        permissions: &[Permission],
    ) -> PermissionRequest {
        PermissionRequest {
            id,
            tab_id: 10 + id,
            space_id,
            origin: origin.into(),
            permissions: permissions.to_vec(),
        }
    }

    #[test]
    fn sentences_name_what_a_site_wants() {
        assert_eq!(
            request_text(&[Camera, Microphone]),
            "wants to use your camera and microphone"
        );
        assert_eq!(request_text(&[Microphone]), "wants to use your microphone");
        assert_eq!(
            request_text(&[Camera, Location, Notifications]),
            "wants to use your camera, know your location and show notifications"
        );
        assert_eq!(request_icon(&[Location]), Icon::MapPin);
        assert_eq!(request_icon(&[Other]), Icon::Shield);
    }

    #[test]
    fn answers_hold_per_space_site_and_permission() {
        let meet = "https://meet.example/";
        let mut prompts = PermissionPrompts::default();
        assert_eq!(prompts.decided(1, meet, &[Camera]), None);
        prompts.remember(&request(1, 1, meet, &[Camera, Microphone]), true);
        assert_eq!(prompts.decided(1, meet, &[Microphone]), Some(true));
        assert_eq!(prompts.decided(1, meet, &[Camera, Microphone]), Some(true));
        // Not for another permission, space or site.
        assert_eq!(prompts.decided(1, meet, &[Camera, Location]), None);
        assert_eq!(prompts.decided(2, meet, &[Camera]), None);
        assert_eq!(
            prompts.decided(1, "https://other.example/", &[Camera]),
            None
        );
        // One blocked permission blocks the request.
        prompts.remember(&request(2, 1, meet, &[Location]), false);
        assert_eq!(prompts.decided(1, meet, &[Camera, Location]), Some(false));
        // Screen sharing is always asked.
        prompts.remember(&request(3, 1, meet, &[Screen]), true);
        assert_eq!(prompts.decided(1, meet, &[Screen]), None);
    }

    #[test]
    fn answering_settles_waiting_requests_of_the_same_site() {
        let meet = "https://meet.example/";
        let mut prompts = PermissionPrompts {
            queue: vec![
                request(1, 1, meet, &[Camera, Microphone]),
                request(2, 1, meet, &[Microphone]),
                request(3, 1, meet, &[Location]),
                request(4, 2, meet, &[Microphone]),
            ],
            ..PermissionPrompts::default()
        };
        let first = prompts.remove(1).unwrap();
        prompts.remember(&first, true);
        assert_eq!(prompts.take_decided(), [(2, true)]);
        let ids: Vec<u64> = prompts.queue.iter().map(|request| request.id).collect();
        assert_eq!(ids, [3, 4]);
        // The card shows the oldest request of the tab on screen only.
        assert_eq!(prompts.shown(Some(13)).map(|request| request.id), Some(3));
        assert!(prompts.shown(Some(99)).is_none());
        assert!(prompts.shown(None).is_none());
    }
}
