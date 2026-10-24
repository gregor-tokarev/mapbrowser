//! The page's right-click menu. Alloy browsers get CEF's minimal one (Back,
//! Forward, editing commands, Copy for a selection), not Chrome's; links,
//! images and selections get Chrome's most used items on top of it, the
//! page itself Reload, and every menu ends with Inspect.

use super::{BrowserLink, binary_to_vec, frame_url, popups, request_tab};
use crate::engine::{EngineEvent, opens_in_tab, page::search_label};
use async_channel::Sender;
use cef::*;

const MENU_OPEN_LINK: i32 = sys::cef_menu_id_t::MENU_ID_USER_FIRST as i32;
const MENU_COPY_LINK: i32 = MENU_OPEN_LINK + 1;
const MENU_DOWNLOAD_LINK: i32 = MENU_OPEN_LINK + 2;
const MENU_OPEN_IMAGE: i32 = MENU_OPEN_LINK + 3;
const MENU_SAVE_IMAGE: i32 = MENU_OPEN_LINK + 4;
const MENU_COPY_IMAGE_ADDRESS: i32 = MENU_OPEN_LINK + 5;
const MENU_SEARCH: i32 = MENU_OPEN_LINK + 6;
const MENU_COPY_IMAGE: i32 = MENU_OPEN_LINK + 7;
const MENU_INSPECT: i32 = MENU_OPEN_LINK + 8;
const _: () = assert!(MENU_INSPECT <= sys::cef_menu_id_t::MENU_ID_USER_LAST as i32);

/// What was right-clicked, as the menu's items need it.
struct Target {
    link: String,
    /// The image's address, for images with contents.
    image: String,
    selection: String,
}

impl Target {
    fn of(params: Option<&ContextMenuParams>) -> Self {
        let Some(params) = params else {
            return Self {
                link: String::new(),
                image: String::new(),
                selection: String::new(),
            };
        };
        let text = |text: CefStringUserfree| CefString::from(&text).to_string();
        let image =
            params.media_type() == ContextMenuMediaType::IMAGE && params.has_image_contents() != 0;
        Self {
            link: text(params.link_url()),
            image: if image {
                text(params.source_url())
            } else {
                String::new()
            },
            selection: text(params.selection_text()),
        }
    }
}

wrap_context_menu_handler! {
    pub(super) struct TabMenu { link: BrowserLink }
    impl ContextMenuHandler {
        fn on_before_context_menu(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, params: Option<&mut ContextMenuParams>, model: Option<&mut MenuModel>) {
            let Some(model) = model else { return; };
            model.remove(sys::cef_menu_id_t::MENU_ID_VIEW_SOURCE as i32);
            model.remove(sys::cef_menu_id_t::MENU_ID_PRINT as i32);
            let target = Target::of(params.as_deref());
            let page = frame_url(frame.as_deref());
            let mut index = 0;
            let mut add = |id: i32, label: &str| {
                model.insert_item_at(index, id, Some(&CefString::from(label)));
                index += 1;
            };
            if !target.link.is_empty() {
                // mailto: and javascript: links can be copied, not opened.
                if opens_in_tab(&target.link, &page) {
                    add(MENU_OPEN_LINK, "Open Link in New Tab");
                    add(MENU_DOWNLOAD_LINK, "Download Linked File");
                }
                add(MENU_COPY_LINK, "Copy Link Address");
            }
            if !target.image.is_empty() {
                // data: images can't open in a tab; Chromium refuses them.
                if opens_in_tab(&target.image, &page) {
                    add(MENU_OPEN_IMAGE, "Open Image in New Tab");
                }
                add(MENU_SAVE_IMAGE, "Save Image");
                add(MENU_COPY_IMAGE, "Copy Image");
                add(MENU_COPY_IMAGE_ADDRESS, "Copy Image Address");
            }
            if index > 0 {
                model.insert_separator_at(index);
            } else if target.selection.is_empty() {
                // The page itself: Back, Forward, then Reload, as in Chrome.
                // CEF runs Reload itself; editable fields have no Forward.
                let forward = model.index_of(sys::cef_menu_id_t::MENU_ID_FORWARD as i32);
                if let Ok(forward) = usize::try_from(forward) {
                    model.insert_item_at(forward + 1, sys::cef_menu_id_t::MENU_ID_RELOAD as i32, Some(&CefString::from("Reload")));
                }
            }
            if let Some(label) = search_label(&target.selection) {
                // Right after Copy, as in Chrome.
                let copy = model.index_of(sys::cef_menu_id_t::MENU_ID_COPY as i32);
                let at = usize::try_from(copy + 1).unwrap_or(0);
                model.insert_item_at(at, MENU_SEARCH, Some(&CefString::from(label.as_str())));
            }
            // The developer tools at what was clicked, as in Chrome.
            model.add_separator();
            model.add_item(MENU_INSPECT, Some(&CefString::from("Inspect")));
            tidy_separators(model);
        }
        fn on_context_menu_command(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, params: Option<&mut ContextMenuParams>, command_id: i32, _event_flags: EventFlags) -> i32 {
            let target = Target::of(params.as_deref());
            let frame = frame.as_deref();
            let download = |url: &str| {
                if let Some(host) = browser.as_deref().and_then(|browser| browser.host()) {
                    // To the Downloads folder, shown in the sidebar.
                    host.start_download(Some(&CefString::from(url)));
                }
            };
            match command_id {
                MENU_OPEN_LINK => request_tab(&self.link, frame, target.link, false),
                MENU_COPY_LINK => self.link.send(EngineEvent::CopyText { text: target.link, image: false }),
                MENU_DOWNLOAD_LINK => download(&target.link),
                MENU_OPEN_IMAGE => request_tab(&self.link, frame, target.image, false),
                MENU_SAVE_IMAGE => download(&target.image),
                MENU_COPY_IMAGE => {
                    if let Some(host) = browser.as_deref().and_then(|browser| browser.host()) {
                        let mut callback = CopyImage::new(self.link.events.clone());
                        host.download_image(Some(&CefString::from(target.image.as_str())), 0, 0, 0, Some(&mut callback));
                    }
                }
                MENU_COPY_IMAGE_ADDRESS => self.link.send(EngineEvent::CopyText { text: target.image, image: true }),
                MENU_INSPECT => {
                    if let Some(host) = browser.as_deref().and_then(|browser| browser.host()) {
                        let at = params.as_deref().map(|params| Point { x: params.xcoord(), y: params.ycoord() });
                        let mut client = popups::tools_client(self.link.clone());
                        host.show_dev_tools(Some(&WindowInfo::default()), Some(&mut client), Some(&BrowserSettings::default()), at.as_ref());
                    }
                }
                MENU_SEARCH => {
                    let words = target.selection.split_whitespace().collect::<Vec<_>>().join(" ");
                    request_tab(&self.link, frame, crate::model::search_url(&words), false);
                }
                _ => return 0,
            }
            1
        }
    }
}

// "Copy Image": the image as the page has it, encoded as PNG off the main
// thread and put on the clipboard by the shell.
wrap_download_image_callback! {
    struct CopyImage { events: Sender<EngineEvent> }
    impl DownloadImageCallback {
        fn on_download_image_finished(&self, _image_url: Option<&CefString>, _http_status_code: i32, image: Option<&mut Image>) {
            let Some(image) = image.filter(|image| image.is_empty() == 0).cloned() else {
                let _ = self.events.try_send(EngineEvent::CopyImage(None));
                return;
            };
            let mut task = EncodeCopiedImage::new(self.events.clone(), image);
            post_task(ThreadId::FILE_USER_VISIBLE, Some(&mut task));
        }
    }
}

// CefImage may be used on any browser process thread.
wrap_task! {
    struct EncodeCopiedImage { events: Sender<EngineEvent>, image: Image }
    impl Task {
        fn execute(&self) {
            let (mut width, mut height) = (0, 0);
            let png = self
                .image
                .as_png(1., 1, Some(&mut width), Some(&mut height))
                .map(|png| binary_to_vec(&png))
                .filter(|png| !png.is_empty());
            let _ = self.events.try_send(EngineEvent::CopyImage(png));
        }
    }
}

/// Removing items can leave leading, trailing or doubled separators.
fn tidy_separators(model: &MenuModel) {
    let mut index = 0;
    let mut after_separator = true;
    while index < model.count() {
        let separator = model.type_at(index) == MenuItemType::SEPARATOR;
        if separator && after_separator {
            model.remove_at(index);
        } else {
            after_separator = separator;
            index += 1;
        }
    }
    if after_separator && let Some(last) = model.count().checked_sub(1) {
        model.remove_at(last);
    }
}
