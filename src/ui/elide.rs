//! One line of text cut short to fit its box: a host loses its start,
//! whole labels at a time, so the domain that owns it stays readable
//! ("…session-join.attacker.example"), a file name loses its middle,
//! keeping how it ends and its extension, and a name its end. GPUI 0.2.2
//! cuts text only at the end, and the room a line gets is known only once
//! it is laid out, so the text is measured and painted then, in the
//! parent's text style.

use gpui::{
    App, Bounds, Div, Font, ParentElement, Pixels, SharedString, Styled, Window, canvas, div,
    point, px,
};

/// Where a line too long for its box loses characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cut {
    /// Keep the end, from the start of a label: hosts and domains.
    Start,
    /// Keep the start and the end: file names.
    Middle,
    /// Keep the start: names, such as a space's.
    End,
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

/// The width of `text` in `font` at `size`, adding up its characters'
/// advances: near enough to decide what fits beside it before layout,
/// where no window is at hand to lay it out.
pub fn advance_width(text: &str, font: &Font, size: f32, cx: &App) -> f32 {
    let system = cx.text_system();
    let id = system.resolve_font(font);
    text.chars()
        .filter_map(|ch| system.advance(id, px(size), ch).ok())
        .map(|advance| f32::from(advance.width))
        .sum()
}

/// An address that fits in `room`, measured with `width`: its scheme and
/// host whole, and what follows them cut in the middle. A host too long
/// for the room loses its start, never the domain that owns it, and the
/// path goes ("…account-verify-session.example.net").
pub(super) fn fit_address(text: &str, room: f32, width: impl Fn(&str) -> f32) -> String {
    let start = text.find("://").map_or(0, |scheme| scheme + 3);
    let end = text[start..]
        .find(['/', '?', '#'])
        .map_or(text.len(), |index| start + index);
    let (host, rest) = text.split_at(end);
    let host_width = width(host);
    if host_width > room {
        return fit(host, room, Cut::Start, width);
    }
    format!("{host}{}", fit(rest, room - host_width, Cut::Middle, width))
}

/// `address` with invisible break opportunities after "://" and after each
/// dot, so text that wraps breaks between a host's labels ("https://" /
/// "meet.google.com.session-join."), never inside one. GPUI wraps at
/// spaces and before characters that aren't word characters, and counts
/// '.', '-' and ':' as word characters; '/' already breaks.
pub fn breakable(address: &str) -> String {
    const BREAK: char = '\u{200B}';
    let (scheme, rest) = match address.find("://") {
        Some(index) => address.split_at(index + 3),
        None => ("", address),
    };
    let mut text = String::with_capacity(address.len() + 16);
    text.push_str(scheme);
    if !scheme.is_empty() {
        text.push(BREAK);
    }
    for ch in rest.chars() {
        text.push(ch);
        if ch == '.' {
            text.push(BREAK);
        }
    }
    text
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
        Cut::End => 0,
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
    let end = match cut {
        Cut::Start => from_label(text, &end),
        Cut::Middle | Cut::End => &end,
    };
    format!("{start}…{end}")
}

/// The end of a host kept by `Cut::Start`, from the start of a label:
/// "…example.com", never "…xample.com" or "….example.com". It keeps at
/// least the domain that owns the host (or two labels, for names the
/// public suffix list doesn't know), cut short only when not even that
/// fits.
fn from_label<'a>(text: &str, end: &'a str) -> &'a str {
    let cut_off = &text[..text.len() - end.len()];
    let end_at_label = cut_off.is_empty() || cut_off.ends_with('.');
    let end = end.trim_start_matches('.');
    if end_at_label {
        return end;
    }
    let Some((_, rest)) = end.split_once('.') else {
        return end;
    };
    // A host's start (the permission card's first line) ends with a dot,
    // and the domain that owns it is elsewhere.
    let owner = psl::domain_str(text)
        .filter(|domain| !text.ends_with('.') && domain.len() < text.len())
        .map(str::len);
    let keeps_owner = match owner {
        Some(owner) => rest.len() >= owner,
        None => rest.contains('.'),
    };
    if keeps_owner { rest } else { end }
}
