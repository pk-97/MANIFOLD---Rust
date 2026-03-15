//! UIRoot — owns the entire UI state for one window.
//!
//! Contains the UITree, UIInputSystem, ScreenLayout, all panels,
//! and the dropdown overlay. The app layer creates one UIRoot per
//! workspace window and forwards winit events through it.

use manifold_ui::*;
use manifold_ui::input::{Key, Modifiers, PointerAction};
use manifold_ui::node::Vec2;

/// What the currently-open dropdown is selecting for.
#[derive(Debug, Clone)]
pub enum DropdownContext {
    BlendMode(usize),
    MidiNote(usize),
    MidiChannel(usize),
    Resolution,
    AddEffect(InspectorTab),
    GenType(usize),
}

/// Owns all UI state for one window.
pub struct UIRoot {
    // Core
    pub tree: UITree,
    pub input: UIInputSystem,
    pub layout: ScreenLayout,

    // Panels
    pub transport: TransportPanel,
    pub header: HeaderPanel,
    pub footer: FooterPanel,
    pub layer_headers: LayerHeaderPanel,
    pub inspector: InspectorCompositePanel,
    pub viewport: TimelineViewportPanel,
    pub dropdown: DropdownPanel,

    // State
    built: bool,
    screen_width: f32,
    screen_height: f32,
    time_accumulator: f32,

    /// Context for the currently-open dropdown (set before open, read on selection).
    dropdown_context: Option<DropdownContext>,
}

impl UIRoot {
    pub fn new() -> Self {
        Self {
            tree: UITree::new(),
            input: UIInputSystem::new(),
            layout: ScreenLayout::new(1280.0, 720.0),
            transport: TransportPanel::new(),
            header: HeaderPanel::new(),
            footer: FooterPanel::new(),
            layer_headers: LayerHeaderPanel::new(),
            inspector: InspectorCompositePanel::new(),
            viewport: TimelineViewportPanel::new(),
            dropdown: DropdownPanel::new(),
            built: false,
            screen_width: 1280.0,
            screen_height: 720.0,
            time_accumulator: 0.0,
            dropdown_context: None,
        }
    }

    /// Build all panels. Call once after creation and after resize.
    pub fn build(&mut self) {
        self.tree.clear();
        self.layout = ScreenLayout::new(self.screen_width, self.screen_height);

        self.transport.build(&mut self.tree, &self.layout);
        self.header.build(&mut self.tree, &self.layout);
        self.footer.build(&mut self.tree, &self.layout);
        self.layer_headers.build(&mut self.tree, &self.layout);
        self.inspector.build(&mut self.tree, &self.layout);
        self.viewport.build(&mut self.tree, &self.layout);

        self.dropdown.set_screen_size(self.screen_width, self.screen_height);

        self.built = true;
    }

    /// Handle a resize event. Rebuilds all panels.
    pub fn resize(&mut self, width: f32, height: f32) {
        let same_size = (width - self.screen_width).abs() < 1.0
            && (height - self.screen_height).abs() < 1.0;
        if same_size && self.built {
            return;
        }
        self.screen_width = width;
        self.screen_height = height;
        self.build();
    }

    /// Process a pointer event from winit.
    pub fn pointer_event(&mut self, pos: Vec2, action: PointerAction, time: f32) {
        self.time_accumulator = time;
        self.input.process_pointer(&mut self.tree, pos, action, time);
    }

    /// Process a right-click from winit.
    pub fn right_click(&mut self, pos: Vec2) {
        self.input.process_right_click(&self.tree, pos);
    }

    /// Process a key event from winit.
    pub fn key_event(&mut self, key: Key, modifiers: Modifiers) {
        self.input.process_key(key, modifiers);
    }

    /// Open a dropdown with a context that determines what the selection means.
    pub fn open_dropdown(&mut self, context: DropdownContext, items: Vec<DropdownItem>, trigger: manifold_ui::node::Rect) {
        self.dropdown_context = Some(context);
        self.dropdown.open(items, trigger, 120.0, &mut self.tree);
    }

    /// Drain events from the input system and route to panels.
    /// Returns all panel actions for the app layer to dispatch.
    pub fn process_events(&mut self) -> Vec<PanelAction> {
        if !self.built {
            return Vec::new();
        }

        let events = self.input.drain_events();
        let mut actions = Vec::new();

        for event in &events {
            // Dropdown gets first crack at all events.
            if self.dropdown.is_open() {
                if let Some(dd_action) = self.dropdown.handle_event(event, &mut self.tree) {
                    match dd_action {
                        DropdownAction::Selected(index) => {
                            if let Some(ctx) = self.dropdown_context.take() {
                                if let Some(action) = Self::dropdown_to_action(ctx, index) {
                                    actions.push(action);
                                }
                            }
                        }
                        DropdownAction::Dismissed => {
                            self.dropdown_context = None;
                        }
                    }
                    continue; // Event consumed by dropdown.
                }
            }

            // Route to panels.
            let mut panel_actions;

            panel_actions = self.transport.handle_event(event, &self.tree);
            actions.append(&mut panel_actions);

            panel_actions = self.header.handle_event(event, &self.tree);
            actions.append(&mut panel_actions);

            panel_actions = self.footer.handle_event(event, &self.tree);
            actions.append(&mut panel_actions);

            panel_actions = self.layer_headers.handle_event(event, &self.tree);
            actions.append(&mut panel_actions);

            panel_actions = self.inspector.handle_event(event, &self.tree);
            actions.append(&mut panel_actions);

            panel_actions = self.viewport.handle_event(event, &self.tree);
            actions.append(&mut panel_actions);
        }

        actions
    }

    /// Convert a dropdown selection into the appropriate PanelAction based on context.
    fn dropdown_to_action(ctx: DropdownContext, index: usize) -> Option<PanelAction> {
        match ctx {
            DropdownContext::BlendMode(layer_idx) => {
                use manifold_core::types::BlendMode;
                let mode = BlendMode::from_index(index);
                Some(PanelAction::SetBlendMode(layer_idx, format!("{:?}", mode)))
            }
            DropdownContext::MidiNote(_layer_idx) => {
                Some(PanelAction::DropdownSelected(index))
            }
            DropdownContext::MidiChannel(_layer_idx) => {
                Some(PanelAction::DropdownSelected(index))
            }
            DropdownContext::Resolution => {
                Some(PanelAction::DropdownSelected(index))
            }
            DropdownContext::AddEffect(_tab) => {
                Some(PanelAction::DropdownSelected(index))
            }
            DropdownContext::GenType(_layer_idx) => {
                Some(PanelAction::DropdownSelected(index))
            }
        }
    }

    /// Per-frame update — push state changes to panels.
    pub fn update(&mut self) {
        if !self.built {
            return;
        }
        self.transport.update(&mut self.tree);
        self.header.update(&mut self.tree);
        self.footer.update(&mut self.tree);
        self.layer_headers.update(&mut self.tree);
        self.inspector.update(&mut self.tree);
        self.viewport.update(&mut self.tree);
    }
}
