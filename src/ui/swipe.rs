//! Two-finger horizontal swipes on the sidebar switch spaces (arc-design.md
//! §16): one switch per gesture, once it has travelled far enough sideways.

use gpui::{Point, ScrollDelta, TouchPhase};

/// Sideways travel that makes a gesture a swipe.
const THRESHOLD: f32 = 70.;
/// Vertical travel after which a gesture scrolls the tab list for good.
const VERTICAL: f32 = 12.;

#[derive(Debug, Default)]
pub struct SwipeTracker {
    /// The current gesture's travel, while it may still become a swipe.
    travel: Option<Point<f32>>,
}

impl SwipeTracker {
    /// Follows one scroll event. Returns whether to go to the next space
    /// (fingers moving left) or the previous one, the moment the gesture
    /// becomes a swipe. Wheels without touch phases, the rest of a gesture
    /// after its switch and the momentum after the fingers lift never count.
    pub fn update(&mut self, phase: TouchPhase, delta: ScrollDelta) -> Option<bool> {
        let (TouchPhase::Moved, ScrollDelta::Pixels(delta)) = (phase, delta) else {
            self.travel = matches!(phase, TouchPhase::Started).then(Point::default);
            return None;
        };
        let travel = self.travel.as_mut()?;
        travel.x += f32::from(delta.x);
        travel.y += f32::from(delta.y);
        if travel.y.abs() >= travel.x.abs() {
            if travel.y.abs() > VERTICAL {
                self.travel = None;
            }
            return None;
        }
        let next = travel.x < 0.;
        (travel.x.abs() >= THRESHOLD).then(|| {
            self.travel = None;
            next
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px};

    fn pixels(x: f32, y: f32) -> ScrollDelta {
        ScrollDelta::Pixels(point(px(x), px(y)))
    }

    /// Feeds a gesture: Started, the moves, Ended. Returns each result.
    fn gesture(tracker: &mut SwipeTracker, moves: &[(f32, f32)]) -> Vec<Option<bool>> {
        tracker.update(TouchPhase::Started, pixels(0., 0.));
        let results = moves
            .iter()
            .map(|&(x, y)| tracker.update(TouchPhase::Moved, pixels(x, y)))
            .collect();
        tracker.update(TouchPhase::Ended, pixels(0., 0.));
        results
    }

    #[test]
    fn a_sideways_gesture_switches_once_past_the_threshold() {
        let mut tracker = SwipeTracker::default();
        // Fingers moving left go to the next space, right to the previous one.
        let left = gesture(
            &mut tracker,
            &[(-30., 2.), (-30., -3.), (-15., 0.), (-60., 0.)],
        );
        assert_eq!(left, [None, None, Some(true), None]);
        let right = gesture(&mut tracker, &[(40., 4.), (40., 0.)]);
        assert_eq!(right, [None, Some(false)]);
        // Short of the threshold nothing happens.
        assert_eq!(
            gesture(&mut tracker, &[(-35., 0.), (-30., 0.)]),
            [None, None]
        );
    }

    #[test]
    fn momentum_after_the_fingers_lift_is_ignored() {
        let mut tracker = SwipeTracker::default();
        assert_eq!(gesture(&mut tracker, &[(-40., 0.)]), [None]);
        // Momentum events come as moves without a new start.
        for _ in 0..5 {
            assert_eq!(tracker.update(TouchPhase::Moved, pixels(-40., 0.)), None);
        }
    }

    #[test]
    fn vertical_scrolling_stays_vertical() {
        let mut tracker = SwipeTracker::default();
        // Once the list scrolls, later sideways travel doesn't switch spaces.
        let scroll = gesture(&mut tracker, &[(3., -20.), (-90., 0.), (-90., 0.)]);
        assert_eq!(scroll, [None, None, None]);
        // A little vertical wobble at the start of a swipe is fine.
        assert_eq!(
            gesture(&mut tracker, &[(1., 6.), (-40., 2.), (-40., 0.)]),
            [None, None, Some(true)]
        );
    }

    #[test]
    fn wheels_without_touch_phases_never_swipe() {
        let mut tracker = SwipeTracker::default();
        let lines = ScrollDelta::Lines(point(-10., 0.));
        assert_eq!(tracker.update(TouchPhase::Moved, lines), None);
        tracker.update(TouchPhase::Started, pixels(0., 0.));
        assert_eq!(tracker.update(TouchPhase::Moved, lines), None);
        assert_eq!(tracker.update(TouchPhase::Moved, pixels(-80., 0.)), None);
    }
}
