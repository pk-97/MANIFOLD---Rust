//! Modal progress surface for video export.
//!
//! The content thread owns export state. This panel only mirrors the latest
//! snapshot and emits a cancellation intent; it stays modal until content
//! confirms that the export has finished.

use crate::chrome::components;
use crate::color;
use crate::input::{Key, UIEvent};
use crate::node::{FontWeight, NodeId, Rect, TextAlign, UINodeType, UIStyle, Vec2};
use crate::tree::UITree;

use super::PanelAction;
use super::actions::ProjectAction;
use super::overlay::{Anchor, Modality, Overlay, OverlayPlacement, OverlayResponse, SizePolicy};
use super::popup_shell::{self, PopupShell};

const PANEL_W: f32 = 440.0;
const PANEL_H: f32 = 210.0;
const PAD: f32 = 20.0;
const TITLE_H: f32 = 24.0;
const FILENAME_H: f32 = 20.0;
const STATUS_H: f32 = 20.0;
const BAR_H: f32 = 12.0;
const CANCEL_H: f32 = 28.0;

const KEY_PROGRESS_TRACK: u64 = 74_004;
const KEY_PROGRESS_FILL: u64 = 74_005;
const KEY_CANCEL: u64 = 74_007;

/// Centered modal shown while the content thread renders and encodes a video.
pub struct ExportProgressPanel {
    open: bool,
    output_name: String,
    status: String,
    progress: f32,
    cancel_requested: bool,
    shell: Option<PopupShell>,
    cancel_id: NodeId,
}

impl Default for ExportProgressPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl ExportProgressPanel {
    pub fn new() -> Self {
        Self {
            open: false,
            output_name: String::new(),
            status: "Preparing export".into(),
            progress: 0.0,
            cancel_requested: false,
            shell: None,
            cancel_id: NodeId::PLACEHOLDER,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Start a fresh export. The app opens this before sending the export
    /// command to content, so the user sees the initial preparing state.
    pub fn begin(&mut self, output_name: &str) {
        self.open = true;
        self.output_name.clear();
        self.output_name.push_str(output_name);
        self.status.clear();
        self.status.push_str("Preparing export");
        self.progress = 0.0;
        self.cancel_requested = false;
    }

    /// Apply an export snapshot. Returns true when a visible value changed.
    /// Cancellation remains pending even when later snapshots carry another
    /// status string from the content thread.
    pub fn update(&mut self, status: &str, progress: f32) -> bool {
        let progress = sanitize_progress(progress);
        let progress_changed = self.progress != progress;
        self.progress = progress;
        if self.cancel_requested {
            return progress_changed;
        }
        if self.status != status {
            self.status.clear();
            self.status.push_str(status);
            self.open = true;
            return true;
        }
        if !self.open {
            self.open = true;
            return true;
        }
        progress_changed
    }

    /// Close after content confirms completion and clear the previous export.
    pub fn finish(&mut self) -> bool {
        let was_open = self.open;
        self.open = false;
        self.output_name.clear();
        self.status.clear();
        self.status.push_str("Preparing export");
        self.progress = 0.0;
        self.cancel_requested = false;
        was_open
    }

    /// Request cancellation once. A second click or Escape is swallowed.
    pub fn request_cancel(&mut self) -> bool {
        if self.cancel_requested || !self.open {
            return false;
        }
        self.cancel_requested = true;
        self.status.clear();
        self.status.push_str("Cancelling export");
        true
    }

    pub fn cancel_requested(&self) -> bool {
        self.cancel_requested
    }

    fn build_nodes(&mut self, tree: &mut UITree, placement: OverlayPlacement) {
        let rect = placement.rect;
        let shell = popup_shell::build(
            tree,
            (placement.screen.x, placement.screen.y),
            rect,
            &popup_shell::PopupStyle::MODAL,
        );

        let inner_x = rect.x + PAD;
        let inner_w = rect.width - PAD * 2.0;
        let title_y = rect.y + PAD;
        tree.add_label(
            Some(shell.container),
            inner_x,
            title_y,
            inner_w,
            TITLE_H,
            "Exporting video",
            title_style(),
        );
        let filename = crate::text::truncate_with_ellipsis(
            tree.measurer(),
            &self.output_name,
            color::FONT_LABEL,
            color::FONT_WEIGHT_DEFAULT,
            inner_w,
        );
        tree.add_label(
            Some(shell.container),
            inner_x,
            title_y + TITLE_H + 2.0,
            inner_w,
            FILENAME_H,
            &filename,
            filename_style(),
        );

        let status_y = title_y + TITLE_H + FILENAME_H + 20.0;
        let status = crate::text::truncate_with_ellipsis(
            tree.measurer(),
            &self.status,
            color::FONT_BODY,
            color::FONT_WEIGHT_DEFAULT,
            inner_w,
        );
        tree.add_label(
            Some(shell.container),
            inner_x,
            status_y,
            inner_w,
            STATUS_H,
            &status,
            status_style(),
        );

        let bar_y = status_y + STATUS_H + 10.0;
        let track = tree.add_node_keyed(
            Some(shell.container),
            Rect::new(inner_x, bar_y, inner_w, BAR_H),
            UINodeType::Panel,
            progress_track_style(),
            None,
            crate::node::UIFlags::empty(),
            KEY_PROGRESS_TRACK,
        );
        tree.add_node_keyed(
            Some(track),
            Rect::new(inner_x, bar_y, inner_w * self.progress, BAR_H),
            UINodeType::Panel,
            progress_fill_style(),
            None,
            crate::node::UIFlags::empty(),
            KEY_PROGRESS_FILL,
        );

        tree.add_label(
            Some(shell.container),
            inner_x,
            bar_y + BAR_H + 4.0,
            inner_w,
            18.0,
            percent_text(self.progress).as_str(),
            percent_style(),
        );

        let cancel_y = rect.y + rect.height - PAD - CANCEL_H;
        let cancel_text = if self.cancel_requested {
            "Cancelling export"
        } else {
            "Cancel export"
        };
        self.cancel_id = tree.add_button_keyed(
            Some(shell.container),
            inner_x,
            cancel_y,
            inner_w,
            CANCEL_H,
            cancel_style(self.cancel_requested),
            cancel_text,
            KEY_CANCEL,
        );
        if self.cancel_requested {
            tree.clear_flag(self.cancel_id, crate::node::UIFlags::INTERACTIVE);
        }
        self.shell = Some(shell);
    }
}

impl Overlay for ExportProgressPanel {
    fn is_open(&self) -> bool {
        self.open
    }

    fn modality(&self) -> Modality {
        // popup_shell owns the scrim; adding the overlay driver's scrim would
        // double-dim the window.
        Modality::Modal {
            dim_background: false,
        }
    }

    fn anchor(&self) -> Anchor {
        Anchor::Centered
    }

    fn size_policy(&self) -> SizePolicy {
        SizePolicy::Content
    }

    fn desired_size(&self) -> Vec2 {
        Vec2::new(PANEL_W, PANEL_H)
    }

    fn build_at(&mut self, tree: &mut UITree, placement: OverlayPlacement) {
        if self.open {
            self.build_nodes(tree, placement);
        }
    }

    fn on_event(&mut self, event: &UIEvent, _tree: &mut UITree) -> OverlayResponse {
        match event {
            UIEvent::KeyDown {
                key: Key::Escape, ..
            } => {
                let action = self
                    .request_cancel()
                    .then_some(PanelAction::Project(ProjectAction::CancelExport));
                OverlayResponse::Consumed(action.into_iter().collect())
            }
            UIEvent::Click { node_id, .. } if *node_id == self.cancel_id => {
                let action = self
                    .request_cancel()
                    .then_some(PanelAction::Project(ProjectAction::CancelExport));
                OverlayResponse::Consumed(action.into_iter().collect())
            }
            // A modal progress surface must remain visible until content says
            // the export is done, including when its scrim is clicked.
            UIEvent::Click { .. } => OverlayResponse::Consumed(Vec::new()),
            _ => OverlayResponse::Ignored,
        }
    }
}

fn sanitize_progress(progress: f32) -> f32 {
    if progress.is_finite() {
        progress.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn title_style() -> UIStyle {
    UIStyle {
        font_size: color::FONT_BODY,
        font_weight: FontWeight::Bold,
        text_color: color::TEXT_PRIMARY_C32,
        ..UIStyle::default()
    }
}

fn filename_style() -> UIStyle {
    UIStyle {
        font_size: color::FONT_LABEL,
        text_color: color::TEXT_DIMMED_C32,
        ..UIStyle::default()
    }
}

fn status_style() -> UIStyle {
    UIStyle {
        font_size: color::FONT_BODY,
        text_color: color::TEXT_PRIMARY_C32,
        ..UIStyle::default()
    }
}

fn percent_style() -> UIStyle {
    UIStyle {
        font_size: color::FONT_LABEL,
        text_color: color::TEXT_DIMMED_C32,
        text_align: TextAlign::Right,
        ..UIStyle::default()
    }
}

fn progress_track_style() -> UIStyle {
    UIStyle {
        bg_color: color::SLIDER_TRACK_PRESSED_C32,
        corner_radius: color::HAIRLINE_RADIUS,
        ..UIStyle::default()
    }
}

fn progress_fill_style() -> UIStyle {
    UIStyle {
        bg_color: color::HEADER_PROGRESS_FILL,
        corner_radius: color::HAIRLINE_RADIUS,
        ..UIStyle::default()
    }
}

fn cancel_style(cancelling: bool) -> UIStyle {
    let mut style = components::button_secondary_style();
    if cancelling {
        style.bg_color = color::BUTTON_INACTIVE;
        style.hover_bg_color = color::BUTTON_INACTIVE;
        style.pressed_bg_color = color::BUTTON_INACTIVE;
        style.text_color = color::TEXT_DIMMED_C32;
    }
    UIStyle {
        font_size: color::FONT_LABEL,
        ..style
    }
}

fn percent_text(progress: f32) -> String {
    format!("{:.0}%", progress * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel_and_tree() -> (ExportProgressPanel, UITree) {
        let mut panel = ExportProgressPanel::new();
        panel.begin("renders/final-video.mov");
        let mut tree = UITree::new();
        panel.build_at(
            &mut tree,
            OverlayPlacement {
                rect: Rect::new(280.0, 195.0, PANEL_W, PANEL_H),
                screen: Vec2::new(1000.0, 600.0),
            },
        );
        (panel, tree)
    }

    #[test]
    fn centered_contained_layout() {
        let (panel, tree) = panel_and_tree();
        assert_eq!(panel.anchor(), Anchor::Centered);
        assert_eq!(panel.desired_size(), Vec2::new(PANEL_W, PANEL_H));
        let shell = panel.shell.as_ref().unwrap();
        assert!(tree.has_flag(shell.container, crate::node::UIFlags::CLIPS_CHILDREN));
        let cancel = tree.get_node(panel.cancel_id).unwrap();
        assert_eq!(cancel.node_type, UINodeType::Button);
        assert!(cancel.bounds.x >= 280.0 && cancel.bounds.x_max() <= 720.0);
    }

    #[test]
    fn updates_clamp_and_dirty_check() {
        let mut panel = ExportProgressPanel::new();
        panel.begin("video.mov");
        assert!(panel.update("Rendering", 1.4));
        assert!(!panel.update("Rendering", 1.0));
        assert_eq!(panel.progress, 1.0);
        assert!(panel.update("Rendering", f32::NAN));
        assert_eq!(panel.progress, 0.0);
    }

    #[test]
    fn cancel_is_once_and_pending_updates_persist() {
        let mut panel = ExportProgressPanel::new();
        panel.begin("video.mov");
        assert!(panel.request_cancel());
        assert!(!panel.request_cancel());
        assert!(panel.cancel_requested());
        panel.update("Encoding", 0.5);
        assert_eq!(panel.status, "Cancelling export");
        assert_eq!(panel.progress, 0.5);
    }

    #[test]
    fn outside_click_cannot_dismiss_and_escape_cancels() {
        let (mut panel, mut tree) = panel_and_tree();
        let outside = UIEvent::Click {
            node_id: NodeId::PLACEHOLDER,
            pos: Vec2::new(1.0, 1.0),
            modifiers: Default::default(),
        };
        assert!(matches!(
            panel.on_event(&outside, &mut tree),
            OverlayResponse::Consumed(_)
        ));
        assert!(panel.is_open());
        let escape = UIEvent::KeyDown {
            node_id: NodeId::PLACEHOLDER,
            key: Key::Escape,
            modifiers: Default::default(),
        };
        let OverlayResponse::Consumed(actions) = panel.on_event(&escape, &mut tree) else {
            panic!("Escape not consumed")
        };
        assert_eq!(actions.len(), 1);
        assert!(panel.cancel_requested());
    }

    #[test]
    fn finish_resets_state() {
        let mut panel = ExportProgressPanel::new();
        panel.begin("video.mov");
        panel.request_cancel();
        assert!(panel.finish());
        assert!(!panel.is_open());
        assert!(!panel.cancel_requested());
        assert_eq!(panel.progress, 0.0);
        assert_eq!(panel.status, "Preparing export");
    }
}
