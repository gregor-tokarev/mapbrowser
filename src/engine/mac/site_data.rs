//! "Clear Site Data" from the site menu: a site's cookies and storage in
//! one space's profile. Cookies go through the profile's cookie manager;
//! local storage, IndexedDB, caches and service workers through DevTools'
//! Storage.clearDataForOrigin on the tab's browser, which needs no DevTools
//! window. The page reloads once the storage is gone.

use super::Engine;
use cef::*;
use std::sync::{Arc, Mutex};

/// A clear in progress: the DevTools message it waits for, and the
/// observer's registration, which keeps it listening until then.
#[derive(Default)]
struct Pending {
    message_id: Option<i32>,
    registration: Option<Registration>,
}

impl Engine {
    /// Deletes the cookies and stored data of `origin` ("https://host") in
    /// the space's profile, then reloads the tab showing it.
    pub fn clear_site_data(&mut self, space_id: u64, tab_id: u64, origin: &str) {
        let Ok(context) = self.space_context(space_id) else {
            return;
        };
        let url = CefString::from(origin);
        if let Some(cookies) = context.cookie_manager(None) {
            cookies.delete_cookies(Some(&url), None, None);
        }
        let Some(browser) = self.live_browser(tab_id).cloned() else {
            return;
        };
        let (Some(host), Some(mut params)) = (browser.host(), dictionary_value_create()) else {
            return;
        };
        params.set_string(Some(&CefString::from("origin")), Some(&url));
        params.set_string(
            Some(&CefString::from("storageTypes")),
            Some(&CefString::from("all")),
        );
        let pending = Arc::new(Mutex::new(Pending::default()));
        let mut observer = StorageCleared::new(pending.clone());
        let registration = host.add_dev_tools_message_observer(Some(&mut observer));
        // 0: Chromium numbers the message. Its result arrives later, as a
        // task on this (the UI) thread, so the id is noted in time.
        let message_id = host.execute_dev_tools_method(
            0,
            Some(&CefString::from("Storage.clearDataForOrigin")),
            Some(&mut params),
        );
        if message_id == 0 || registration.is_none() {
            // Not sent, or nothing listens: reload with the cookies gone.
            browser.reload();
            return;
        }
        *pending.lock().expect("site data clearing") = Pending {
            message_id: Some(message_id),
            registration,
        };
    }
}

wrap_dev_tools_message_observer! {
    struct StorageCleared { pending: Arc<Mutex<Pending>> }
    impl DevToolsMessageObserver {
        fn on_dev_tools_method_result(&self, browser: Option<&mut Browser>, message_id: i32, _success: i32, _result: Option<&[u8]>) {
            {
                let mut pending = self.pending.lock().expect("site data clearing");
                if pending.message_id != Some(message_id) {
                    return;
                }
                pending.message_id = None;
            }
            // The page starts again without what it kept.
            if let Some(browser) = browser {
                browser.reload();
            }
            stop_listening(&self.pending);
        }
        fn on_dev_tools_agent_detached(&self, _browser: Option<&mut Browser>) {
            // The browser closed first: nothing is coming.
            stop_listening(&self.pending);
        }
    }
}

/// Unregisters the observer, after its callback has returned.
fn stop_listening(pending: &Arc<Mutex<Pending>>) {
    let mut task = Unregister::new(pending.clone());
    post_task(ThreadId::UI, Some(&mut task));
}

wrap_task! {
    struct Unregister { pending: Arc<Mutex<Pending>> }
    impl Task {
        fn execute(&self) {
            let registration = self.pending.lock().expect("site data clearing").registration.take();
            drop(registration);
        }
    }
}
