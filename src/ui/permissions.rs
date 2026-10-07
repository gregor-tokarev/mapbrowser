//! Permission requests: a card in the sidebar, above the bottom bar, asks
//! whether a site may use the camera, microphone, location and so on. Only
//! the active tab's requests show; the others wait until their tab does.
//! Answers hold for the rest of the session, per space, site and
//! permission (Chromium also keeps prompt answers, such as location, in
//! the space's profile; camera and microphone it never keeps).

use super::{
    icons::{Icon, icon},
    press_only, shadow,
    theme::Theme,
    truncated,
};
use crate::{
    app::BrowserApp,
    engine::{Permission, PermissionDecision},
    favicons::host_of,
    model::{SpaceId, TabId},
};
use gpui::{
    Context, Div, FontWeight, InteractiveElement, ParentElement, StatefulInteractiveElement,
    Styled, Window, div, px,
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
    /// This session's answers: allowed or blocked.
    decisions: HashMap<(SpaceId, String, Permission), bool>,
}

/// Screen sharing is asked every time, as in Chrome: what is on screen
/// changes.
fn remembered(permission: Permission) -> bool {
    permission != Permission::Screen
}

impl PermissionPrompts {
    /// The session's answer to all of `permissions`: blocked if any is,
    /// allowed if all are.
    fn decided(&self, space_id: SpaceId, origin: &str, permissions: &[Permission]) -> Option<bool> {
        let mut allowed = true;
        for &permission in permissions {
            match remembered(permission)
                .then(|| {
                    self.decisions
                        .get(&(space_id, origin.to_owned(), permission))
                })
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
        for &permission in request.permissions.iter().filter(|&&p| remembered(p)) {
            self.decisions.insert(
                (request.space_id, request.origin.clone(), permission),
                allow,
            );
        }
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

/// "use your camera and microphone", "know your location"…
fn request_text(permissions: &[Permission]) -> String {
    let both = [Permission::Camera, Permission::Microphone]
        .iter()
        .all(|permission| permissions.contains(permission));
    let phrases: Vec<&str> = permissions
        .iter()
        .filter_map(|permission| {
            Some(match permission {
                Permission::Camera if both => "use your camera and microphone",
                Permission::Microphone if both => return None,
                Permission::Camera => "use your camera",
                Permission::Microphone => "use your microphone",
                Permission::Screen => "share your screen",
                Permission::Location => "know your location",
                Permission::Notifications => "show notifications",
                Permission::Clipboard => "see text and images you copy",
                Permission::Midi => "use your MIDI devices",
                Permission::Downloads => "download multiple files",
                Permission::StorageAccess => "use its cookies and site data",
                Permission::Other => "use other features of this device",
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

fn request_icon(permissions: &[Permission]) -> Icon {
    match permissions.first() {
        Some(Permission::Camera) => Icon::Camera,
        Some(Permission::Microphone) => Icon::Mic,
        Some(Permission::Screen) => Icon::Monitor,
        Some(Permission::Location) => Icon::MapPin,
        Some(Permission::Notifications) => Icon::Bell,
        Some(Permission::Clipboard) => Icon::Clipboard,
        Some(Permission::Downloads) => Icon::ArrowDown,
        _ => Icon::Shield,
    }
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

    fn respond_permission(&self, id: u64, answer: PermissionDecision) {
        self.engine.borrow().respond_permission(id, answer);
    }

    pub fn permission_shown(&self) -> Option<&PermissionRequest> {
        self.permissions.shown(self.active_tab_id())
    }

    /// Esc: closes the open menu, else dismisses the permission card.
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.menu.is_some() {
            self.close_menu(cx);
        } else if let Some(id) = self.permission_shown().map(|request| request.id) {
            self.answer_permission(id, PermissionDecision::Dismiss, cx);
        }
    }

    /// "meet.google.com wants to use your camera and microphone", with
    /// Block and Allow, for a sidebar `width` wide.
    pub(super) fn render_permission_card(
        &self,
        width: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let request = self.permission_shown()?;
        let id = request.id;
        let host = host_of(&request.origin).unwrap_or_else(|| request.origin.clone());
        // The text wraps within a known width: the card's, less its padding
        // and the icon. The close button sits in the corner, beside the host.
        let text_width = width - 16. - 2. * CARD_PADDING - ICON_TILE - ICON_GAP;
        let button = |label: &'static str, answer: PermissionDecision| {
            let primary = answer == PermissionDecision::Allow;
            let (fill, hover) = if primary {
                (theme.focus(), theme.focus().opacity(0.88))
            } else {
                (theme.control(), theme.control_hover())
            };
            press_only(
                div()
                    .id(label)
                    .flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .h(px(28.))
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
                    .child(label),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.answer_permission(id, answer, cx)))
        };
        let close_hover = theme.hover();
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
                                .w(px(text_width.max(40.)))
                                .text_size(px(13.))
                                .line_height(px(17.))
                                .child(
                                    truncated(host)
                                        .mr(px(CLOSE_BUTTON))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.title()),
                                )
                                .child(
                                    div()
                                        .text_color(theme.muted())
                                        .child(request_text(&request.permissions)),
                                ),
                        ),
                )
                .child(
                    press_only(
                        div()
                            .id("permission-close")
                            .absolute()
                            .top(px(8.))
                            .right(px(8.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(CLOSE_BUTTON))
                            .rounded(px(5.))
                            .hover(move |style| style.bg(close_hover))
                            .child(icon(Icon::Close, 15., theme.icon())),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.answer_permission(id, PermissionDecision::Dismiss, cx)
                    })),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(button("Block", PermissionDecision::Block))
                        .child(button("Allow", PermissionDecision::Allow)),
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
