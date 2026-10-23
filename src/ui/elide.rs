//! One line of text cut short to fit its box, other than at its end: a
//! host loses its start, so the domain that owns it stays readable
//! ("…session-join.attacker.example"), and a file name loses its middle,
//! keeping how it ends and its extension. GPUI 0.2.2 cuts text only at the
//! end, and the room a line gets is known only once it is laid out, so the
//! text is measured and painted then, in the parent's text style.

use gpui::{
    App, Bounds, Div, ParentElement, Pixels, SharedString, Styled, Window, canvas, div, point, px,
};

/// Where a line too long for its box loses characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cut {
    /// Keep the end: hosts and domains.
    Start,
    /// Keep the start and the end: file names.
    Middle,
}

/// `text` on one line `height` tall, cut where `cut` says if it doesn't
/// fit. Style the text (size, weight, colour) on the returned box.
pub fn elided(text: impl Into<SharedString>, cut: Cut, height: f32) -> Div {
    let text: SharedString = text.into();
    div().h(px(height)).min_w_0().overflow_hidden().child(
        canvas(
            |_, _, _| {},
            move |bounds, (), window, cx| paint(&text, cut, bounds, window, cx),
        )
        .size_full(),
    )
}

fn paint(text: &str, cut: Cut, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    let style = window.text_style();
    let rem = window.rem_size();
    let font_size = style.font_size.to_pixels(rem);
    let width = |text: &str| {
        let run = style.to_run(text.len());
        f32::from(
            window
                .text_system()
                .layout_line(text, font_size, &[run], None)
                .width,
        )
    };
    let shown = fit(text, f32::from(bounds.size.width), cut, width);
    let run = style.to_run(shown.len());
    let line = window
        .text_system()
        .shape_line(shown.into(), font_size, &[run], None);
    // Centred in the box, whatever line height the text has.
    let line_height = style.line_height_in_pixels(rem);
    let top = bounds.top() + (bounds.size.height - line_height) / 2.;
    let _ = line.paint(point(bounds.left(), top), line_height, window, cx);
}

/// `text`, or as much of it as fits in `room` with an ellipsis where `cut`
/// says, measured with `width`. Toasts and the link pill fit their text
/// with it too.
pub(super) fn fit(text: &str, room: f32, cut: Cut, width: impl Fn(&str) -> f32) -> String {
    if width(text) <= room {
        return text.to_owned();
    }
    // The most characters that fit, assuming fewer never take more room.
    let (mut fits, mut too_many) = (0, text.chars().count());
    while too_many - fits > 1 {
        let kept = (fits + too_many) / 2;
        if width(&keep(text, kept, cut)) <= room {
            fits = kept;
        } else {
            too_many = kept;
        }
    }
    keep(text, fits, cut)
}

/// `text` with `kept` of its characters and an ellipsis where they were cut.
fn keep(text: &str, kept: usize, cut: Cut) -> String {
    let chars: Vec<char> = text.chars().collect();
    let tail = match cut {
        Cut::Start => kept,
        // At least the extension, if there is room for more than it.
        Cut::Middle => {
            let extension = chars
                .iter()
                .rev()
                .position(|&ch| ch == '.')
                .map_or(0, |dot| dot + 1);
            let half = kept / 2;
            if extension < kept {
                half.max(extension)
            } else {
                half
            }
        }
    }
    .min(kept);
    let head = kept - tail;
    let start: String = chars[..head].iter().collect();
    let end: String = chars[chars.len() - tail..].iter().collect();
    // "…example.com" reads better than "….example.com".
    let end = match cut {
        Cut::Start => end.trim_start_matches('.'),
        Cut::Middle => &end,
    };
    format!("{start}…{end}")
}
