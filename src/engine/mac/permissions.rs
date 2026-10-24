//! Camera, microphone, screen, location and other permission requests go
//! to the shell, which asks the user. With Alloy style and no handler CEF
//! denies media access and ignores every other prompt. The site menu reads
//! and resets the answers Chromium keeps in each space's profile.

use super::{BrowserLink, Engine};
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
    assert!(Prompt::CEF_PERMISSION_TYPE_AR_SESSION as u32 == 1);
    assert!(Prompt::CEF_PERMISSION_TYPE_CAMERA_PAN_TILT_ZOOM as u32 == 2);
    assert!(Prompt::CEF_PERMISSION_TYPE_CAMERA_STREAM as u32 == 4);
    assert!(Prompt::CEF_PERMISSION_TYPE_CAPTURED_SURFACE_CONTROL as u32 == 8);
    assert!(Prompt::CEF_PERMISSION_TYPE_CLIPBOARD as u32 == 16);
    assert!(Prompt::CEF_PERMISSION_TYPE_TOP_LEVEL_STORAGE_ACCESS as u32 == 32);
    assert!(Prompt::CEF_PERMISSION_TYPE_DISK_QUOTA as u32 == 64);
    assert!(Prompt::CEF_PERMISSION_TYPE_LOCAL_FONTS as u32 == 128);
    assert!(Prompt::CEF_PERMISSION_TYPE_GEOLOCATION as u32 == 256);
    assert!(Prompt::CEF_PERMISSION_TYPE_HAND_TRACKING as u32 == 512);
    assert!(Prompt::CEF_PERMISSION_TYPE_IDENTITY_PROVIDER as u32 == 1024);
    assert!(Prompt::CEF_PERMISSION_TYPE_IDLE_DETECTION as u32 == 2048);
    assert!(Prompt::CEF_PERMISSION_TYPE_MIC_STREAM as u32 == 4096);
    assert!(Prompt::CEF_PERMISSION_TYPE_MIDI_SYSEX as u32 == 8192);
    assert!(Prompt::CEF_PERMISSION_TYPE_MULTIPLE_DOWNLOADS as u32 == 16384);
    assert!(Prompt::CEF_PERMISSION_TYPE_NOTIFICATIONS as u32 == 32768);
    assert!(Prompt::CEF_PERMISSION_TYPE_KEYBOARD_LOCK as u32 == 1 << 16);
    assert!(Prompt::CEF_PERMISSION_TYPE_POINTER_LOCK as u32 == 1 << 17);
    assert!(Prompt::CEF_PERMISSION_TYPE_PROTECTED_MEDIA_IDENTIFIER as u32 == 1 << 18);
    assert!(Prompt::CEF_PERMISSION_TYPE_REGISTER_PROTOCOL_HANDLER as u32 == 1 << 19);
    assert!(Prompt::CEF_PERMISSION_TYPE_STORAGE_ACCESS as u32 == 1 << 20);
    assert!(Prompt::CEF_PERMISSION_TYPE_VR_SESSION as u32 == 1 << 21);
    assert!(Prompt::CEF_PERMISSION_TYPE_WEB_APP_INSTALLATION as u32 == 1 << 22);
    assert!(Prompt::CEF_PERMISSION_TYPE_WINDOW_MANAGEMENT as u32 == 1 << 23);
    assert!(Prompt::CEF_PERMISSION_TYPE_FILE_SYSTEM_ACCESS as u32 == 1 << 24);
    assert!(Prompt::CEF_PERMISSION_TYPE_LOCAL_NETWORK_ACCESS_DEPRECATED as u32 == 1 << 25);
    assert!(Prompt::CEF_PERMISSION_TYPE_LOCAL_NETWORK as u32 == 1 << 26);
    assert!(Prompt::CEF_PERMISSION_TYPE_LOOPBACK_NETWORK as u32 == 1 << 27);
    assert!(Prompt::CEF_PERMISSION_TYPE_SENSORS as u32 == 1 << 28);
};

/// The content settings the site menu lists. Each one asks by default, so
/// an Allow or Block found for a site is an answer someone gave; settings
/// that allow by default (sensors) would list every site.
/// `reset_settings` names every setting Reset clears.
const SITE_SETTINGS: [(ContentSettingTypes, Permission); 10] = [
    (ContentSettingTypes::MEDIASTREAM_CAMERA, Permission::Camera),
    (ContentSettingTypes::MEDIASTREAM_MIC, Permission::Microphone),
    (ContentSettingTypes::GEOLOCATION, Permission::Location),
    (
        ContentSettingTypes::NOTIFICATIONS,
        Permission::Notifications,
    ),
    (
        ContentSettingTypes::CLIPBOARD_READ_WRITE,
        Permission::Clipboard,
    ),
    (ContentSettingTypes::MIDI_SYSEX, Permission::Midi),
    (
        ContentSettingTypes::AUTOMATIC_DOWNLOADS,
        Permission::Downloads,
    ),
    (ContentSettingTypes::LOCAL_FONTS, Permission::LocalFonts),
    (
        ContentSettingTypes::WINDOW_MANAGEMENT,
        Permission::WindowManagement,
    ),
    (
        ContentSettingTypes::IDLE_DETECTION,
        Permission::IdleDetection,
    ),
];

impl Engine {
    /// What the space's profile keeps for the site at `origin`: each
    /// permission it allows (true) or blocks. Empty for a space whose
    /// profile isn't open: none of its pages was shown this session.
    pub fn site_permissions(&self, space_id: u64, origin: &str) -> Vec<(Permission, bool)> {
        let Some(context) = self.contexts.get(&space_id) else {
            return Vec::new();
        };
        let url = CefString::from(origin);
        SITE_SETTINGS
            .iter()
            .filter_map(|&(kind, permission)| {
                let value = context.content_setting(Some(&url), Some(&url), kind);
                if value == ContentSettingValues::ALLOW {
                    Some((permission, true))
                } else if value == ContentSettingValues::BLOCK {
                    Some((permission, false))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Forgets the profile's answer for `permission` at `origin`: the site
    /// asks again next time.
    pub fn reset_site_permission(&mut self, space_id: u64, origin: &str, permission: Permission) {
        let Ok(context) = self.space_context(space_id) else {
            return;
        };
        let url = CefString::from(origin);
        for &kind in reset_settings(permission) {
            context.set_content_setting(
                Some(&url),
                Some(&url),
                kind,
                ContentSettingValues::DEFAULT,
            );
        }
    }
}

/// Every content setting Chromium keeps an answer to `permission` in, for
/// Reset: the menu lists answers this session gave to any prompt, not only
/// `SITE_SETTINGS`. Each is the setting Chromium decides a prompt of that
/// kind by, so it is one this Chromium knows; setting one it doesn't
/// know would crash it.
fn reset_settings(permission: Permission) -> &'static [ContentSettingTypes] {
    use ContentSettingTypes as Kind;
    match permission {
        // getUserMedia, and moving the camera.
        Permission::Camera => &[Kind::MEDIASTREAM_CAMERA, Kind::CAMERA_PAN_TILT_ZOOM],
        Permission::Microphone => &[Kind::MEDIASTREAM_MIC],
        Permission::CapturedSurfaceControl => &[Kind::CAPTURED_SURFACE_CONTROL],
        Permission::Location => &[Kind::GEOLOCATION],
        Permission::Notifications => &[Kind::NOTIFICATIONS],
        Permission::Clipboard => &[Kind::CLIPBOARD_READ_WRITE],
        Permission::Midi => &[Kind::MIDI_SYSEX, Kind::MIDI],
        Permission::Downloads => &[Kind::AUTOMATIC_DOWNLOADS],
        Permission::StorageAccess => &[Kind::STORAGE_ACCESS, Kind::TOP_LEVEL_STORAGE_ACCESS],
        Permission::LocalFonts => &[Kind::LOCAL_FONTS],
        Permission::WindowManagement => &[Kind::WINDOW_MANAGEMENT],
        Permission::IdleDetection => &[Kind::IDLE_DETECTION],
        Permission::PointerLock => &[Kind::POINTER_LOCK],
        Permission::KeyboardLock => &[Kind::KEYBOARD_LOCK],
        Permission::LocalNetwork => &[Kind::LOCAL_NETWORK],
        Permission::LoopbackNetwork => &[Kind::LOOPBACK_NETWORK],
        Permission::Sensors => &[Kind::SENSORS],
        Permission::FileSystem => &[Kind::FILE_SYSTEM_WRITE_GUARD],
        Permission::AppInstallation => &[Kind::WEB_APP_INSTALLATION],
        Permission::VirtualReality => &[Kind::VR],
        Permission::AugmentedReality => &[Kind::AR],
        Permission::HandTracking => &[Kind::HAND_TRACKING],
        // Asked every time, or not kept as a content setting.
        Permission::Screen
        | Permission::DiskQuota
        | Permission::ProtectedMedia
        | Permission::ProtocolHandler
        | Permission::IdentityProvider
        | Permission::Other => &[],
    }
}

/// Requests waiting for the user, shared by every browser's handler and
/// the engine. Callbacks run on the UI thread, where a prompt's answer can
/// call straight back into the handler, so they are never answered, nor
/// dropped (an unanswered media request is refused then), with the lock
/// held.
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
            // Answered, or ended by Chromium (navigation, closing). What is
            // left of a request goes once the lock is released.
            let ended: Vec<(u64, Pending)> = self.link.permissions.requests().pending
                .extract_if(|_, pending| pending.instance == self.link.instance && pending.prompt_id == Some(prompt_id))
                .collect();
            for (id, pending) in ended {
                drop(pending);
                self.link.send(EngineEvent::PermissionDismissed { id });
            }
        }
    }
}
