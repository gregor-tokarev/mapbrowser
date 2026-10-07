//! Camera, microphone, screen, location and other permission requests go
//! to the shell, which asks the user. With Alloy style and no handler CEF
//! denies media access and ignores every other prompt.

use super::BrowserLink;
use crate::engine::{
    EngineEvent, Permission, PermissionDecision, media_permissions, prompt_permissions,
};
use cef::*;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};

// The shared bit tables (`media_permissions`, `prompt_permissions`) use
// CEF's values; keep them in step with the bindings.
const _: () = {
    use sys::cef_media_access_permission_types_t as Media;
    use sys::cef_permission_request_types_t as Prompt;
    assert!(Media::CEF_MEDIA_PERMISSION_DEVICE_AUDIO_CAPTURE as u32 == 1);
    assert!(Media::CEF_MEDIA_PERMISSION_DEVICE_VIDEO_CAPTURE as u32 == 2);
    assert!(Media::CEF_MEDIA_PERMISSION_DESKTOP_AUDIO_CAPTURE as u32 == 4);
    assert!(Media::CEF_MEDIA_PERMISSION_DESKTOP_VIDEO_CAPTURE as u32 == 8);
    assert!(Prompt::CEF_PERMISSION_TYPE_CAMERA_PAN_TILT_ZOOM as u32 == 2);
    assert!(Prompt::CEF_PERMISSION_TYPE_CAMERA_STREAM as u32 == 4);
    assert!(Prompt::CEF_PERMISSION_TYPE_CLIPBOARD as u32 == 16);
    assert!(Prompt::CEF_PERMISSION_TYPE_TOP_LEVEL_STORAGE_ACCESS as u32 == 32);
    assert!(Prompt::CEF_PERMISSION_TYPE_GEOLOCATION as u32 == 256);
    assert!(Prompt::CEF_PERMISSION_TYPE_MIC_STREAM as u32 == 4096);
    assert!(Prompt::CEF_PERMISSION_TYPE_MIDI_SYSEX as u32 == 8192);
    assert!(Prompt::CEF_PERMISSION_TYPE_MULTIPLE_DOWNLOADS as u32 == 16384);
    assert!(Prompt::CEF_PERMISSION_TYPE_NOTIFICATIONS as u32 == 32768);
    assert!(Prompt::CEF_PERMISSION_TYPE_STORAGE_ACCESS as u32 == 1 << 20);
};

/// Requests waiting for the user, shared by every browser's handler and
/// the engine. Callbacks run on the UI thread, where a prompt's answer can
/// call straight back into the handler, so they are never answered with
/// the lock held.
#[derive(Clone, Default)]
pub(super) struct PermissionRequests(Arc<Mutex<Requests>>);

#[derive(Default)]
struct Requests {
    next_id: u64,
    pending: HashMap<u64, Pending>,
}

/// A request's callback, answered at most once.
struct Pending {
    instance: u64,
    /// CEF's id of a permission prompt; media requests have none.
    prompt_id: Option<u64>,
    callback: Option<Callback>,
}

enum Callback {
    /// getUserMedia must be allowed exactly the devices it asked for.
    Media(MediaAccessCallback, u32),
    Prompt(PermissionPromptCallback),
}

impl Callback {
    fn answer(self, decision: PermissionDecision) {
        match (self, decision) {
            (Callback::Media(callback, requested), PermissionDecision::Allow) => {
                callback.cont(requested)
            }
            (Callback::Media(callback, _), _) => callback.cancel(),
            (Callback::Prompt(callback), decision) => callback.cont(match decision {
                // Chromium keeps both in the space's profile.
                PermissionDecision::Allow => PermissionRequestResult::ACCEPT,
                PermissionDecision::Block => PermissionRequestResult::DENY,
                PermissionDecision::Dismiss => PermissionRequestResult::DISMISS,
            }),
        }
    }
}

impl Drop for Pending {
    /// An unanswered media request is refused. Prompts end with their page:
    /// Chromium ignores them then, which an answer here would turn into a
    /// dismissal it holds against the site.
    fn drop(&mut self) {
        if let Some(callback @ Callback::Media(..)) = self.callback.take() {
            callback.answer(PermissionDecision::Dismiss);
        }
    }
}

impl PermissionRequests {
    fn requests(&self) -> MutexGuard<'_, Requests> {
        self.0.lock().expect("permission requests")
    }

    fn add(
        &self,
        link: &BrowserLink,
        prompt_id: Option<u64>,
        origin: Option<&CefString>,
        permissions: Vec<Permission>,
        callback: Callback,
    ) {
        let id = {
            let mut requests = self.requests();
            requests.next_id += 1;
            let id = requests.next_id;
            requests.pending.insert(
                id,
                Pending {
                    instance: link.instance,
                    prompt_id,
                    callback: Some(callback),
                },
            );
            id
        };
        link.send(EngineEvent::PermissionRequest {
            id,
            tab_id: link.tab_id,
            origin: origin.map(ToString::to_string).unwrap_or_default(),
            permissions,
        });
    }

    /// Answers request `id`; later answers to it do nothing.
    pub(super) fn respond(&self, id: u64, decision: PermissionDecision) {
        let pending = self.requests().pending.remove(&id);
        if let Some(callback) = pending.and_then(|mut pending| pending.callback.take()) {
            callback.answer(decision);
        }
    }

    /// Drops the requests of the page in a browser instance, once the
    /// browser closes or shows another page, and tells the shell.
    pub(super) fn drop_page(&self, link: &BrowserLink) {
        let dropped: Vec<(u64, Pending)> = self
            .requests()
            .pending
            .extract_if(|_, pending| pending.instance == link.instance)
            .collect();
        for (id, pending) in dropped {
            drop(pending);
            link.send(EngineEvent::PermissionDismissed { id });
        }
    }

    /// Drops every request, for shutting down while Chromium still runs.
    pub(super) fn clear(&self) {
        let pending = std::mem::take(&mut self.requests().pending);
        drop(pending);
    }
}

wrap_permission_handler! {
    pub(super) struct TabPermissions { link: BrowserLink }
    impl PermissionHandler {
        fn on_request_media_access_permission(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, requesting_origin: Option<&CefString>, requested_permissions: u32, callback: Option<&mut MediaAccessCallback>) -> i32 {
            let permissions = media_permissions(requested_permissions);
            let Some(callback) = callback.filter(|_| !permissions.is_empty()) else { return 0; };
            let callback = Callback::Media(callback.clone(), requested_permissions);
            self.link.permissions.add(&self.link, None, requesting_origin, permissions, callback);
            1
        }
        fn on_show_permission_prompt(&self, _browser: Option<&mut Browser>, prompt_id: u64, requesting_origin: Option<&CefString>, requested_permissions: u32, callback: Option<&mut PermissionPromptCallback>) -> i32 {
            let permissions = prompt_permissions(requested_permissions);
            let Some(callback) = callback.filter(|_| !permissions.is_empty()) else { return 0; };
            let callback = Callback::Prompt(callback.clone());
            self.link.permissions.add(&self.link, Some(prompt_id), requesting_origin, permissions, callback);
            1
        }
        fn on_dismiss_permission_prompt(&self, _browser: Option<&mut Browser>, prompt_id: u64, _result: PermissionRequestResult) {
            // Answered, or ended by Chromium (navigation, closing).
            let ended: Vec<u64> = self.link.permissions.requests().pending
                .extract_if(|_, pending| pending.instance == self.link.instance && pending.prompt_id == Some(prompt_id))
                .map(|(id, _)| id)
                .collect();
            for id in ended {
                self.link.send(EngineEvent::PermissionDismissed { id });
            }
        }
    }
}
