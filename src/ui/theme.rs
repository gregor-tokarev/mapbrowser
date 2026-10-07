//! Space themes. Each palette is an Arc-like window gradient plus the few
//! colours Arc tints per theme (titles, focus, command bar surface), taken
//! from rendered Arc windows rather than its picker colours. Everything else
//! is black or white at a fixed alpha, so it takes the gradient's hue.

use crate::model::THEME_COUNT;
use gpui::{
    Background, Hsla, Pixels, Point, Rgba, Size, hsla, linear_color_stop, linear_gradient, rgb,
    rgba,
};

/// The window gradient, fitted to Arc's lavender window: the second colour
/// is reached 70% of the way along a 150° line (CSS degrees).
const GRADIENT_ANGLE: f32 = 150.;
const GRADIENT_END: f32 = 0.7;

/// 0xRRGGBB colours of one appearance of a palette.
struct Tones {
    from: u32,
    to: u32,
    title: u32,
    focus: u32,
    surface: u32,
}

const fn tones(from: u32, to: u32, title: u32, focus: u32, surface: u32) -> Tones {
    Tones {
        from,
        to,
        title,
        focus,
        surface,
    }
}

/// `[light, dark]` per theme index: lavender (Arc's default), periwinkle,
/// green, peach, pink, teal, grey and amber.
const PALETTES: [[Tones; 2]; THEME_COUNT as usize] = [
    [
        tones(0xe2e3fa, 0xf9eded, 0x0a0d4e, 0x8d92fb, 0xfdfdff),
        tones(0x26273c, 0x2d2633, 0xdcdefd, 0x5d63f0, 0x1d1e30),
    ],
    [
        tones(0xd6e3fb, 0xe4e6fd, 0x000c33, 0x6f8cf0, 0xfcfdff),
        tones(0x1e2642, 0x262843, 0xdde4fd, 0x5a73e6, 0x1a2037),
    ],
    [
        tones(0xd3efd8, 0xcbe7d5, 0x001e17, 0x5cb978, 0xfbfefb),
        tones(0x1c2c23, 0x1a2a26, 0xd7f2df, 0x3f9a5c, 0x17241d),
    ],
    [
        tones(0xfbd2b8, 0xf4b5a3, 0x320e03, 0xd39778, 0xfefaf7),
        tones(0x3b2920, 0x36211f, 0xfbe2d6, 0xb8704f, 0x2d201a),
    ],
    [
        tones(0xf9d3e0, 0xeec6e5, 0x3b0b25, 0xdf7aa6, 0xfffafc),
        tones(0x3b2230, 0x322036, 0xfadbe8, 0xc0567f, 0x2e1b26),
    ],
    [
        tones(0xc9eef0, 0xc2e2ea, 0x002a2e, 0x3aa6b3, 0xf9feff),
        tones(0x182d31, 0x172629, 0xd3f1f3, 0x2c8c98, 0x142427),
    ],
    [
        tones(0xededed, 0xe5e5e6, 0x18181b, 0x7f7f87, 0xfafafa),
        tones(0x2b2b2e, 0x232326, 0xececef, 0x6c6c74, 0x222225),
    ],
    [
        tones(0xf7e6b8, 0xf1d39b, 0x2d1d00, 0xc49a3a, 0xfffcf3),
        tones(0x362d19, 0x2f2617, 0xf6ebcf, 0xa8822c, 0x2a2314),
    ],
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub dark: bool,
    from: Rgba,
    to: Rgba,
    title: Rgba,
    focus: Rgba,
    surface: Rgba,
}

impl Theme {
    pub fn new(index: u8, dark: bool) -> Self {
        let tones = &PALETTES[usize::from(index % THEME_COUNT)][usize::from(dark)];
        Self {
            dark,
            from: rgb(tones.from),
            to: rgb(tones.to),
            title: rgb(tones.title),
            focus: rgb(tones.focus),
            surface: rgb(tones.surface),
        }
    }

    /// The theme `t` of the way from `self` to `other`, for space switches.
    pub fn lerp(&self, other: &Self, t: f32) -> Self {
        if t >= 1. {
            return *other;
        }
        Self {
            dark: other.dark,
            from: mix(self.from, other.from, t),
            to: mix(self.to, other.to, t),
            title: mix(self.title, other.title, t),
            focus: mix(self.focus, other.focus, t),
            surface: mix(self.surface, other.surface, t),
        }
    }

    pub fn background(&self) -> Background {
        linear_gradient(
            GRADIENT_ANGLE,
            linear_color_stop(self.from, 0.),
            linear_color_stop(self.to, GRADIENT_END),
        )
    }

    /// The window background at `point` of a window of `size`, computed the
    /// way GPUI's quad shader fills `background()`.
    pub fn background_at(&self, point: Point<Pixels>, size: Size<Pixels>) -> Rgba {
        let (width, height) = (f32::from(size.width), f32::from(size.height));
        if width <= 0. || height <= 0. {
            return self.from;
        }
        let radians = (GRADIENT_ANGLE % 360. - 90.).to_radians();
        let (mut dx, mut dy) = (radians.cos(), radians.sin());
        if width > height {
            dy *= height / width;
        } else {
            dx *= width / height;
        }
        let (x, y) = (
            f32::from(point.x) - width / 2.,
            f32::from(point.y) - height / 2.,
        );
        let along = (x * dx + y * dy) / dx.hypot(dy);
        let t = if dx.abs() > dy.abs() {
            (along + width / 2.) / width
        } else {
            (along + height / 2.) / height
        };
        mix(self.from, self.to, (t / GRADIENT_END).clamp(0., 1.))
    }

    /// Black in light themes and white in dark ones, at `alpha`.
    pub fn ink(&self, alpha: f32) -> Hsla {
        hsla(0., 0., if self.dark { 1. } else { 0. }, alpha)
    }

    fn by_mode(&self, light: f32, dark: f32) -> Hsla {
        self.ink(if self.dark { dark } else { light })
    }

    /// Tab titles and other primary text.
    pub fn title(&self) -> Hsla {
        self.title.into()
    }

    /// Toolbar glyphs.
    pub fn icon(&self) -> Hsla {
        self.by_mode(0.58, 0.62)
    }

    /// The URL pill's hostname.
    pub fn url_text(&self) -> Hsla {
        self.by_mode(0.62, 0.70)
    }

    /// The space title.
    pub fn muted(&self) -> Hsla {
        self.by_mode(0.45, 0.50)
    }

    /// "New Tab", "Clear", placeholders.
    pub fn tertiary(&self) -> Hsla {
        self.by_mode(0.32, 0.42)
    }

    /// Unavailable navigation buttons, inactive space dots.
    pub fn disabled(&self) -> Hsla {
        self.by_mode(0.22, 0.25)
    }

    /// The URL pill and favorite tiles.
    pub fn control(&self) -> Hsla {
        self.by_mode(0.05, 0.06)
    }

    pub fn control_hover(&self) -> Hsla {
        self.by_mode(0.09, 0.10)
    }

    /// Hovered rows and buttons.
    pub fn hover(&self) -> Hsla {
        self.by_mode(0.055, 0.07)
    }

    /// The selected row: a near-white pill, or a light veil in dark themes.
    pub fn selected(&self) -> Hsla {
        if self.dark {
            hsla(0., 0., 1., 0.12)
        } else {
            rgba(0xf6f6f8f2).into()
        }
    }

    pub fn divider(&self) -> Hsla {
        self.by_mode(0.14, 0.12)
    }

    /// Highlights: the command bar's selected row, a selected favorite's border.
    pub fn focus(&self) -> Hsla {
        self.focus.into()
    }

    /// Buttons that delete something, with white text.
    pub fn danger(&self) -> Hsla {
        rgb(0xd13b33).into()
    }

    /// The command bar and menus.
    pub fn surface(&self) -> Hsla {
        self.surface.into()
    }

    /// The page card before a page paints: Chromium's white, or near-black.
    pub fn card(&self) -> Hsla {
        if self.dark {
            rgb(0x1f1f22).into()
        } else {
            gpui::white()
        }
    }

    /// The pale, theme-tinted card of an empty space.
    pub fn empty_card(&self) -> Hsla {
        if self.dark {
            mix(self.from, rgb(0x1f1f22), 0.45).into()
        } else {
            mix(self.from, rgb(0xffffff), 0.6).into()
        }
    }

    /// A colour swatch of the theme for the space menu.
    pub fn swatch(&self) -> Background {
        linear_gradient(
            135.,
            linear_color_stop(self.from, 0.),
            linear_color_stop(self.focus, 1.),
        )
    }
}

fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    let lerp = |a: f32, b: f32| a + (b - a) * t;
    Rgba {
        r: lerp(a.r, b.r),
        g: lerp(a.g, b.g),
        b: lerp(a.b, b.b),
        a: lerp(a.a, b.a),
    }
}

/// `color` under black at `alpha`, as 0xAARRGGBB (opaque).
pub fn darkened_argb(color: Rgba, alpha: f32) -> u32 {
    let channel = |value: f32| (value * (1. - alpha) * 255.).round().clamp(0., 255.) as u32;
    0xff00_0000 | channel(color.r) << 16 | channel(color.g) << 8 | channel(color.b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px, size};

    fn hex(color: Rgba) -> u32 {
        darkened_argb(color, 0.) & 0xff_ffff
    }

    #[test]
    fn every_theme_has_both_appearances() {
        for index in 0..THEME_COUNT {
            let light = Theme::new(index, false);
            let dark = Theme::new(index, true);
            assert!(!light.dark && dark.dark);
            assert_ne!(light, dark);
        }
        // Indices wrap like `Space::theme`.
        assert_eq!(Theme::new(THEME_COUNT, false), Theme::new(0, false));
    }

    #[test]
    fn lerp_moves_every_colour_between_the_themes() {
        let lavender = Theme::new(0, false);
        let green = Theme::new(2, false);
        assert_eq!(lavender.lerp(&green, 0.), lavender);
        assert_eq!(lavender.lerp(&green, 1.), green);
        let half = lavender.lerp(&green, 0.5);
        for (a, b, mid) in [
            (lavender.from, green.from, half.from),
            (lavender.title, green.title, half.title),
            (lavender.focus, green.focus, half.focus),
        ] {
            for (a, b, mid) in [(a.r, b.r, mid.r), (a.g, b.g, mid.g), (a.b, b.b, mid.b)] {
                assert!((mid - (a + b) / 2.).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn background_samples_follow_the_gradient_direction() {
        let theme = Theme::new(0, false);
        let window = size(px(1440.), px(900.));
        // The far corners are the end colours; the gradient is clamped there.
        assert_eq!(
            hex(theme.background_at(point(px(0.), px(0.)), window)),
            0xe2e3fa
        );
        assert_eq!(
            hex(theme.background_at(point(px(1440.), px(900.)), window)),
            0xf9eded
        );
        // The centre is halfway along the line, so 0.5 / 0.7 of the way.
        let centre = theme.background_at(point(px(720.), px(450.)), window);
        let expected = theme.from.r + (theme.to.r - theme.from.r) * 0.5 / GRADIENT_END;
        assert!((centre.r - expected).abs() < 1e-3);
        let top_right = theme.background_at(point(px(1440.), px(0.)), window);
        let bottom_left = theme.background_at(point(px(0.), px(900.)), window);
        assert!(top_right.r > bottom_left.r);
    }

    #[test]
    fn darkening_composites_black_over_the_colour() {
        assert_eq!(darkened_argb(rgb(0xffffff), 0.), 0xffff_ffff);
        assert_eq!(darkened_argb(rgb(0xffffff), 0.5), 0xff80_8080);
        assert_eq!(darkened_argb(rgb(0x204060), 1.), 0xff00_0000);
    }
}
