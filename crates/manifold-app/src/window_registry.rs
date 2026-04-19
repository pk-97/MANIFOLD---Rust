use std::collections::HashMap;
use std::sync::Arc;

/// Role of a window in the application.
#[derive(Debug, Clone)]
pub enum WindowRole {
    /// Main workspace window (UI, timeline, etc.)
    Workspace,
    /// External output window (projector, LED wall, secondary monitor)
    Output { presentation: bool },
}

/// State for a single window.
pub struct WindowState {
    pub window: Arc<winit::window::Window>,
    /// `Some` for the workspace window and any output window without a dedicated
    /// presenter thread. `None` for output windows whose surface is owned by
    /// `OutputPresenterHandle` on a separate thread (macOS fullscreen path).
    pub surface: Option<manifold_gpu::GpuSurface>,
    pub role: WindowRole,
}

/// Registry of all open windows.
pub struct WindowRegistry {
    windows: HashMap<winit::window::WindowId, WindowState>,
    creation_order: Vec<winit::window::WindowId>,
}

impl WindowRegistry {
    pub fn new() -> Self {
        Self {
            windows: HashMap::new(),
            creation_order: Vec::new(),
        }
    }

    pub fn add(&mut self, id: winit::window::WindowId, state: WindowState) {
        self.creation_order.push(id);
        self.windows.insert(id, state);
    }

    pub fn remove(&mut self, id: &winit::window::WindowId) -> Option<WindowState> {
        self.creation_order.retain(|wid| wid != id);
        self.windows.remove(id)
    }

    pub fn get(&self, id: &winit::window::WindowId) -> Option<&WindowState> {
        self.windows.get(id)
    }

    pub fn get_mut(&mut self, id: &winit::window::WindowId) -> Option<&mut WindowState> {
        self.windows.get_mut(id)
    }

    /// Iterate windows in creation order.
    pub fn iter(&self) -> impl Iterator<Item = (&winit::window::WindowId, &WindowState)> {
        self.creation_order
            .iter()
            .filter_map(move |id| self.windows.get(id).map(|state| (id, state)))
    }

    /// Get all Arc<Window> references (for request_redraw).
    /// Non-macOS only — macOS wakes via CVDisplayLink per-window.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub fn window_arcs(&self) -> impl Iterator<Item = &Arc<winit::window::Window>> {
        self.creation_order
            .iter()
            .filter_map(move |id| self.windows.get(id).map(|s| &s.window))
    }

    /// True if any Output-role window is currently open.
    /// Matches Unity's `host.IsMonitorOutputActive`.
    pub fn has_output_window(&self) -> bool {
        self.windows
            .values()
            .any(|s| matches!(&s.role, WindowRole::Output { .. }))
    }
}
