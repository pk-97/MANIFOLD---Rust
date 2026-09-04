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
//! The section 17 overlay loop ([`app_render`]) draws the soft drop-shadow under the
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
/// for click-outside dismissal and parents its content to
/// [`container`](Self::container) — the container clips children (see [`build`]),
/// so content is structurally contained by the popup surface.
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
///
/// The container carries `CLIPS_CHILDREN` by construction — the same invariant
/// `UITree::begin_region` enforces for top-level regions, applied to the popup
/// surface itself. Content **must be parented to [`PopupShell::container`]**:
/// children can then never paint (GPU scissor) or hit-test
/// (`UITree::is_inside_clip_ancestors`) outside the container, on every
/// build/rebuild path, no matter which region wraps the popup. Content minted
/// as a sibling escapes the clip.
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

    // One rounded, 1px-bordered container. The section 17 overlay loop lifts it with a
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
    tree.set_flag(container, UIFlags::CLIPS_CHILDREN);

    PopupShell { backdrop, container }
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
        // Built scrim-first so the section 17 shadow skips it and lifts the container.
        assert!(shell.backdrop.index() < shell.container.index());
    }

    #[test]
    fn container_clips_children_by_construction() {
        // The containment invariant: anything parented to the container is
        // scissor-clipped on paint and rejected by hit_test outside it.
        let mut tree = UITree::new();
        let shell = build(
            &mut tree,
            (1920.0, 1080.0),
            Rect::new(100.0, 80.0, 300.0, 200.0),
            &PopupStyle::MODAL,
        );
        assert!(tree.has_flag(shell.container, UIFlags::CLIPS_CHILDREN));

        // A child minted partially outside the container paints clipped and
        // is not hittable at the outside point — the backdrop (full-screen,
        // minted earlier) is what a click there lands on.
        let child = tree.add_button(
            Some(shell.container),
            100.0,
            260.0,
            300.0,
            40.0,
            UIStyle::default(),
            "x",
        );
        assert_eq!(tree.hit_test(Vec2::new(150.0, 270.0)), Some(child));
        assert_eq!(
            tree.hit_test(Vec2::new(150.0, 290.0)),
            Some(shell.backdrop),
            "below the container the child is clipped away; the scrim takes the click"
        );
    }
}
