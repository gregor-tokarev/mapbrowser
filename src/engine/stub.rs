//! No-op engine for platforms without the Chromium embedding. It creates no
//! page views and reports no events, so the GPUI shell renders every tab as
//! an unloaded page. Used for UI previews and tests.

use super::EngineEvent;
use anyhow::Result;
use async_channel::Receiver;
use std::{ffi::c_void, path::Path};

/// Methods mirror the macOS engine; `allow(dead_code)` marks those this
/// platform's shell never calls.
pub struct Engine;

impl Engine {
    pub fn initialize(_data_dir: &Path) -> Result<(Self, Receiver<EngineEvent>)> {
        let (_, events) = async_channel::unbounded();
        Ok((Self, events))
    }

    #[allow(dead_code)]
    pub fn attach(&mut self, _parent: *mut c_void) -> Result<()> {
        Ok(())
    }

    pub fn create_tab(&mut self, _tab_id: u64, _workspace_id: u64, _url: &str) -> Result<()> {
        Ok(())
    }

    pub fn register_pending(&mut self) {}
    pub fn activate_tab(&mut self, _tab_id: u64) {}
    pub fn set_bounds(&mut self, _x: f64, _y: f64, _width: f64, _height: f64) {}
    pub fn set_transition(&mut self, _from: u64, _to: u64, _progress: f32, _direction: i32) {}
    pub fn focus_tab(&self, _tab_id: u64) {}
    pub fn focus_shell(&self) {}
    pub fn navigate(&self, _tab_id: u64, _url: &str) {}
    pub fn back(&self, _tab_id: u64) {}
    pub fn forward(&self, _tab_id: u64) {}
    pub fn reload(&self, _tab_id: u64) {}
    #[allow(dead_code)]
    pub fn stop(&self, _tab_id: u64) {}
    #[allow(dead_code)]
    pub fn has_tab(&self, _tab_id: u64) -> bool {
        false
    }
    pub fn forget_closed(&mut self, _tab_id: u64) {}
    pub fn close_tab(&mut self, _tab_id: u64) {}

    pub fn shutdown(&mut self) -> Result<()> {
        Ok(())
    }
}
