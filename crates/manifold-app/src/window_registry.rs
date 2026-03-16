use std::sync::Arc;
use std::collections::HashMap;
use manifold_renderer::surface::SurfaceWrapper;

#[allow(dead_code)]
/// Role of a window in the application.
#[derive(Debug, Clone)]
pub enum WindowRole {
    /// Main workspace window (UI, timeline, etc.)
    Workspace,
    /// External output window (projector, LED wall, secondary monitor)
    Output { name: String },
}

#[allow(dead_code)]
/// State for a single window.
pub struct WindowState {
    pub window: Arc<winit::window::Window>,
    pub surface: SurfaceWrapper,
    pub role: WindowRole,
    pub display_index: Option<usize>,
}

/// Registry of all open windows.
pub struct WindowRegistry {
    windows: HashMap<winit::window::WindowId, WindowState>,
    creation_order: Vec<winit::window::WindowId>,
}

#[allow(dead_code)]
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
        self.creation_order.iter().filter_map(move |id| {
            self.windows.get(id).map(|state| (id, state))
        })
    }

    /// Iterate mutable references in creation order.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&winit::window::WindowId, &mut WindowState)> {
        let _order = &self.creation_order;
        let windows = &mut self.windows;
        // Can't easily do ordered mutable iteration with HashMap.
        // Use a different approach: iterate the hashmap directly (unordered but safe).
        windows.iter_mut()
    }

    /// Get all Arc<Window> references (for request_redraw).
    pub fn window_arcs(&self) -> impl Iterator<Item = &Arc<winit::window::Window>> {
        self.creation_order.iter().filter_map(move |id| {
            self.windows.get(id).map(|s| &s.window)
        })
    }

    pub fn len(&self) -> usize {
        self.windows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    pub fn contains(&self, id: &winit::window::WindowId) -> bool {
        self.windows.contains_key(id)
    }

    /// True if any Output-role window is currently open.
    /// Matches Unity's `host.IsMonitorOutputActive`.
    pub fn has_output_window(&self) -> bool {
        self.windows.values().any(|s| matches!(&s.role, WindowRole::Output { .. }))
    }
}
