//! Reports ⌃'s release, which ends a ⌃Tab walk through recent tabs. The
//! page usually has keyboard focus, and then GPUI never sees modifiers.

use super::EngineEvent;
use async_channel::Sender;
use block2::RcBlock;
use objc2::{rc::Retained, runtime::AnyObject};
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags};
use std::{cell::Cell, ptr::NonNull};

pub(super) fn install_control_monitor(events: Sender<EngineEvent>) -> Option<Retained<AnyObject>> {
    let held = Cell::new(false);
    let block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit passes a valid event that outlives this handler.
        let control = unsafe { event.as_ref() }
            .modifierFlags()
            .contains(NSEventModifierFlags::Control);
        if held.replace(control) && !control {
            let _ = events.try_send(EngineEvent::ControlReleased);
        }
        event.as_ptr()
    });
    // SAFETY: The handler always returns the event, so AppKit delivers it
    // unchanged; AppKit copies the block.
    unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::FlagsChanged, &block)
    }
}
