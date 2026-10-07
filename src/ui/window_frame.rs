//! The browser window opens where it was last: on the same display, at the
//! same size, and in fullscreen if the user left it there.

use crate::model::SavedWindow;
use gpui::{App, Bounds, DisplayId, Pixels, Size, Window, WindowBounds, point, px, size};

/// The first window's size, and the smallest the window gets.
pub const DEFAULT_SIZE: Size<Pixels> = size(px(1280.), px(820.));
pub const MIN_SIZE: Size<Pixels> = size(px(820.), px(540.));

/// Where the window opens: its saved frame on its display, moved and
/// shrunk as needed to lie on it, or else centred on the main display.
pub fn initial(saved: Option<&SavedWindow>, cx: &App) -> (WindowBounds, Option<DisplayId>) {
    saved
        .and_then(|saved| {
            let display = match &saved.display {
                Some(uuid) => cx
                    .displays()
                    .into_iter()
                    .find(|display| display.uuid().is_ok_and(|id| id.to_string() == *uuid)),
                None => cx.primary_display(),
            }?;
            let bounds = fit(saved, display.bounds())?;
            let bounds = if saved.fullscreen {
                WindowBounds::Fullscreen(bounds)
            } else {
                WindowBounds::Windowed(bounds)
            };
            Some((bounds, Some(display.id())))
        })
        .unwrap_or_else(|| {
            let bounds = Bounds::centered(None, DEFAULT_SIZE, cx);
            (WindowBounds::Windowed(bounds), None)
        })
}

/// The saved frame moved and shrunk as little as needed to lie on a
/// display with `display` bounds, and no smaller than `MIN_SIZE` if the
/// display has room. `None` for no frame at all, or one off the display.
fn fit(saved: &SavedWindow, display: Bounds<Pixels>) -> Option<Bounds<Pixels>> {
    let SavedWindow {
        x,
        y,
        width,
        height,
        ..
    } = *saved;
    if ![x, y, width, height].iter().all(|value| value.is_finite()) {
        return None;
    }
    let frame = Bounds::new(point(px(x), px(y)), size(px(width), px(height)));
    if !frame.intersects(&display) {
        return None;
    }
    // Along one axis: the length that fits, then the start that keeps it on the display.
    let fit_axis = |start: f32, length: f32, min: f32, from: Pixels, extent: Pixels| {
        let (from, extent) = (f32::from(from), f32::from(extent));
        let length = length.max(min).min(extent);
        (start.clamp(from, from + extent - length), length)
    };
    let (x, width) = fit_axis(
        x,
        width,
        MIN_SIZE.width.into(),
        display.left(),
        display.size.width,
    );
    let (y, height) = fit_axis(
        y,
        height,
        MIN_SIZE.height.into(),
        display.top(),
        display.size.height,
    );
    Some(Bounds::new(
        point(px(x), px(y)),
        size(px(width), px(height)),
    ))
}

/// Follows the window's frame, for saving it.
#[derive(Default)]
pub struct WindowFrame {
    /// The display the window was on and its UUID. Looking the UUID up
    /// allocates, and the frame changes on every step of a resize.
    display: Option<(DisplayId, Option<String>)>,
}

impl WindowFrame {
    /// The window's frame to save. In fullscreen that is the frame it
    /// leaves fullscreen for, and it opens in fullscreen again only if a
    /// page didn't ask for it (`page_fullscreen`).
    pub fn saved(&mut self, window: &Window, cx: &App, page_fullscreen: bool) -> SavedWindow {
        let (bounds, fullscreen) = match window.window_bounds() {
            WindowBounds::Fullscreen(bounds) => (bounds, !page_fullscreen),
            WindowBounds::Windowed(bounds) | WindowBounds::Maximized(bounds) => (bounds, false),
        };
        let display = window.display(cx).and_then(|display| {
            let id = display.id();
            match &self.display {
                Some((known, uuid)) if *known == id => uuid.clone(),
                _ => {
                    let uuid = display.uuid().ok().map(|uuid| uuid.to_string());
                    self.display = Some((id, uuid.clone()));
                    uuid
                }
            }
        });
        SavedWindow {
            x: bounds.origin.x.into(),
            y: bounds.origin.y.into(),
            width: bounds.size.width.into(),
            height: bounds.size.height.into(),
            display,
            fullscreen,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(x: f32, y: f32, width: f32, height: f32) -> SavedWindow {
        SavedWindow {
            x,
            y,
            width,
            height,
            display: None,
            fullscreen: false,
        }
    }

    fn bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    }

    #[test]
    fn frames_that_fit_stay_as_they_were() {
        let display = bounds(0., 0., 1512., 982.);
        assert_eq!(
            fit(&frame(40., 33., 1300., 900.), display),
            Some(bounds(40., 33., 1300., 900.))
        );
        // Displays left of or above the main one have their own origin.
        assert_eq!(
            fit(
                &frame(-1800., 20., 1280., 820.),
                bounds(-1920., 0., 1920., 1080.)
            ),
            Some(bounds(-1800., 20., 1280., 820.))
        );
    }

    #[test]
    fn frames_move_and_shrink_onto_their_display() {
        let display = bounds(0., 0., 1440., 900.);
        // Partly off the right and bottom edges: moved back on.
        assert_eq!(
            fit(&frame(1000., 600., 1280., 820.), display),
            Some(bounds(160., 80., 1280., 820.))
        );
        // Larger than the display, as after switching to a smaller one.
        assert_eq!(
            fit(&frame(-30., -10., 2560., 1440.), display),
            Some(bounds(0., 0., 1440., 900.))
        );
        // Never below the minimum size, unless the display is smaller.
        assert_eq!(
            fit(&frame(10., 10., 300., 200.), display),
            Some(bounds(10., 10., 820., 540.))
        );
        assert_eq!(
            fit(&frame(10., 10., 300., 200.), bounds(0., 0., 800., 500.)),
            Some(bounds(0., 0., 800., 500.))
        );
    }

    #[test]
    fn frames_off_the_display_or_broken_are_dropped() {
        let display = bounds(0., 0., 1440., 900.);
        for broken in [
            frame(1500., 100., 800., 600.),
            frame(100., -700., 800., 600.),
            frame(f32::NAN, 0., 800., 600.),
            frame(0., 0., f32::INFINITY, 600.),
            frame(0., 0., 0., 600.),
            frame(0., 0., -800., 600.),
        ] {
            assert_eq!(fit(&broken, display), None, "{broken:?}");
        }
        assert_eq!(
            fit(&frame(0., 0., 800., 600.), bounds(0., 0., 0., 0.)),
            None
        );
    }
}
