//! The page's right-click menu. Alloy browsers get CEF's minimal one (Back,
//! Forward, editing commands, Copy for a selection), not Chrome's; links,
//! images and selections get Chrome's most used items on top of it.

use super::{BrowserLink, frame_url, request_tab};
use crate::engine::{EngineEvent, opens_in_tab, page::search_label};
use cef::*;

const MENU_OPEN_LINK: i32 = sys::cef_menu_id_t::MENU_ID_USER_FIRST as i32;
const MENU_COPY_LINK: i32 = MENU_OPEN_LINK + 1;
const MENU_DOWNLOAD_LINK: i32 = MENU_OPEN_LINK + 2;
const MENU_OPEN_IMAGE: i32 = MENU_OPEN_LINK + 3;
const MENU_SAVE_IMAGE: i32 = MENU_OPEN_LINK + 4;
const MENU_COPY_IMAGE: i32 = MENU_OPEN_LINK + 5;
const MENU_SEARCH: i32 = MENU_OPEN_LINK + 6;
const _: () = assert!(MENU_SEARCH <= sys::cef_menu_id_t::MENU_ID_USER_LAST as i32);

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

// `popup`: the menu of a popup window, which offers no downloads: popups
// have no download handler.
wrap_context_menu_handler! {
    pub(super) struct TabMenu { link: BrowserLink, popup: bool }
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
                    if !self.popup {
                        add(MENU_DOWNLOAD_LINK, "Download Linked File");
                    }
                }
                add(MENU_COPY_LINK, "Copy Link Address");
            }
            if !target.image.is_empty() {
                // data: images can't open in a tab; Chromium refuses them.
                if opens_in_tab(&target.image, &page) {
                    add(MENU_OPEN_IMAGE, "Open Image in New Tab");
                }
                if !self.popup {
                    add(MENU_SAVE_IMAGE, "Save Image");
                }
                add(MENU_COPY_IMAGE, "Copy Image Address");
            }
            if index > 0 {
                model.insert_separator_at(index);
            }
            if let Some(label) = search_label(&target.selection) {
                // Right after Copy, as in Chrome.
                let copy = model.index_of(sys::cef_menu_id_t::MENU_ID_COPY as i32);
                let at = usize::try_from(copy + 1).unwrap_or(0);
                model.insert_item_at(at, MENU_SEARCH, Some(&CefString::from(label.as_str())));
            }
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
                MENU_COPY_LINK => self.link.send(EngineEvent::CopyText(target.link)),
                MENU_DOWNLOAD_LINK => download(&target.link),
                MENU_OPEN_IMAGE => request_tab(&self.link, frame, target.image, false),
                MENU_SAVE_IMAGE => download(&target.image),
                MENU_COPY_IMAGE => self.link.send(EngineEvent::CopyText(target.image)),
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
