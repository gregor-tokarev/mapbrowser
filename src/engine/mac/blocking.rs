//! Content blocking hook on each space's request context. Chromium asks it
//! about every request on the IO thread, so allowed requests return `None`
//! straight away: returning any handler would make CEF load cookies first.

use crate::blocker::{ContentBlocker, ResourceKind};
use cef::*;

pub(super) fn request_context_handler(blocker: ContentBlocker) -> RequestContextHandler {
    BlockingContext::new(blocker, AbortFrame::new())
}

wrap_request_context_handler! {
    struct BlockingContext { blocker: ContentBlocker, abort_frame: ResourceRequestHandler }
    impl RequestContextHandler {
        fn resource_request_handler(&self, browser: Option<&mut Browser>, _frame: Option<&mut Frame>, request: Option<&mut Request>, _is_navigation: i32, is_download: i32, request_initiator: Option<&CefString>, disable_default_handling: Option<&mut i32>) -> Option<ResourceRequestHandler> {
            let request = request?;
            let kind = resource_kind(request.resource_type())?;
            if is_download != 0 {
                return None;
            }
            // Borrow the URL's UTF-16 buffer and skip non-web schemes before
            // converting; data: URLs in particular can be large.
            let owned = request.url();
            let borrowed = Option::<&sys::_cef_string_utf16_t>::from(&owned).map(|raw| CefString::from(*raw))?;
            let url = String::from_utf16_lossy(borrowed.as_slice().filter(|url| is_web(url))?);
            let initiator = request_initiator.and_then(CefString::as_slice).map(String::from_utf16_lossy).unwrap_or_default();
            let page_url = || Some(CefString::from(&browser?.main_frame()?.url()).to_string());
            if !self.blocker.should_block(&url, &initiator, kind, page_url) {
                return None;
            }
            if kind == ResourceKind::SubFrame {
                // A cancelled frame navigation (ERR_ABORTED) leaves the ad slot
                // empty; any other failure commits Chromium's error page there.
                return Some(self.abort_frame.clone());
            }
            // Fails at once (ERR_UNKNOWN_URL_SCHEME): no cookie load, no network.
            if let Some(disable) = disable_default_handling {
                *disable = 1;
            }
            None
        }
    }
}

wrap_resource_request_handler! {
    struct AbortFrame;
    impl ResourceRequestHandler {
        fn on_before_resource_load(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, _request: Option<&mut Request>, _callback: Option<&mut Callback>) -> ReturnValue {
            ReturnValue::CANCEL
        }
    }
}

/// Follows Chromium's webRequest types. `None` for top-level pages, which are
/// never blocked.
fn resource_kind(kind: ResourceType) -> Option<ResourceKind> {
    Some(match kind {
        ResourceType::MAIN_FRAME | ResourceType::NAVIGATION_PRELOAD_MAIN_FRAME => return None,
        ResourceType::SUB_FRAME | ResourceType::NAVIGATION_PRELOAD_SUB_FRAME => {
            ResourceKind::SubFrame
        }
        ResourceType::STYLESHEET => ResourceKind::Stylesheet,
        ResourceType::SCRIPT
        | ResourceType::WORKER
        | ResourceType::SHARED_WORKER
        | ResourceType::SERVICE_WORKER => ResourceKind::Script,
        ResourceType::IMAGE | ResourceType::FAVICON => ResourceKind::Image,
        ResourceType::FONT_RESOURCE => ResourceKind::Font,
        ResourceType::OBJECT | ResourceType::PLUGIN_RESOURCE => ResourceKind::Object,
        ResourceType::MEDIA => ResourceKind::Media,
        ResourceType::XHR => ResourceKind::Xhr,
        ResourceType::PING => ResourceKind::Ping,
        ResourceType::CSP_REPORT => ResourceKind::CspReport,
        _ => ResourceKind::Other,
    })
}

fn is_web(url: &[u16]) -> bool {
    ["http:", "https:"].iter().any(|scheme| {
        url.len() > scheme.len() && scheme.bytes().zip(url).all(|(a, &b)| u16::from(a) == b)
    })
}
