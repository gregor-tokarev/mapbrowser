//! ⌃Tab goes back to recently used tabs, like Arc's switcher: each space
//! remembers the order its tabs were last shown in. Holding ⌃ and pressing
//! Tab again walks further back; releasing ⌃ goes to the tab reached.
//!
//! Per space, because only the active space's tabs are in the sidebar,
//! where the walk is shown, and a space keeps one context (work, home):
//! flipping back never jumps to another space.

use crate::model::{Space, SpaceId, TabId};
use std::collections::HashMap;

/// How many tabs a walk goes through, the current one included (Arc: 5).
const LIMIT: usize = 5;

/// Each space's tabs, the most recently shown first. Kept in memory only.
#[derive(Default)]
pub struct RecentTabs(HashMap<SpaceId, Vec<TabId>>);

impl RecentTabs {
    /// Notes `space`'s selected tab as its most recent one, forgetting
    /// tabs no longer in it.
    pub fn visit(&mut self, space: &Space) {
        let Some(selected) = space.selected_tab_id else {
            return;
        };
        let recent = self.0.entry(space.id).or_default();
        if recent.first() == Some(&selected) {
            return;
        }
        recent.retain(|&id| id != selected && space.tabs.iter().any(|tab| tab.id == id));
        recent.insert(0, selected);
        recent.truncate(LIMIT);
    }

    pub fn forget_space(&mut self, id: SpaceId) {
        self.0.remove(&id);
    }

    /// The tabs a walk in `space` goes through, its selected tab first:
    /// the most recently shown ones, then, while there are fewer than five
    /// (after a launch), the next ones in sidebar order.
    pub fn order(&self, space: &Space) -> Vec<TabId> {
        let tabs = &space.tabs;
        let Some(start) = space
            .selected_tab_id
            .and_then(|id| tabs.iter().position(|tab| tab.id == id))
        else {
            return Vec::new();
        };
        let recent = self.0.get(&space.id).into_iter().flatten().copied();
        let sidebar = tabs[start..].iter().chain(&tabs[..start]).map(|tab| tab.id);
        let mut order = vec![tabs[start].id];
        for id in recent.chain(sidebar) {
            if order.len() == LIMIT {
                break;
            }
            if !order.contains(&id) && tabs.iter().any(|tab| tab.id == id) {
                order.push(id);
            }
        }
        order
    }
}

/// A ⌃Tab walk while ⌃ is held.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabSwitch {
    order: Vec<TabId>,
    index: usize,
}

impl TabSwitch {
    /// ⌃Tab starts at the previous tab, ⌃⇧Tab at the oldest one. `None`
    /// with no other tab to go to.
    pub fn start(order: Vec<TabId>, back: bool) -> Option<Self> {
        let mut switch = (order.len() > 1).then_some(Self { order, index: 0 })?;
        switch.step(back);
        Some(switch)
    }

    /// Tab goes one tab further back, ⇧Tab one forward, round to the
    /// current tab and on.
    pub fn step(&mut self, back: bool) {
        let count = self.order.len();
        self.index = if back {
            (self.index + 1) % count
        } else {
            (self.index + count - 1) % count
        };
    }

    /// Where releasing ⌃ goes.
    pub fn target(&self) -> TabId {
        self.order[self.index]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::BrowserState;

    /// A space with tabs 0 to `count - 1` in sidebar order, tab 0 selected.
    fn space(count: usize) -> (BrowserState, SpaceId, Vec<TabId>) {
        let mut state = BrowserState::default();
        let space = state.selected_space_id;
        let mut tabs = (0..count)
            .map(|n| state.new_tab(space, format!("{n}.test")).unwrap())
            .collect::<Vec<_>>();
        // New tabs open at the top.
        tabs.reverse();
        state.select_tab(space, tabs[0]);
        (state, space, tabs)
    }

    fn show(recent: &mut RecentTabs, state: &mut BrowserState, space: SpaceId, tab: TabId) {
        state.select_tab(space, tab);
        recent.visit(state.space(space).unwrap());
    }

    #[test]
    fn the_most_recent_tabs_come_first_then_the_sidebar() {
        let (mut state, space, t) = space(8);
        let mut recent = RecentTabs::default();
        // After a launch only the sidebar order is known.
        assert_eq!(recent.order(state.space(space).unwrap()), t[..5]);
        for tab in [t[6], t[2], t[7], t[2], t[4]] {
            show(&mut recent, &mut state, space, tab);
        }
        assert_eq!(
            recent.order(state.space(space).unwrap()),
            [t[4], t[2], t[7], t[6], t[5]]
        );
        // Closed tabs drop out.
        state.close_tab(space, t[7]);
        show(&mut recent, &mut state, space, t[3]);
        assert_eq!(
            recent.order(state.space(space).unwrap()),
            [t[3], t[4], t[2], t[6], t[5]]
        );
        // At most five are remembered, however many were shown.
        for tab in [t[0], t[1], t[2], t[3], t[4], t[5]] {
            show(&mut recent, &mut state, space, tab);
        }
        assert_eq!(
            recent.order(state.space(space).unwrap()),
            [t[5], t[4], t[3], t[2], t[1]]
        );
    }

    #[test]
    fn spaces_keep_their_own_order() {
        let (mut state, personal, t) = space(4);
        let work = state.spaces[1].id;
        let other = state.new_tab(work, "work.test").unwrap();
        let mut recent = RecentTabs::default();
        show(&mut recent, &mut state, personal, t[3]);
        show(&mut recent, &mut state, work, other);
        show(&mut recent, &mut state, personal, t[1]);
        let order = |recent: &RecentTabs| recent.order(state.space(personal).unwrap());
        assert_eq!(order(&recent), [t[1], t[3], t[2], t[0]]);
        assert_eq!(recent.order(state.space(work).unwrap()), [other]);
        recent.forget_space(personal);
        assert_eq!(order(&recent), [t[1], t[2], t[3], t[0]]);
        // An empty space has nothing to walk.
        let empty = state.create_space("Empty");
        assert!(recent.order(state.space(empty).unwrap()).is_empty());
    }

    #[test]
    fn walks_go_back_and_forth_and_wrap_round() {
        let walk = |back: bool, steps: &[bool]| {
            let mut switch = TabSwitch::start(vec![1, 2, 3, 4], back).unwrap();
            for &back in steps {
                switch.step(back);
            }
            switch.target()
        };
        assert_eq!(walk(true, &[]), 2);
        assert_eq!(walk(true, &[true]), 3);
        assert_eq!(walk(true, &[true, true, true]), 1);
        assert_eq!(walk(true, &[true, false]), 2);
        assert_eq!(walk(false, &[]), 4);
        assert_eq!(walk(false, &[false]), 3);
        assert_eq!(walk(false, &[true]), 1);
        assert_eq!(TabSwitch::start(vec![1], true), None);
        assert_eq!(TabSwitch::start(Vec::new(), false), None);
    }
}
