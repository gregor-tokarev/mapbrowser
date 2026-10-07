//! Which page events need a redraw: a redraw costs a render, a present and
//! about ten display-link wakeups, so only changes to something on screen
//! get one. Whatever is skipped is drawn when it shows again (showing the
//! sidebar, leaving fullscreen and switching spaces redraw everything).

use crate::model::Pin;

/// What the window draws of one tab.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Drawn {
    /// Its title, in its sidebar row. Favorite tiles show none.
    pub title: bool,
    /// Its host: the favicon, and the URL pill while selected. Previews
    /// without Chromium also print the active tab's address on the card.
    pub address: bool,
    /// Its loading progress and whether it can go back or forward.
    pub status: bool,
}

/// What is drawn of a tab that is `listed` in a space the sidebar shows
/// (the active one, or the one a space switch leaves) or is the `active`
/// tab, with the sidebar shown or not, and a page in fullscreen or not.
pub fn drawn(sidebar: bool, listed: bool, pin: Pin, active: bool, fullscreen: bool) -> Drawn {
    let in_sidebar = sidebar && listed;
    Drawn {
        title: in_sidebar && pin != Pin::Favorite,
        address: in_sidebar || (cfg!(not(target_os = "macos")) && active),
        status: active && !fullscreen,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn favorites_show_their_icon_but_no_title() {
        let favorite = drawn(true, true, Pin::Favorite, false, false);
        assert!(!favorite.title && favorite.address && !favorite.status);
        for pin in [Pin::Pinned, Pin::Today] {
            let row = drawn(true, true, pin, false, false);
            assert!(row.title && row.address && !row.status);
        }
    }

    #[test]
    fn a_hidden_sidebar_shows_only_the_active_page_status() {
        let active = drawn(false, true, Pin::Today, true, false);
        assert!(!active.title && active.status);
        assert_eq!(active.address, cfg!(not(target_os = "macos")));
        assert_eq!(
            drawn(false, true, Pin::Today, false, false),
            Drawn::default()
        );
    }

    #[test]
    fn a_fullscreen_page_shows_nothing_of_any_tab() {
        // A fullscreen page hides the sidebar too.
        let active = drawn(false, true, Pin::Today, true, true);
        assert!(!active.title && !active.status);
        assert_eq!(
            drawn(false, true, Pin::Pinned, false, true),
            Drawn::default()
        );
    }

    #[test]
    fn tabs_of_hidden_spaces_show_nothing() {
        for pin in [Pin::Favorite, Pin::Pinned, Pin::Today] {
            assert_eq!(drawn(true, false, pin, false, false), Drawn::default());
        }
        let active = drawn(true, true, Pin::Today, true, false);
        assert_eq!(
            active,
            Drawn {
                title: true,
                address: true,
                status: true
            }
        );
    }
}
