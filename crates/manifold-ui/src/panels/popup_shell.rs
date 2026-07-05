//! Shared popup chrome — one dismiss scrim + one rounded, bordered container.
//!
//! Every popup (the lightweight [`dropdown`](super::dropdown), the modal
//! [`ableton_picker`](super::ableton_picker) / [`browser_popup`](super::browser_popup))
//! used to hand-roll its own shell: three different scrim dims, two border
//! techniques (a real 1px `border_width` vs a fake outer+inner panel pair), and
//! per-file bg/border literals. They now all call [`build`], so the popup look
//! lives in **one** place — the future visual upgrade (gradient body, heavier
//! shadow) changes this function, not three call sites.
//!
//! The §17 overlay loop ([`app_render`]) draws the soft drop-shadow under the
//! container automatically: it skips the leading full-screen scrim and shadows
//! the next node (the container). So the shell deliberately does *not* paint its
//! own shadow.
//!
//! Two surface kinds, [`PopupStyle::DROPDOWN`] (light, barely dims) and
//! [`PopupStyle::MODAL`] (darker well, dims the screen to focus it) — the only
//! difference is the palette; the structure is identical.

use crate::node::*;
use crate::tree::UITree;

/// The two node ids the shell mints. The caller stores [`backdrop`](Self::backdrop)
/// for click-outside dismissal and adds its content as later siblings (drawn on
/// top of [`container`](Self::container), since z-order follows build order).
pub struct PopupShell {
    pub backdrop: NodeId,
    pub container: NodeId,
}

/// Palette for a popup kind. Structure is shared; only these three colours vary.
pub struct PopupStyle {
    pub scrim: Color32,
    pub bg: Color32,
    pub border: Color32,
}

impl PopupStyle {
    /// Lightweight popup (option menu): a near-invisible scrim and a panel-tier
    /// fill — it floats over the UI without dimming it.
    pub const DROPDOWN: PopupStyle = PopupStyle {
        scrim: crate::color::DROPDOWN_SCRIM,
        bg: crate::color::DROPDOWN_BG,
        border: crate::color::DROPDOWN_BORDER,
    };

    /// Modal picker (Ableton / browser): a darker well behind a dimming scrim —
    /// it pulls focus off the rest of the screen.
    pub const MODAL: PopupStyle = PopupStyle {
        scrim: crate::color::MODAL_SCRIM,
        bg: crate::color::MODAL_BG,
        border: crate::color::MODAL_BORDER,
    };
}

/// Build the scrim + container. `screen` is the full screen size (the scrim
/// covers it to catch outside clicks); `rect` is the container's bounds. Both
/// nodes are parentless top-level overlay nodes, matching the popups' existing
/// build pattern.
pub fn build(tree: &mut UITree, screen: (f32, f32), rect: Rect, style: &PopupStyle) -> PopupShell {
    // Full-screen dismiss scrim — interactive so clicks outside the container
    // land here (and dismiss) instead of passing through to the panels behind.
    let backdrop = tree.add_button(
        None,
        0.0,
        0.0,
        screen.0,
        screen.1,
        UIStyle {
            bg_color: style.scrim,
            ..UIStyle::default()
        },
        "",
    );

    // One rounded, 1px-bordered container. The §17 overlay loop lifts it with a
    // soft shadow; this panel carries no shadow of its own.
    let container = tree.add_panel(
        None,
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        UIStyle {
            bg_color: style.bg,
            border_color: style.border,
            border_width: 1.0,
            corner_radius: crate::color::POPUP_RADIUS,
            ..UIStyle::default()
        },
    );

    PopupShell { backdrop, container }
}

/// Scale every node created since `first_node` about `center`, by `scale` —
/// the D17 "modal/dropdown enter" 0.98→1 pop for a popup whose content isn't
/// already parameterized by a single scaled rect the way `DropdownPanel`'s
/// `bounds` (or `ableton_picker`/`browser_popup`'s scaled `px`/`py`/`pw`/`ph`
/// locals) are. A geometric post-pass over the popup's own just-built node
/// range: correct regardless of how that layout code computed its absolute
/// positions, and cheap (one popup's node count, run only while its
/// `enter_anim` is still mid-flight). `SettingsPopup` is the one caller —
/// its rows are built from `ChromeHost`'s flex layout, not one resizable
/// rect. A no-op once `scale` settles at 1.0.
pub fn scale_nodes_about(tree: &mut UITree, first_node: usize, center: (f32, f32), scale: f32) {
    if (scale - 1.0).abs() < 0.0005 {
        return;
    }
    let (cx, cy) = center;
    for i in first_node..tree.count() {
        let id = tree.id_at(i);
        let Some(b) = tree.get_node(id).map(|n| n.bounds) else {
            continue;
        };
        let new_b = Rect::new(
            cx + (b.x - cx) * scale,
            cy + (b.y - cy) * scale,
            b.width * scale,
            b.height * scale,
        );
        tree.set_bounds(id, new_b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_mints_scrim_then_container() {
        let mut tree = UITree::new();
        let shell = build(
            &mut tree,
            (1920.0, 1080.0),
            Rect::new(100.0, 80.0, 300.0, 200.0),
            &PopupStyle::MODAL,
        );
        // Scrim is full-screen + interactive (catches outside clicks).
        let scrim = tree.get_node(shell.backdrop).unwrap();
        assert_eq!(scrim.node_type, UINodeType::Button);
        assert!(scrim.flags.contains(UIFlags::INTERACTIVE));
        assert_eq!(scrim.bounds.width, 1920.0);
        // Container carries the modal palette + the popup radius.
        let c = tree.get_node(shell.container).unwrap();
        assert_eq!(c.style.bg_color, crate::color::MODAL_BG);
        assert_eq!(c.style.border_color, crate::color::MODAL_BORDER);
        assert_eq!(c.style.corner_radius, crate::color::POPUP_RADIUS);
        // Built scrim-first so the §17 shadow skips it and lifts the container.
        assert!(shell.backdrop.index() < shell.container.index());
    }
}
