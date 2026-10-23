//! Toolbar and menu glyphs, embedded in the binary and served to GPUI's
//! `svg()` element, which tints them. Most are Lucide icons (ISC licence,
//! licenses/Lucide-ISC.txt) at a lighter stroke; the arrows and the sidebar
//! glyph are drawn after SF Symbols' proportions.

use gpui::{AssetSource, Hsla, SharedString, Styled, Svg, px, svg};
use std::borrow::Cow;

macro_rules! icons {
    ($($name:ident => $file:literal),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Icon {
            $($name),*
        }

        impl Icon {
            fn path(self) -> &'static str {
                match self {
                    $(Icon::$name => concat!("icons/", $file, ".svg")),*
                }
            }
        }

        const FILES: &[(&str, &[u8])] = &[
            $((
                concat!("icons/", $file, ".svg"),
                include_bytes!(concat!("../../assets/icons/", $file, ".svg")),
            )),*
        ];
    };
}

icons! {
    Archive => "archive",
    ArrowDown => "arrow-down",
    ArrowLeft => "arrow-left",
    ArrowRight => "arrow-right",
    Bell => "bell",
    Broom => "brush-cleaning",
    Camera => "camera",
    Check => "check",
    ChevronDown => "chevron-down",
    ChevronUp => "chevron-up",
    CircleAlert => "circle-alert",
    Clipboard => "clipboard",
    Close => "x",
    Copy => "copy",
    Ellipsis => "ellipsis",
    File => "file",
    Globe => "globe",
    Hourglass => "hourglass",
    Link => "link",
    Lock => "lock",
    MapPin => "map-pin",
    Mic => "mic",
    Minus => "minus",
    Monitor => "monitor",
    Pin => "pin",
    PinOff => "pin-off",
    Plus => "plus",
    Reload => "rotate-cw",
    Reopen => "rotate-ccw",
    Search => "search",
    Shield => "shield",
    ShieldAlert => "shield-alert",
    ShieldOff => "shield-off",
    Sidebar => "sidebar",
    Speaker => "volume-2",
    SpeakerOff => "volume-x",
    Star => "star",
    Warning => "triangle-alert",
    WifiOff => "wifi-off",
}

/// The application's assets: only icons.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(FILES
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(FILES
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}

/// `icon` as a square of `size` points in `color`.
pub fn icon(icon: Icon, size: f32, color: impl Into<Hsla>) -> Svg {
    svg()
        .path(icon.path())
        .size(px(size))
        .flex_none()
        .text_color(color)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_an_embedded_svg() {
        for (name, bytes) in FILES {
            let text = std::str::from_utf8(bytes).unwrap();
            assert!(
                text.contains("<svg") && text.contains("currentColor"),
                "{name}"
            );
            assert!(Assets.load(name).unwrap().is_some());
        }
        assert_eq!(Assets.list("icons/").unwrap().len(), FILES.len());
        assert!(Assets.load("icons/missing.svg").unwrap().is_none());
    }
}
