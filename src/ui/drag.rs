//! Dragging in the sidebar: tabs move between Favorites, Pinned and Today
//! (arc-design.md §16), and the sidebar's right edge resizes it (§1).

use super::{
    UI_FONT, placeholder_favicon, shadow,
    sidebar::{FAVORITE_GAP, ROW_GAP},
    theme::Theme,
    truncated,
};
use crate::model::{MAX_SIDEBAR_WIDTH, MIN_SIDEBAR_WIDTH, Pin, TabId};
use gpui::{
    Bounds, Context, Image, IntoElement, ParentElement, Pixels, Point, Render, SharedString, Size,
    Styled, Window, div, img, point, prelude::FluentBuilder, px, size,
};
use std::{cell::Cell, rc::Rc, sync::Arc};

/// The edge handle's width; it lies inside the sidebar, against its edge.
pub const EDGE_HANDLE: f32 = 6.;
/// Dragging the edge further left than this hides the sidebar, like Arc.
const HIDE_BELOW: f32 = 120.;
const INDICATOR: f32 = 2.;
/// A dragged tab's size over the space switcher.
const COMPACT_PREVIEW: f32 = 30.;

/// A tab being dragged; the sidebar drops it where its indicator shows.
pub struct DraggedTab(pub TabId);

/// The sidebar's right edge being dragged.
pub struct DraggedEdge {
    /// The width when the drag started, kept if the drag hides the sidebar.
    pub start_width: f32,
}

/// The sidebar width, in whole points, for its edge dragged with the
/// pointer at `x` (the handle's middle), or `None` to hide the sidebar.
pub fn edge_width(x: f32) -> Option<f32> {
    (x >= HIDE_BELOW).then(|| {
        (x + EDGE_HANDLE / 2.)
            .round()
            .clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH)
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DropTarget {
    pub pin: Pin,
    /// Position in the section, counting the dragged tab if it is there.
    pub index: usize,
}

/// A tab drag in progress over the active space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabDrag {
    pub tab: TabId,
    /// `None` while the pointer is outside the tab list.
    pub target: Option<DropTarget>,
}

/// Where the active space's sidebar parts were last drawn, in window
/// coordinates, so a dragged tab finds its place among them.
#[derive(Debug, Default)]
pub struct DropZones {
    pub favorites: Vec<Bounds<Pixels>>,
    /// The top of the scrolling tab list, under the URL pill and favorites.
    pub list_top: Pixels,
    pub title: Bounds<Pixels>,
    pub pinned: Vec<Bounds<Pixels>>,
    pub divider: Bounds<Pixels>,
    pub new_tab: Bounds<Pixels>,
    pub today: Vec<Bounds<Pixels>>,
}

impl DropZones {
    /// Takes the tab list's parts as laid out: the space title, `pinned`
    /// rows, the divider, "+ New Tab" and the Today rows.
    pub fn measure_list(&mut self, parts: &[Bounds<Pixels>], pinned: usize) {
        let [title, rest @ ..] = parts else {
            return;
        };
        let (pinned, rest) = rest.split_at(pinned.min(rest.len()));
        let [divider, new_tab, today @ ..] = rest else {
            return;
        };
        self.title = *title;
        self.pinned = pinned.to_vec();
        self.divider = *divider;
        self.new_tab = *new_tab;
        self.today = today.to_vec();
    }

    /// The row, or tile, of the tab at `index` in `pin`'s section.
    pub fn tab(&self, pin: Pin, index: usize) -> Option<Bounds<Pixels>> {
        match pin {
            Pin::Favorite => &self.favorites,
            Pin::Pinned => &self.pinned,
            Pin::Today => &self.today,
        }
        .get(index)
        .copied()
    }

    /// Above the space title a tab becomes a favorite, above the divider a
    /// pinned tab, below it a Today tab (Arc).
    pub fn target(&self, pointer: Point<Pixels>) -> DropTarget {
        let y = pointer.y;
        let (pin, index) = if y < self.title.bottom().max(self.list_top) {
            (Pin::Favorite, tile_index(&self.favorites, pointer))
        } else if y < self.divider.center().y {
            (Pin::Pinned, row_index(&self.pinned, y))
        } else {
            (Pin::Today, row_index(&self.today, y))
        };
        DropTarget { pin, index }
    }

    /// The drop indicator for `target`: a line across the gap between rows,
    /// or upright between favorite tiles.
    pub fn indicator(&self, target: DropTarget) -> Bounds<Pixels> {
        let across = |y: Pixels| {
            Bounds::new(
                point(self.title.left(), y - px(INDICATOR / 2.)),
                size(self.title.size.width, px(INDICATOR)),
            )
        };
        // Midway between the row before the slot and the one after it.
        let between = |rows: &[Bounds<Pixels>], first_above: Bounds<Pixels>, after: Option<_>| {
            let above = target
                .index
                .checked_sub(1)
                .and_then(|index| rows.get(index))
                .unwrap_or(&first_above);
            let below: Option<&Bounds<Pixels>> = rows.get(target.index).or(after);
            across(below.map_or(above.bottom() + px(ROW_GAP / 2.), |below| {
                (above.bottom() + below.top()) * 0.5
            }))
        };
        match target.pin {
            Pin::Favorite => {
                let upright = |x: Pixels, tile: &Bounds<Pixels>| {
                    Bounds::new(
                        point(x - px(INDICATOR / 2.), tile.top()),
                        size(px(INDICATOR), tile.size.height),
                    )
                };
                let gap = px(FAVORITE_GAP / 2.);
                match (self.favorites.get(target.index), self.favorites.last()) {
                    (Some(tile), _) => upright(tile.left() - gap, tile),
                    (None, Some(last)) => upright(last.right() + gap, last),
                    // The first favorite: under the URL pill, where its row will be.
                    (None, None) => across(self.list_top + px(4.)),
                }
            }
            Pin::Pinned => between(&self.pinned, self.title, Some(&self.divider)),
            Pin::Today => between(&self.today, self.new_tab, None),
        }
    }
}

/// Rows whose middle is above the pointer come before it.
fn row_index(rows: &[Bounds<Pixels>], y: Pixels) -> usize {
    rows.partition_point(|row| row.center().y < y)
}

/// Tiles in reading order: the slot is before the first tile in a later row
/// than the pointer, or in its row and right of it.
fn tile_index(tiles: &[Bounds<Pixels>], pointer: Point<Pixels>) -> usize {
    let gap = px(FAVORITE_GAP / 2.);
    tiles
        .iter()
        .position(|tile| {
            pointer.y < tile.top() - gap
                || (pointer.y <= tile.bottom() + gap && pointer.x < tile.center().x)
        })
        .unwrap_or(tiles.len())
}

/// A dragged tab under the pointer: its row or favorite tile, lifted. Over
/// the space switcher it shrinks to its icon beside the pointer, leaving
/// the space it would move to in sight.
#[derive(Clone)]
pub struct TabPreview {
    pub title: SharedString,
    pub host: Option<String>,
    pub favicon: Option<Arc<Image>>,
    pub theme: Theme,
    pub size: Size<Pixels>,
    pub tile: bool,
    /// Where the pointer holds it.
    pub cursor: Point<Pixels>,
    /// Set while the pointer is over the space switcher.
    pub over_spaces: Rc<Cell<bool>>,
}

impl Render for TabPreview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let compact = self.over_spaces.get();
        let icon_size = if self.tile || compact { 20. } else { 16. };
        let favicon = match self.favicon.clone() {
            Some(image) => img(image)
                .size(px(icon_size))
                .flex_none()
                .into_any_element(),
            None => placeholder_favicon(self.host.as_deref(), &self.theme).into_any_element(),
        };
        if compact {
            return div().w(self.size.width).h(self.size.height).child(
                div()
                    .absolute()
                    .left(self.cursor.x + px(8.))
                    .top(self.cursor.y - px(COMPACT_PREVIEW + 8.))
                    .size(px(COMPACT_PREVIEW))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.))
                    .bg(self.theme.surface())
                    .border_1()
                    .border_color(self.theme.ink(0.08))
                    .shadow(vec![shadow(4., 14., 0.16)])
                    .child(favicon),
            );
        }
        div()
            .font_family(UI_FONT)
            .flex()
            .items_center()
            .w(self.size.width)
            .h(self.size.height)
            .rounded(px(10.))
            .bg(self.theme.surface())
            .border_1()
            .border_color(self.theme.ink(0.08))
            .shadow(vec![shadow(4., 14., 0.16)])
            .opacity(0.92)
            .map(|preview| {
                if self.tile {
                    preview.justify_center().child(favicon)
                } else {
                    preview.pl(px(9.5)).pr(px(10.)).child(favicon).child(
                        truncated(self.title.clone())
                            .flex_1()
                            .min_w_0()
                            .ml(px(11.5))
                            .text_size(px(14.))
                            .text_color(self.theme.title()),
                    )
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    }

    /// Rows 36 tall at a 41 pitch from `top`.
    fn rows(top: f32, count: usize) -> Vec<Bounds<Pixels>> {
        (0..count)
            .map(|index| rect(8., top + 41. * index as f32, 212., 36.))
            .collect()
    }

    /// The demo space: three favorites, two pinned tabs and three Today tabs.
    fn zones() -> DropZones {
        DropZones {
            favorites: (0..3)
                .map(|index| rect(8. + 73.5 * index as f32, 86., 65., 47.))
                .collect(),
            list_top: px(133.),
            title: rect(8., 142., 212., 34.),
            pinned: rows(178., 2),
            divider: rect(18., 260., 192., 12.),
            new_tab: rect(8., 277., 212., 36.),
            today: rows(318., 3),
        }
    }

    #[test]
    fn the_list_is_measured_part_by_part() {
        let demo = zones();
        let mut parts = vec![demo.title];
        parts.extend(&demo.pinned);
        parts.extend([demo.divider, demo.new_tab]);
        parts.extend(&demo.today);
        let mut measured = DropZones::default();
        measured.measure_list(&parts, 2);
        assert_eq!(measured.title, demo.title);
        assert_eq!(measured.pinned, demo.pinned);
        assert_eq!(measured.divider, demo.divider);
        assert_eq!(measured.new_tab, demo.new_tab);
        assert_eq!(measured.today, demo.today);
        // A list that doesn't match the space is ignored.
        let mut stale = DropZones::default();
        stale.measure_list(&parts[..2], 2);
        assert!(stale.pinned.is_empty());
    }

    fn at(x: f32, y: f32) -> Point<Pixels> {
        point(px(x), px(y))
    }

    fn target(pin: Pin, index: usize) -> DropTarget {
        DropTarget { pin, index }
    }

    #[test]
    fn rows_drop_before_the_first_row_whose_middle_is_below_the_pointer() {
        let zones = zones();
        assert_eq!(zones.target(at(100., 180.)), target(Pin::Pinned, 0));
        assert_eq!(zones.target(at(100., 200.)), target(Pin::Pinned, 1));
        assert_eq!(zones.target(at(100., 255.)), target(Pin::Pinned, 2));
        // Below the divider's middle: Today, the "+ New Tab" row included.
        assert_eq!(zones.target(at(100., 268.)), target(Pin::Today, 0));
        assert_eq!(zones.target(at(100., 300.)), target(Pin::Today, 0));
        assert_eq!(zones.target(at(100., 345.)), target(Pin::Today, 1));
        // Empty space under the last row appends.
        assert_eq!(zones.target(at(100., 700.)), target(Pin::Today, 3));
    }

    #[test]
    fn above_the_space_title_tabs_become_favorites() {
        let zones = zones();
        // Over the tiles: before the tile whose middle is right of the pointer.
        assert_eq!(zones.target(at(20., 100.)), target(Pin::Favorite, 0));
        assert_eq!(zones.target(at(60., 100.)), target(Pin::Favorite, 1));
        assert_eq!(zones.target(at(215., 100.)), target(Pin::Favorite, 3));
        // The title row and the URL pill add to the end.
        assert_eq!(zones.target(at(100., 160.)), target(Pin::Favorite, 3));
        assert_eq!(zones.target(at(100., 50.)), target(Pin::Favorite, 0));
        // A space without favorites still takes them above its title.
        let empty = DropZones {
            favorites: Vec::new(),
            ..zones
        };
        assert_eq!(empty.target(at(100., 160.)), target(Pin::Favorite, 0));
    }

    #[test]
    fn wrapped_favorites_take_the_row_under_the_pointer() {
        // Five favorites: four in the first row, one in the second.
        let tiles: Vec<_> = (0..5)
            .map(|index| {
                rect(
                    8. + 55.75 * (index % 4) as f32,
                    86. + 56. * (index / 4) as f32,
                    46.75,
                    47.,
                )
            })
            .collect();
        assert_eq!(tile_index(&tiles, at(215., 100.)), 4);
        assert_eq!(tile_index(&tiles, at(20., 150.)), 4);
        assert_eq!(tile_index(&tiles, at(100., 150.)), 5);
        assert_eq!(tile_index(&tiles, at(100., 10.)), 0);
        assert_eq!(tile_index(&[], at(100., 100.)), 0);
    }

    #[test]
    fn a_scrolled_list_keeps_favorites_above_its_visible_top() {
        // No favorites, the list under the URL pill scrolled by 120: the
        // title is out of view and the pill still takes new favorites.
        let mut zones = DropZones {
            favorites: Vec::new(),
            list_top: px(78.),
            ..zones()
        };
        let parts = [&mut zones.title, &mut zones.divider, &mut zones.new_tab];
        for part in parts
            .into_iter()
            .chain(&mut zones.pinned)
            .chain(&mut zones.today)
        {
            part.origin.y -= px(120.);
        }
        assert_eq!(zones.target(at(100., 60.)), target(Pin::Favorite, 0));
        assert_eq!(zones.target(at(100., 100.)), target(Pin::Pinned, 1));
    }

    #[test]
    fn indicators_sit_in_the_gaps() {
        let demo = zones();
        let centre = |zones: &DropZones, pin, index| {
            let bounds = zones.indicator(target(pin, index));
            (f32::from(bounds.center().x), f32::from(bounds.center().y))
        };
        // Lines across the rows: between two, after the last, before the first.
        assert_eq!(centre(&demo, Pin::Pinned, 1), (114., 216.5));
        assert_eq!(centre(&demo, Pin::Pinned, 2), (114., 257.5));
        assert_eq!(centre(&demo, Pin::Today, 0), (114., 315.5));
        assert_eq!(centre(&demo, Pin::Today, 3), (114., 438.5));
        // Empty sections: under the title, under "+ New Tab", under the pill.
        let empty = DropZones {
            favorites: Vec::new(),
            pinned: Vec::new(),
            today: Vec::new(),
            ..zones()
        };
        assert_eq!(centre(&empty, Pin::Pinned, 0), (114., 218.));
        assert_eq!(centre(&empty, Pin::Today, 0), (114., 315.5));
        assert_eq!(centre(&empty, Pin::Favorite, 0), (114., 137.));
        // Between favorite tiles the line stands upright in the gap.
        let upright = demo.indicator(target(Pin::Favorite, 1));
        assert_eq!(f32::from(upright.size.width), INDICATOR);
        assert_eq!(f32::from(upright.size.height), 47.);
        assert_eq!(centre(&demo, Pin::Favorite, 1), (77., 109.5));
        assert_eq!(centre(&demo, Pin::Favorite, 3), (224.5, 109.5));
    }

    #[test]
    fn the_edge_resizes_within_limits_and_hides_the_sidebar_far_left() {
        // The pointer holds the handle's middle, 3 pt inside the edge.
        assert_eq!(edge_width(225.), Some(228.));
        assert_eq!(edge_width(300.), Some(303.));
        assert_eq!(edge_width(200.4), Some(203.));
        assert_eq!(edge_width(900.), Some(MAX_SIDEBAR_WIDTH));
        assert_eq!(edge_width(150.), Some(MIN_SIDEBAR_WIDTH));
        assert_eq!(edge_width(HIDE_BELOW), Some(MIN_SIDEBAR_WIDTH));
        assert_eq!(edge_width(119.), None);
        assert_eq!(edge_width(-20.), None);
    }
}
