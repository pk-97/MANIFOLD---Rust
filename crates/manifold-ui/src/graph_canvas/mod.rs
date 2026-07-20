//! `GraphCanvas` — editable node-graph view hosted by the editor
//! window.
//!
//! The canvas is data-driven from `GraphSnapshot`s pushed by the
//! content thread (one per frame while the editor is open). When a new
//! topology lands, nodes are auto-laid-out by topological depth: source
//! nodes (no inputs) on the left, each downstream node placed to the
//! right of its deepest predecessor. Node positions persist across
//! parameter-only updates, so the layout doesn't twitch when only
//! `Mix.amount` changes.
//!
//! Future-proofing: when V2's editor lets users move nodes, snapshot
//! `NodeSnapshot.editor_pos` will switch from `None` to `Some`. The
//! canvas already prefers stored positions over auto-layout when present.
//!
//! Rendering goes through `UIRenderer` rect+text primitives — no UITree
//! / Panel infrastructure. Pan via middle-mouse drag, zoom via scroll
//! wheel, hover highlights. No editing yet.

use crate::color;
use crate::graph_view::{
    GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID, GraphSnapshot, GroupSnapshot,
    NodeSnapshot, PortKindSnapshot, PortSnapshot, WireSnapshot,
};
use crate::node::Color32;
// Re-exported so sibling submodules see both via `use super::*;`. The canvas
// emits `GraphEditCommand` (Phase 4.3); `PanelAction` remains for the mapping
// popover's `EffectMapping*` edits, which are a separate command family.
pub(crate) use crate::{GraphEditCommand, PanelAction};
use crate::transform::Axis;
// `MappingPopover` is brought into module scope by the `pub use` re-export below,
// so the `mapping_popover` field can name it without a second import.

/// Set `GROUP_CANVAS_LOG=1` in the environment to print the gesture pipeline
/// (scope enter/exit, group/ungroup emits, marquee commits) to stderr. Cheap
/// when off — one env read per gesture, never per frame. The handoff doc's
/// debug-friendly mandate: a failing interaction should leave a trail.
fn group_log_enabled() -> bool {
    std::env::var_os("GROUP_CANVAS_LOG").is_some()
}

macro_rules! group_log {
    ($($arg:tt)*) => {
        if group_log_enabled() {
            eprintln!("[group-canvas] {}", format!($($arg)*));
        }
    };
}

// Re-export the macro so sibling submodules can use it via `use super::*;`.
// (The unused-import lint can't see macro use through a glob re-export.)
#[allow(unused_imports)]
pub(crate) use group_log;

// ── Submodules (one concern each). `GraphCanvas` is one struct whose impl
// blocks are split across these siblings; the view-model types, layout
// engine, and free functions live in their concern file. ──
mod camera;
mod hit;
mod interaction;
mod layout;
pub mod mapping_popover;
mod model;
mod render;

#[cfg(test)]
mod tests;

// Re-exports so every submodule (via `use super::*;`) and external callers
// (`crate::graph_canvas::X`) keep resolving the moved types and free fns
// unchanged. The PUBLIC surface other files depend on — `Rect`,
// `GraphCanvas`, `resolve_level`, `resolve_card_param_node_id`,
// `node_preview_target` — is re-exported here.
// Only the names referenced cross-module (or externally as
// `crate::graph_canvas::X`) are re-exported. Module-internal helpers stay
// private to their file; test-only items (`LayeredLayout`, `ports_compatible`,
// `rects_overlap`) are imported directly by `tests.rs` from their module.
pub(crate) use hit::marquee_hits;
pub use hit::GraphCanvasTargets;
pub(crate) use interaction::CanvasDrag;
pub use mapping_popover::MappingPopover;
// App-facing structural-walk helpers — the editor present path resolves the
// canvas scope level + preview targets off the same UI snapshot the canvas reads.
pub use model::{node_preview_target, resolve_card_param_node_id, resolve_level};
pub(crate) use crate::draw::{elide_to_width, text_width};
pub(crate) use model::{
    NodeActionKind, NodeRow, NodeView, PortHit, WireView, expose_glyph_bounds, find_node_scope,
    fmt_table_cell, format_color_hex, group_wires_by_pair, kind_is_exposable,
    param_convert_for_kind, spark_has_variation, vec_channel_labels, wrap_text,
};

const HEADER_HEIGHT: f32 = 28.0;
/// Node body width in graph units. the unified
/// slider widget's fixed label + value-cell columns need real room to leave
/// the track a usable width without eliding every param name. Still compact
/// by design: a narrower node is also a *shorter* node,
/// since the always-on preview screen is sized to `NODE_WIDTH - 2·PREVIEW_PAD`
/// at the project aspect. On-node param rows and the title still truncate to
/// fit when a name genuinely doesn't (a long name + a "← wired"/"↳ outer"
/// suffix will always be able to outrun any reasonable width). (True per-node
/// content-sizing is a later refinement; this uniform width is the
/// high-leverage first step.)
const NODE_WIDTH: f32 = 270.0;
const NODE_HEADER_HEIGHT: f32 = 22.0;
/// Padding around the preview strip inside a node, so the thumbnail reads as a
/// recessed screen rather than a fill bleeding to the node edges.
const PREVIEW_PAD: f32 = 6.0;
/// Bounding box of a node's output-preview screen, inset by `PREVIEW_PAD` on
/// each side. Only nodes (and groups) that output an image carry the screen;
/// pure scalar nodes (param distributors, the generator input) don't reserve
/// the space. It sits in its own band between the header and the param/port
/// rows, so ports never overlap the thumbnail.
///
/// The screen takes the *project* aspect ratio (see [`preview_screen_size`]),
/// fit inside `PREVIEW_IMG_W` × `PREVIEW_MAX_H`. A landscape (16:9) show stays
/// full-width and short — its 162px height sits under the cap, so nothing about
/// the common case changes. A portrait show gets a taller, narrower screen
/// centered in the band rather than a tiny letterboxed sliver in a fixed 16:9
/// box.
const PREVIEW_IMG_W: f32 = NODE_WIDTH - 2.0 * PREVIEW_PAD;
/// Cap on the preview screen's height in graph units, so a portrait project
/// doesn't blow the node up to its full-width portrait height (288×512). Kept
/// above the 16:9 width-bound height (162) so landscape projects are unchanged.
const PREVIEW_MAX_H: f32 = 200.0;

/// Preview-screen size `(w, h)` in graph units for the given project aspect
/// ratio (width / height), aspect-fit inside `PREVIEW_IMG_W` × `PREVIEW_MAX_H`.
/// Landscape → width-bound (full width, short); portrait → height-bound
/// (capped height, narrower, centered by the caller).
pub(crate) fn preview_screen_size(aspect: f32) -> (f32, f32) {
    let aspect = if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        16.0 / 9.0
    };
    let width_bound_h = PREVIEW_IMG_W / aspect;
    if width_bound_h <= PREVIEW_MAX_H {
        (PREVIEW_IMG_W, width_bound_h)
    } else {
        (PREVIEW_MAX_H * aspect, PREVIEW_MAX_H)
    }
}
/// Vertical pitch of one on-node parameter row — from one row's top to the
/// next. Matches the card's real row *rhythm*, `param_slider_shared::ROW_HEIGHT`
/// (24) + `ROW_SPACING` (6) = 30, not just the bare row height: the card never
/// packs consecutive rows edge-to-edge, and neither should the node. The slider
/// widget itself draws shorter than the full pitch and is vertically centered
/// in it (see `PARAM_SLIDER_ROW_H`), so the gap is real whitespace, not just
/// unused row height. Nodes carry their params on their face so you read (and
/// tune) them where you are, instead of darting to a side panel.
const PARAM_ROW_H: f32 = 30.0;
/// Height (graph units) of the slider widget itself within a param row —
/// matches the card's `ROW_HEIGHT` (24) exactly. Centered vertically in the
/// taller `PARAM_ROW_H` pitch, leaving a real gap below before the next row.
const PARAM_SLIDER_ROW_H: f32 = 24.0;
/// Width (graph units) of a ranged param's value cell. Wider than the card's
/// shared `slider::VALUE_BOX_W` (56): the card only ever shows the friendly,
/// human-scaled value an effect/generator exposes (e.g. "2.00"), but a raw
/// on-node primitive param can be an unnormalized integer in the millions
/// ("2000000" — particle counts), which crowded 56's margins to nothing.
/// Node-only — cards keep drawing through `slider::VALUE_BOX_W` unchanged.
const PARAM_SLIDER_VALUE_BOX_W: f32 = 72.0;
/// Left padding (graph units) before a param row's content. The expose glyph
/// sits here; the label starts past it. Shared by render + hit so glyph draw and
/// click agree. Matches the value/label rows' `pad_x`.
const PARAM_PAD_X: f32 = 8.0;
/// Diameter (graph units) of the per-row expose glyph — the Blender-style dot at
/// a param's left edge that promotes it onto the outer performance card.
const PARAM_EXPOSE_D: f32 = 7.0;
/// Left inset (graph units) of a param row's label — past the expose glyph plus
/// a small gap, so the label never overlaps the dot.
const PARAM_LABEL_X: f32 = PARAM_PAD_X + PARAM_EXPOSE_D + 4.0;
/// Label-cell width (graph units) for a ranged param's slider widget — the
/// node-face analogue of `slider::DEFAULT_LABEL_WIDTH` (60). still elided common names like "Active Particle Count"
/// hard at 72, once `NODE_WIDTH` had the room to give it more.
const PARAM_SLIDER_LABEL_W: f32 = 84.0;
/// Pixels of horizontal drag that scrub a value across its full min..max
/// range when editing a param on the node face. Matches the inspector
/// sidebar's feel (`DRAG_FULL_RANGE_PX`).
const PARAM_SCRUB_FULL_RANGE_PX: f32 = 240.0;
/// Height (graph units) of the "Edit Code…" footer strip an expanded
/// `wgsl_compute` node carries below its param rows — click opens the multiline
/// kernel editor. Only present when [`NodeView::wgsl_source`] is `Some`.
const WGSL_FOOTER_H: f32 = 20.0;
const PORT_ROW_HEIGHT: f32 = 18.0;
const PORT_RADIUS: f32 = 4.0;
const PORT_COL_WIDTH: f32 = 10.0;
const NODE_CORNER: f32 = 6.0;

/// The one zoom range the canvas lives in — shared by scroll-wheel zoom AND
/// zoom-to-fit, so the fit can never park the view at a zoom the user then
/// can't reach (or scroll back to) by hand. The floor is deliberately low
/// enough to frame a tall generator graph (e.g. an ~30-node glTF import
/// column) on open; the ceiling is the manual magnify limit. Zoom-to-fit
/// additionally never magnifies past 1.0 — a sparse graph shouldn't balloon —
/// but every value it produces still sits inside `[MIN_ZOOM, MAX_ZOOM]`, so
/// nothing it does is unreachable.
pub(crate) const MIN_ZOOM: f32 = 0.05;
pub(crate) const MAX_ZOOM: f32 = 4.0;

// Auto-layout grid spacing: NODE_WIDTH + ~60px breathing room for the wires
// between columns, so nodes never touch horizontally. Derived (not a
// hardcoded literal) so a future NODE_WIDTH change can't silently collapse
// this gap again.
const COL_SPACING: f32 = NODE_WIDTH + 60.0;
const LAYOUT_ORIGIN: (f32, f32) = (60.0, 60.0);
/// Vertical gap between two stacked nodes (or routing lanes) within a
/// column. Node heights vary, so the layout spaces by `height + VGAP`
/// rather than a fixed centre-to-centre pitch.
const LAYOUT_VGAP: f32 = 26.0;
/// Height a virtual routing waypoint occupies in a column. A wire that
/// spans more than one column gets one of these per column it crosses so
/// the crossing-reduction pass can see it and route around it; small so
/// the lane it reserves is thin.
const LAYOUT_DUMMY_H: f32 = 6.0;
/// Up/down sweeps for crossing minimisation. Each sweep reorders every
/// column by the median position of its neighbours; the best-scoring
/// ordering across all sweeps is kept. A handful converges on graphs
/// this size.
const LAYOUT_ORDER_ITERS: usize = 8;
/// Forward/backward passes for vertical coordinate assignment. Each pass
/// pulls every node toward the average height of what it connects to,
/// then resolves overlaps; alternating direction straightens wires.
const LAYOUT_COORD_ITERS: usize = 12;

// ── Wire routing (draw-only) ──
/// One muted-violet colour for every feedback return path, so they read as a
/// family distinct from the blue data / orange control wires regardless of the
/// source port's kind.
///
/// Every geometry colour in the canvas is a plain sRGB [`Color32`], the app-wide
/// colour currency. The `Painter` adapter (`impl Painter for UIRenderer`) is the
/// single place that converts sRGB → linear light before the GPU write, so these
/// read as authored and no draw site can double-convert.
const RETURN_WIRE_COLOR: Color32 = Color32::new(158, 140, 199, 255); // sRGB
/// How far (graph px) above the higher endpoint's node-top a return path arcs,
/// so it clears the node band and reads as "going around".
const RETURN_ARC_CLEAR: f32 = 36.0;
/// Return paths are dashed: `RETURN_DASH` sampled segments drawn, then the same
/// count skipped, repeating — a feedback wire at a glance.
const RETURN_DASH: i32 = 3;
/// Endpoint-tag chip for a quiet (unfocused) return wire — a dark tint of
/// `RETURN_WIRE_COLOR` so it reads as the same feedback family without being
/// as loud as the full arc.
const RETURN_TAG_BG: Color32 = Color32::new(46, 40, 58, 235);
const RETURN_TAG_TEXT: [u8; 4] = [200, 188, 219, 255];
/// Stagger the incoming-wire landing handle by port depth only on nodes with at
/// least this many inputs, so a dense fan-in (e.g. a ~15-input tracking node)
/// splays into the input stack instead of overlapping. Small mixers (a/b,
/// numbered slots) keep their uniform handles.
const FANIN_STAGGER_MIN: usize = 6;

const BG_COLOR: Color32 = Color32::new(26, 26, 31, 255);
const HEADER_BG: Color32 = Color32::new(36, 36, 43, 255);
/// Faint grid-line colour. Toned down further once drawn
/// as lines rather than dots — a line carries a lot more ink than a 2px dot at
/// the same spacing, so the same alpha read as louder than intended.
const GRID_LINE: Color32 = Color32::new(255, 255, 255, 9);
/// Graph-unit increment node dragging snaps to (`DragMode::NodeMove`). Kept
/// fine — this is the precision floor, not the visual grid's spacing.
const GRID_SPACING: f32 = 32.0;
/// How many snap increments apart the *drawn* grid lines are
/// (`GRID_SPACING * GRID_LINE_EVERY`). Dragging still snaps at the finer
/// `GRID_SPACING`; the visible grid is deliberately sparser than that so it
/// reads as a light reference, not a line for every step.
const GRID_LINE_EVERY: f32 = 4.0;

/// Round a graph-space coordinate to the nearest `GRID_SPACING` line.
pub(crate) fn snap_to_grid(v: f32) -> f32 {
    (v / GRID_SPACING).round() * GRID_SPACING
}
const NODE_BG: Color32 = Color32::new(46, 46, 56, 255);
const NODE_BG_HOVER: Color32 = Color32::new(56, 56, 69, 255);
/// Recessed "screen" the preview thumbnail is blitted over (and the letterbox
/// colour for non-16:9 outputs). Near-black so an empty / loading strip reads
/// as an off monitor, not a hole in the node.
const PREVIEW_SCREEN_BG: Color32 = Color32::new(10, 10, 13, 255);
const PREVIEW_SCREEN_BORDER: Color32 = Color32::new(0, 0, 0, 128);
const NODE_HEADER_BG: Color32 = Color32::new(71, 76, 107, 255);
const NODE_BORDER: Color32 = Color32::new(0, 0, 0, 153);
const NODE_BORDER_SELECTED: Color32 = Color32::new(128, 199, 255, 255);
const PORT_TEXTURE2D_COLOR: Color32 = Color32::new(128, 199, 255, 255);
const PORT_TEXTURE3D_COLOR: Color32 = Color32::new(199, 128, 255, 255);
const PORT_SCALAR_COLOR: Color32 = Color32::new(255, 199, 102, 255);
const PORT_ARRAY_COLOR: Color32 = Color32::new(128, 255, 158, 255);
const PORT_CAMERA_COLOR: Color32 = Color32::new(255, 140, 140, 255);
const PORT_LIGHT_COLOR: Color32 = Color32::new(255, 242, 140, 255);
const PORT_MATERIAL_COLOR: Color32 = Color32::new(242, 166, 102, 255);
/// `PortType::Transform` (TRS wire, `node.transform_3d`). Hot pink/magenta —
/// hue ~326°, distinct from every other port colour (nearest neighbours are
/// Camera's salmon at ~0° and Texture3D's purple at ~273°, both >45° away).
const PORT_TRANSFORM_COLOR: Color32 = Color32::new(255, 128, 199, 255);
/// `PortType::Atmosphere` (scene fog/sky wire, `node.atmosphere`). Hazy
/// blue-grey — reads as "atmosphere", hue ~210°, clear of Transform's magenta
/// (~326°) and Camera's salmon (~0°).
const PORT_ATMOSPHERE_COLOR: Color32 = Color32::new(150, 185, 215, 255);
/// `PortType::Object` (one scene object's full bundle, `node.scene_object`).
/// Chartreuse — hue ~93°, the widest open gap on the wheel (between Light's
/// ~53° and Array's ~134°, ≥40° from each neighbour).
const PORT_OBJECT_COLOR: Color32 = Color32::new(178, 255, 115, 255);
/// Ghost-wire tint while dragging over a compatible / incompatible input port —
/// a live green/red "this will / won't connect" hint, so a mis-wire is caught
/// before the drop, not after. The actual connect still validates server-side.
const CONNECT_OK_COLOR: Color32 = Color32::new(107, 224, 133, 217);
const CONNECT_BAD_COLOR: Color32 = Color32::new(235, 97, 97, 217);
/// Group node tint. A group reads as a distinct, slightly heavier box than an
/// atom so a complex graph shows its structure at a glance — teal-leaning
/// header + a faint teal body wash, the colour we reserve for "container".
const GROUP_HEADER_BG: Color32 = Color32::new(46, 87, 102, 255);
/// Preset group accent colours the recolour gesture cycles through — muted so
/// they read as labels, not alerts, under stage lighting. The first entry is
/// the default teal (`GROUP_HEADER_BG`), so cycling from untinted lands on a
/// real colour immediately.
const GROUP_TINT_PALETTE: [Color32; 6] = [
    Color32::new(46, 87, 102, 255),  // teal (default)
    Color32::new(102, 61, 107, 255), // plum
    Color32::new(107, 76, 46, 255),  // amber
    Color32::new(56, 102, 66, 255),  // moss
    Color32::new(102, 56, 61, 255),  // rust
    Color32::new(61, 71, 112, 255),  // indigo
];
const GROUP_BODY_BG: Color32 = Color32::new(41, 56, 64, 255);
const GROUP_BODY_BG_HOVER: Color32 = Color32::new(51, 69, 76, 255);
/// Border on a group's bounding box and the "enter" chevron, brighter than a
/// plain node border so the affordance ("this opens") is legible.
const GROUP_ACCENT: Color32 = Color32::new(115, 209, 224, 255);
/// Breadcrumb bar text + the "› " separators, drawn in the canvas header when
/// the view is inside one or more groups.
const BREADCRUMB_TEXT: [u8; 4] = [180, 215, 220, 255];
const BREADCRUMB_DIM: [u8; 4] = [120, 130, 140, 255];
/// Translucent backdrop behind the debug overlay readout so it stays legible
/// over busy graph content.
const DEBUG_OVERLAY_BG: Color32 = Color32::new(0, 0, 0, 158);
const DEBUG_OVERLAY_TEXT: [u8; 4] = [120, 230, 160, 255];
/// Breadcrumb font size (logical px). The bitmap font is ~0.55em wide; the
/// segment layout uses that ratio so render and hit-test agree.
const BREADCRUMB_FONT: f32 = 12.0;
/// Rubber-band selection rectangle: a faint blue wash with a brighter border.
const MARQUEE_FILL: Color32 = Color32::new(128, 199, 255, 31);
const MARQUEE_BORDER: Color32 = Color32::new(128, 199, 255, 204);
/// Inline scrub bar in the floating Color/Vec channel editor popover
/// (`render_vec_editor`) — a faint translucent track + fill, distinct from the
/// node-row slider widget (which reads `Theme::slider_colors()` instead).
const PARAM_FILL_BG: Color32 = Color32::new(255, 255, 255, 18);
const PARAM_FILL_FG: Color32 = Color32::new(128, 199, 255, 140);
/// Expose glyph: a filled bright-cyan dot when the param is on the outer card,
/// a hollow dim outline when it's exposable but not yet exposed. The cyan is the
/// card accent (`NODE_BORDER_SELECTED`), so "exposed" reads as the same family as
/// the performance surface it feeds.
const PARAM_EXPOSE_ON: Color32 = Color32::new(128, 199, 255, 240);
const PARAM_EXPOSE_OFF: Color32 = Color32::new(150, 150, 165, 130);
/// Halo drawn behind a wire-driven param row's input-socket dot (D5) — the
/// same cyan family as `PARAM_EXPOSE_ON` (the card-routing accent), at a low
/// enough alpha to read as a tint around the socket rather than a second
/// solid dot, so the row's "something feeds this" jack is legible without
/// following the wire back through the graph.
const PARAM_DRIVEN_JACK: Color32 = PARAM_EXPOSE_ON.with_alpha(130);
// Enum dropdown (Phase 2 on-node editing): the selected row reads with an accent
// wash, the cursor row with a faint white lift, over the floating menu backing.
const ENUM_DD_CURRENT_BG: Color32 = Color32::new(128, 199, 255, 46);
const ENUM_DD_HOVER_BG: Color32 = Color32::new(255, 255, 255, 22);
/// "Edit Code…" footer button on a `wgsl_compute` node (Phase 4): a faint raised
/// fill, brighter on hover, so the kernel-editor affordance reads as a button.
const WGSL_FOOTER_BG: Color32 = Color32::new(255, 255, 255, 16);
const WGSL_FOOTER_HOVER_BG: Color32 = Color32::new(255, 255, 255, 32);
/// Header "reveal unused sockets" chip fill — a faint raised pill so the "+N" /
/// "−" toggle reads as a control against the coloured node header.
const REVEAL_CHIP_BG: Color32 = Color32::new(0, 0, 0, 70);
/// Sparkline trace colour — the same soft cyan as the fill bar, a touch brighter
/// so the moving line reads against the node body without shouting.
const SPARKLINE_COLOR: Color32 = Color32::new(140, 209, 255, 217);
/// Reads from the same `color::` tokens the inspector card uses — not a
/// bespoke canvas palette that happens to look similar and can drift from it.
const TEXT_PRIMARY: [u8; 4] = color::TEXT_PRIMARY_C32.to_array();
const TEXT_SECONDARY: [u8; 4] = color::TEXT_DIMMED_C32.to_array();
const TEXT_HEADER: [u8; 4] = [240, 240, 250, 255];
/// Hover-tooltip chrome: a near-opaque dark card with a faint border,
/// drawn above the nodes so the help line reads cleanly over any graph.
const TOOLTIP_BG: Color32 = Color32::new(26, 26, 33, 247);
const TOOLTIP_BORDER: Color32 = Color32::new(115, 122, 153, 217);
const TOOLTIP_TEXT: [u8; 4] = [224, 226, 236, 255];
/// Pink chip behind the "Reset to Default" header button —
/// same family as the MOD badge on the effect card so the
/// "you are diverged" cue is consistent across surfaces.
const RESET_BUTTON_BG: Color32 = Color32::new(199, 69, 115, 230);
const RESET_BUTTON_W: f32 = 124.0;
const RESET_BUTTON_H: f32 = 18.0;
/// Gap between the reset button and the zoom indicator on its right.
const RESET_BUTTON_RIGHT_GAP: f32 = 96.0;
/// Save to Library / Save to Project / Push to Library header pills
/// (PRESET_LIBRARY_DESIGN D4/D3, P3/P4) — same row as "Reset to Default",
/// anchored to its (always-reserved, whether or not Reset itself is
/// currently shown) slot so the pills don't shift position when the
/// modified state changes. Color lives in `color.rs` (the design-token
/// guard's token home), re-exported here under the name every call site in
/// this module already uses.
use color::GRAPH_SAVE_BUTTON_BG as SAVE_BUTTON_BG;
const SAVE_BUTTON_W: f32 = 108.0;
const SAVE_BUTTON_H: f32 = RESET_BUTTON_H;
/// Gap between adjacent header pill buttons.
const SAVE_BUTTON_GAP: f32 = 8.0;

/// Max seconds between two empty-canvas presses for them to count as a
/// double-click, and the max screen-space distance (px) between them.
/// Single-sourced at `color.rs` (UI_WIDGET_UNIFICATION P4/I8) — this module
/// no longer declares its own copy. The recognizer itself (keyed on node
/// identity via `last_click_node`, interaction.rs) stays canvas-side by
/// design; only the timing/radius constants unify.
use color::DOUBLE_CLICK_TIME_SEC as DOUBLE_CLICK_SECONDS;
use color::DOUBLE_CLICK_RADIUS_PX;
/// A left-press that moves less than this on release counts as a click, not a
/// drag — used to tell a pan from a deselecting click, and a marquee from a
/// stray shift-click.
const CLICK_MOVE_SLOP_PX: f32 = 4.0;

/// Points retained in each node's sparkline history. At the editor's ~UI frame
/// rate this is roughly a second of trace — enough to read a slow LFO without
/// hoarding memory across a big graph.
const SPARK_CAPACITY: usize = 48;

pub struct GraphCanvas {
    pub(crate) nodes: Vec<NodeView>,
    pub(crate) wires: Vec<WireView>,
    /// Per-node output-preview source, set by the render host each frame:
    /// `preview_node_id` → (texture handle registered on the renderer, source
    /// UV sub-rect `[u0, v0, u1, v1]`). `draw_node` paints this inline over the
    /// node's preview screen, at the node's own depth band, so a node stacked
    /// above occludes it. The
    /// handle points at the shared preview atlas (live app) or a node's output
    /// texture (headless harness); empty until the host populates it.
    pub(crate) node_preview_src:
        ahash::AHashMap<manifold_foundation::NodeId, (crate::node::TextureHandle, [f32; 4])>,
    /// Hash of the current topology (node ids+types + wire endpoints).
    /// Compared on each `set_snapshot` to skip layout recomputation
    /// when only parameter values changed.
    pub(crate) topology_hash: u64,
    pub(crate) pan: (f32, f32),
    pub(crate) zoom: f32,
    pub(crate) cursor: (f32, f32),
    /// The canvas's one drag lifecycle (P7.2, D8/D9). Replaces the old
    /// `drag_mode: DragMode` + `drag_anchor` + `drag_pan_start` fields — the
    /// grab position lives in the controller's session, not a parallel field.
    pub(crate) drag: crate::drag::DragController<CanvasDrag>,
    pub(crate) hovered: Option<u32>,
    /// Selected node ids at the current scope level. A set so the user can
    /// rubber-band or Shift-click several nodes before collapsing them into a
    /// group. A plain click selects exactly one; Shift toggles.
    pub(crate) selected: ahash::AHashSet<u32>,
    /// The wire feeding a wire-driven param row the user just clicked (D5) —
    /// identified by its `(to_node, to_port)` landing point rather than a
    /// `self.wires` index, since the wire list is rebuilt on every
    /// `set_snapshot` and an index would silently drift to a different wire.
    /// `draw_wire`/`draw_wire_ribbon` (via `wire_touches_focus`) draw the
    /// matching wire at full focus brightness, same as a hovered/selected
    /// endpoint. Cleared by `select_single`/`click_select` — any other click
    /// starts a new interaction, so the highlight shouldn't linger.
    pub(crate) highlighted_wire: Option<(u32, String)>,
    /// `instance.graph.is_some()` for the watched effect. Drives the
    /// "Reset to Default" affordance in the header — only shown when
    /// the user has diverged from the bundled preset.
    pub(crate) has_graph_mod: bool,
    /// Graph edits accumulated this frame from canvas interactions.
    /// Drained by the editor window's input loop after each event.
    pub(crate) pending_actions: Vec<GraphEditCommand>,
    /// Per-node collapse state (UI-only, keyed by runtime node id so it
    /// survives snapshot rebuilds like positions do). A collapsed node
    /// hides its on-face param rows but keeps its header and ports, so it
    /// can still be wired. Absent = expanded.
    pub(crate) collapsed: ahash::AHashMap<u32, bool>,
    /// Per-node "reveal unused sockets" state (UI-only, keyed by runtime node id
    /// so it survives snapshot rebuilds). By default an expanded node hides ports
    /// with no wire (a distributor like Generator Input shows only its *wired*
    /// outputs, not all nine), with a "+N" chip to reveal the rest so you can wire
    /// a currently-unwired one. `true` = show every port. Absent = hide unused.
    pub(crate) revealed_ports: ahash::AHashMap<u32, bool>,
    /// In-place mapping editor for a card binding, anchored on the param
    /// row it was right-clicked from. Surface-agnostic widget; the canvas
    /// just hosts it, draws it on top of the nodes, and forwards pointer
    /// events to it while it's open. Closed by default.
    pub(crate) mapping_popover: MappingPopover,
    /// Wall-clock seconds at the last left-press, used to detect a
    /// double-click — on empty space (opens the node picker) or on a group
    /// node (descends into it). `None` until the first press, and reset to
    /// `None` after a double-click fires so a third press starts a fresh
    /// single-click rather than re-triggering.
    pub(crate) last_click_time: Option<f32>,
    /// Screen-space cursor at the last left-press. Paired with
    /// `last_click_time` so a double-click only registers when the two
    /// presses land within a few pixels of each other.
    pub(crate) last_click_pos: (f32, f32),
    /// Node id under the last left-press (`None` for empty space). A
    /// double-click only counts when both presses land on the *same* target,
    /// so dragging between two groups doesn't accidentally enter one.
    pub(crate) last_click_node: Option<u32>,
    /// Current view scope — a path of group node ids from the document root
    /// to the level being shown. Empty = root. Pushed on enter-group, popped
    /// on exit. The canvas re-resolves which level to render from the live
    /// snapshot each frame using this path, so navigation is purely UI-local
    /// (no command, no content round-trip).
    pub(crate) scope: Vec<u32>,
    /// Display titles of the groups in `scope`, captured at enter time (the
    /// ancestor group nodes aren't in the current level's views, so their
    /// names have to be remembered). Always the same length as `scope`; the
    /// breadcrumb bar reads `["Root", scope_titles…]`.
    pub(crate) scope_titles: Vec<String>,
    /// When true, draw the debug overlay (scope path, selection, hover, drag
    /// mode) in the canvas corner. Toggled by the backtick key. The handoff
    /// doc's mandate: let the canvas tell Peter what it thinks is happening
    /// without a debugger.
    pub(crate) debug_overlay: bool,
    /// Set when the view descends into a group; consumed by the next
    /// `set_snapshot`, which auto-formats the level *only if it has never been
    /// laid out* (every node's `editor_pos` is `None`). Preserves any manual
    /// arrangement — once a layout exists (hand-moved or a prior auto-format),
    /// this never fires for that group again.
    pub(crate) format_on_enter: bool,
    /// Per-node recent history (normalized 0..1) of the node's primary numeric
    /// param, keyed by stable `NodeId`. Pushed each frame by
    /// [`Self::apply_live_values`] from the live tap and drawn as a small
    /// sparkline on the collapsed node face, so a modulated knob reads as a
    /// moving trace — the design's "even the invisible math nodes become
    /// legible." Bounded to [`SPARK_CAPACITY`] points per node; pruned to the
    /// live node set on a topology rebuild. Empty when no editor is watching.
    pub(crate) spark_history:
        ahash::AHashMap<manifold_foundation::NodeId, std::collections::VecDeque<f32>>,
    /// Runtime id of a node the canvas should centre + select once it is laid
    /// out at the current scope — set by [`Self::focus_node`] (jump-to-node from
    /// a card param) and consumed by [`Self::resolve_pending_focus`] one frame
    /// later, after `set_snapshot` has rebuilt the (possibly newly-entered)
    /// level so the node's position is known. `None` when nothing is pending.
    pub(crate) pending_focus: Option<u32>,
    /// Request to frame the whole level (zoom-to-fit) on the next viewport-aware
    /// present. Set when the canvas is created (editor open) and on every scope
    /// change (group enter/exit, breadcrumb jump), consumed by
    /// [`Self::apply_pending_fit`] once the level has finite-positioned nodes —
    /// same retry-next-frame contract as [`Self::pending_focus`]. Non-destructive:
    /// it only moves the camera, never node positions.
    pub(crate) fit_pending: bool,
    /// Lowercased find-a-node query. Non-empty dims nodes whose title/handle
    /// doesn't contain it and brightens the matches, so a name jumps out of a
    /// busy graph. Empty = no search active. Set live by the editor's search box.
    pub(crate) node_search: String,
    /// Project aspect ratio (output width / height) the per-node preview screens
    /// are sized to. Defaults to 16:9; the host refreshes it each frame from the
    /// live compositor dimensions via [`Self::set_preview_aspect`].
    pub(crate) preview_aspect: f32,
    /// Collapse state a node gets on first appearance (no entry in `collapsed`
    /// yet). Default `true` — a fresh graph reads cleanly, expand the one you're
    /// tuning. The headless snapshot flips this so a PNG can show the on-node
    /// param rows. Set before `set_snapshot` so the first layout uses the right
    /// node heights.
    pub(crate) default_collapsed: bool,
    /// Open enum-param dropdown, if any (Phase 2 on-node editing). Set by a
    /// click on an `Enum` param's value; any subsequent press resolves it (pick
    /// an option or dismiss). `None` when closed. Canvas-owned, drawn on top of
    /// the nodes and hit-tested first in `on_left_button_down`.
    pub(crate) enum_dropdown: Option<EnumDropdown>,
    /// Open Color / Vec channel editor, if any (Phase 3 on-node editing). Set by
    /// a click on a `Color` / `Vec2..4` param's value; its channel rows scrub in
    /// place (a press elsewhere inside swallows, a press outside dismisses).
    /// `None` when closed. Canvas-owned, drawn on top of the nodes and hit-tested
    /// (modally) in `on_left_button_down`, same pattern as `enum_dropdown`.
    pub(crate) vec_editor: Option<VecEditor>,
    /// Open `Table`-param grid editor, if any (Phase 4 on-node editing). Set by a
    /// click on a `Table` param's value; clicking a grid cell emits
    /// `EditGraphNodeTableCell` (the panel stays open so you can edit more cells).
    /// `None` when closed. Canvas-owned and modal in `on_left_button_down`, same
    /// pattern as `enum_dropdown` / `vec_editor`.
    pub(crate) table_editor: Option<TableEditor>,

    // ── P2 motion (`UI_CRAFT_AND_MOTION_PLAN.md` D17) ──────────────────
    // No per-frame tick existed anywhere in this module before these three —
    // `GraphCanvas::tick` (below) is the one new per-frame seam, called from
    // `app_render.rs` right next to the existing `canvas.set_snapshot(..)`
    // call that already runs every frame the editor window is open.
    /// Marquee fade in/out: eases visibility instead of an instant pop/
    /// disappear. Targets 1.0 while `drag_mode` is `Marquee`, 0.0 once
    /// released; retargeted unconditionally every `tick` (the
    /// `ChipMotion`/`drawer_height_anim` "call set_target every tick, it
    /// no-ops when already there" convention).
    pub(crate) marquee_alpha: crate::anim::AnimF32,
    /// The marquee's last live screen-space rect, refreshed every `tick`
    /// while the drag is live. `drag_mode` itself resets to `None` the
    /// instant `on_left_button_up` fires — this is what the render pass
    /// draws against while `marquee_alpha` eases back to 0 after release.
    pub(crate) marquee_last_rect: Option<(f32, f32, f32, f32)>,
    /// D17 "wire→port ... pop" (partial — see the doc comment on
    /// `GraphCanvas::tick` for what's NOT done here): a brief ring pop at
    /// the drop point on a successful `ConnectPorts` commit
    /// (`on_left_button_up`'s `WireFrom` arm).
    pub(crate) connect_pop: crate::anim::Transient,
    pub(crate) connect_pop_pos: (f32, f32),
    /// Error shake (D17) — a brief red flash + horizontal shake at the drop
    /// point when a `WireFrom` drag ends somewhere invalid (empty canvas, an
    /// output port, or the source node itself).
    pub(crate) error_shake: crate::anim::Transient,
    pub(crate) error_shake_pos: (f32, f32),

    /// D17 "wire→port magnetize" — the ghost wire's rendered endpoint while
    /// dragging a `WireFrom`. Eases (`Curve::Snap`, back-out) toward a
    /// nearby input port's exact position once `port_under` finds one
    /// within its hit radius; otherwise tracks the raw cursor with no lag
    /// (see `tick_wire_magnet`'s doc for why). `wire_magnet_live` gates
    /// `wire_ghost_endpoint`'s fallback to the raw cursor before the first
    /// tick of a fresh drag (the anim's stale value from the previous drag
    /// would otherwise flash for one frame).
    pub(crate) wire_magnet_x: crate::anim::AnimF32,
    pub(crate) wire_magnet_y: crate::anim::AnimF32,
    pub(crate) wire_magnet_live: bool,
    /// Wall-clock timestamp `tick_wire_magnet` last ran from — mirrors
    /// `DropdownPanel::last_tick` (this tick needs `viewport: Rect`, which
    /// `GraphCanvas::tick` doesn't have; it runs from `present_graph_editor_
    /// window` instead, right where the viewport rect is already computed).
    pub(crate) wire_magnet_last_tick: Option<std::time::Instant>,
    /// D17 "flow pulse" — one dash travels source→dest along a wire the
    /// instant it connects. `Some` only while active; the two endpoints are
    /// screen-space, captured at fire time (the two nodes don't move mid-
    /// pulse — MOTION_MED_MS).
    pub(crate) wire_flow_pulse: crate::anim::Transient,
    pub(crate) wire_flow_pulse_from: (f32, f32),
    pub(crate) wire_flow_pulse_to: (f32, f32),
}

impl GraphCanvas {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            node_preview_src: ahash::AHashMap::new(),
            wires: Vec::new(),
            topology_hash: 0,
            pan: (0.0, 0.0),
            zoom: 1.0,
            cursor: (0.0, 0.0),
            drag: crate::drag::DragController::new(),
            hovered: None,
            selected: ahash::AHashSet::new(),
            highlighted_wire: None,
            has_graph_mod: false,
            pending_actions: Vec::new(),
            collapsed: ahash::AHashMap::new(),
            revealed_ports: ahash::AHashMap::new(),
            mapping_popover: MappingPopover::new(),
            last_click_time: None,
            last_click_pos: (0.0, 0.0),
            last_click_node: None,
            scope: Vec::new(),
            scope_titles: Vec::new(),
            format_on_enter: false,
            spark_history: ahash::AHashMap::new(),
            pending_focus: None,
            fit_pending: true,
            node_search: String::new(),
            preview_aspect: 16.0 / 9.0,
            debug_overlay: false,
            // Nodes default expanded (params visible), Blender-style — there's no
            // +/- toggle to fold them.
            default_collapsed: false,
            enum_dropdown: None,
            vec_editor: None,
            table_editor: None,
            marquee_alpha: crate::anim::AnimF32::new(0.0, color::MOTION_FAST_MS),
            marquee_last_rect: None,
            connect_pop: crate::anim::Transient::default(),
            connect_pop_pos: (0.0, 0.0),
            error_shake: crate::anim::Transient::default(),
            error_shake_pos: (0.0, 0.0),
            wire_magnet_x: crate::anim::AnimF32::new(0.0, color::MOTION_MED_MS)
                .with_curve(crate::anim::Curve::Snap),
            wire_magnet_y: crate::anim::AnimF32::new(0.0, color::MOTION_MED_MS)
                .with_curve(crate::anim::Curve::Snap),
            wire_magnet_live: false,
            wire_magnet_last_tick: None,
            wire_flow_pulse: crate::anim::Transient::default(),
            wire_flow_pulse_from: (0.0, 0.0),
            wire_flow_pulse_to: (0.0, 0.0),
        }
    }

    /// Per-frame tween tick for the canvas's P2 motion pieces (marquee fade,
    /// connect pop, error shake, flow pulse — see each field's own doc
    /// comment). Call once per frame while the graph editor window is open;
    /// `app_render.rs` is the one call site, next to the existing
    /// `set_snapshot`/`apply_live_values` calls that already run every such
    /// frame. Returns `true` while anything is still animating, matching
    /// every other panel's `tick_*` contract (the caller can use it to force
    /// a redraw).
    ///
    /// Wire→port magnetize (D17) is deliberately NOT ticked here — it needs
    /// `viewport: Rect` to resolve a port's screen position (`port_under` +
    /// `to_screen`), which this call site doesn't have. See
    /// `tick_wire_magnet`, ticked separately from `present_graph_editor_
    /// window` where the viewport rect is already computed.
    pub fn tick(&mut self, dt_ms: f32) -> bool {
        let mut any = false;

        // Marquee fade. `origin_screen` is now the session start (D9).
        if let Some(session) = self.drag.session()
            && matches!(&session.payload, CanvasDrag::Marquee)
        {
            let (ox, oy) = (session.start.x, session.start.y);
            let (cx, cy) = self.cursor;
            self.marquee_last_rect = Some((ox.min(cx), oy.min(cy), (cx - ox).abs(), (cy - oy).abs()));
        }
        let marquee_live = matches!(self.drag.payload(), Some(CanvasDrag::Marquee));
        self.marquee_alpha.set_target(if marquee_live { 1.0 } else { 0.0 });
        any |= self.marquee_alpha.tick(dt_ms);

        any |= self.connect_pop.tick(dt_ms);
        any |= self.error_shake.tick(dt_ms);
        any |= self.wire_flow_pulse.tick(dt_ms);

        any
    }

    /// D17 "wire→port magnetize": advance the ghost wire's endpoint by real
    /// elapsed wall-clock time. While dragging a `WireFrom`, hit-tests the
    /// live cursor against input ports with the SAME `port_under` radius the
    /// drop itself uses (so the visual snap point and the functional connect
    /// threshold can never disagree) — within range, eases
    /// (`Curve::Snap`, back-out) toward that port's exact position;
    /// otherwise snaps straight to the raw cursor (no lag — a wire drag
    /// should track the pointer 1:1 except for the deliberate magnet pull).
    /// A no-op outside a `WireFrom` drag. Call once per frame from
    /// `present_graph_editor_window`, which already resolves `viewport` for
    /// the draw pass this feeds.
    pub fn tick_wire_magnet(&mut self, viewport: Rect) {
        let Some(CanvasDrag::WireFrom { .. }) = self.drag.payload() else {
            self.wire_magnet_live = false;
            self.wire_magnet_last_tick = None;
            return;
        };

        let (cx, cy) = self.cursor;
        let magnet_target = self.port_under(viewport, cx, cy).and_then(|hit| {
            if hit.is_output {
                return None; // magnetize toward an INPUT only — the valid drop target
            }
            let node = self.find_node(hit.node_id)?;
            let idx = node.inputs.iter().position(|p| p.name == hit.port_name)?;
            let (gx, gy) = node.input_port_pos_graph(idx);
            Some(self.to_screen(viewport, gx, gy))
        });

        let now = std::time::Instant::now();
        let dt_ms = self
            .wire_magnet_last_tick
            .map(|t| (now - t).as_secs_f32() * 1000.0)
            .unwrap_or(0.0)
            .min(100.0);
        self.wire_magnet_last_tick = Some(now);
        self.wire_magnet_live = true;

        match magnet_target {
            Some((tx, ty)) => {
                self.wire_magnet_x.set_target(tx);
                self.wire_magnet_y.set_target(ty);
                self.wire_magnet_x.tick(dt_ms);
                self.wire_magnet_y.tick(dt_ms);
            }
            None => {
                self.wire_magnet_x.snap(cx);
                self.wire_magnet_y.snap(cy);
            }
        }
    }

    /// The ghost wire's endpoint this frame (`draw_ghost_wire`'s use) —
    /// magnetized toward a nearby input port, or the raw cursor when none is
    /// in range, before `tick_wire_magnet`'s first call this drag (stale
    /// anim value from a previous drag), or outside a `WireFrom` drag
    /// entirely.
    pub(crate) fn wire_ghost_endpoint(&self) -> (f32, f32) {
        if self.wire_magnet_live && matches!(self.drag.payload(), Some(CanvasDrag::WireFrom { .. })) {
            (self.wire_magnet_x.value(), self.wire_magnet_y.value())
        } else {
            self.cursor
        }
    }

    /// Fire the D17 flow pulse: one dash travels `from` → `to` (screen
    /// space) — call right after a `WireFrom` drag commits a `ConnectPorts`
    /// action, alongside `fire_connect_pop`.
    pub(crate) fn fire_wire_flow_pulse(&mut self, from: (f32, f32), to: (f32, f32)) {
        self.wire_flow_pulse_from = from;
        self.wire_flow_pulse_to = to;
        self.wire_flow_pulse.fire(color::MOTION_MED_MS);
    }

    /// Fire the D17 connect-pop at `(sx, sy)` (screen space) — call right
    /// after a `WireFrom` drag commits a `ConnectPorts` action.
    pub(crate) fn fire_connect_pop(&mut self, sx: f32, sy: f32) {
        self.connect_pop_pos = (sx, sy);
        self.connect_pop.fire(color::MOTION_MED_MS);
    }

    /// Fire the D17 error shake at `(sx, sy)` (screen space) — call when a
    /// `WireFrom` drag ends somewhere invalid.
    pub(crate) fn fire_error_shake(&mut self, sx: f32, sy: f32) {
        self.error_shake_pos = (sx, sy);
        self.error_shake.fire(240.0);
    }

    /// Choose whether nodes appearing for the first time start expanded (all
    /// param rows visible) rather than collapsed. Call before `set_snapshot` so
    /// the first auto-layout uses the expanded node heights. Used by the headless
    /// snapshot to show the on-node param rows.
    pub fn set_default_expanded(&mut self, on: bool) {
        self.default_collapsed = !on;
    }

    /// Force one node's collapse state, overriding `default_collapsed` for
    /// just that id. No live gesture drives this yet (no on-canvas fold
    /// affordance — see `draw_node`'s "no collapse +/- toggle" comment); this
    /// exists for verification surfaces (the headless snapshot harness) that
    /// need to show a specific node collapsed regardless of the canvas-wide
    /// default, e.g. proving D6's "N params" chip
    /// (`docs/SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md` §2) on a group box next
    /// to an expanded sibling in the same capture.
    pub fn set_collapsed(&mut self, node_id: u32, collapsed: bool) {
        self.collapsed.insert(node_id, collapsed);
        if let Some(n) = self.nodes.iter_mut().find(|n| n.id == node_id) {
            n.collapsed = collapsed;
        }
    }

    /// Tell the canvas whether the watched effect is currently on its
    /// bundled-preset default (`false`) or carries a per-card graph
    /// override (`true`). When `true`, the header surfaces a
    /// "Reset to Default" button. Called once per frame by the editor
    /// window's present path.
    pub fn set_has_graph_mod(&mut self, has_mod: bool) {
        self.has_graph_mod = has_mod;
    }

    /// Set the find-a-node query (stored lowercased). Empty clears the search,
    /// restoring every node to full brightness.
    pub fn set_node_search(&mut self, query: &str) {
        self.node_search = query.to_ascii_lowercase();
    }

    /// The active find-a-node query (lowercased), or empty when no search is
    /// running. Lets the editor re-seed the field when reopening the search box.
    pub fn node_search(&self) -> &str {
        &self.node_search
    }
}

impl Default for GraphCanvas {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
}

/// An open enum-param dropdown on the node face. Clicking the value of an `Enum`
/// param row opens this list of the param's options, anchored directly under the
/// row; clicking an option emits `SetGraphNodeParam` with the chosen index and
/// closes it. pick from a list, never click-to-cycle.
/// Canvas-owned and hit-tested inside the canvas's own press handler — no
/// app-level input plumbing, same path as the row scrub + expose checkbox.
// Single-host by design (UI_WIDGET_UNIFICATION D17): the chrome "twin" this
// once shared a contract with (a sidebar enum picker) was deleted by
// GRAPH_EDITOR_REDESIGN Phase 6 — chrome cards never render enum dropdowns
// anymore (enum params render as labeled sliders there). Do not build a
// shared option-list abstraction between this and `dropdown.rs` — D17 names
// and rejects that as an adapter trap around a misfit.
#[derive(Debug, Clone)]
pub(crate) struct EnumDropdown {
    /// Runtime (doc) id of the node whose param this drives.
    pub(crate) node_id: u32,
    /// Inner param name — the `param_name` the emitted command carries.
    pub(crate) param_name: String,
    /// Option labels, in index order (index = the enum value set on pick).
    pub(crate) options: Vec<String>,
    /// Currently-selected index, highlighted in the list.
    pub(crate) current: usize,
    /// Screen-space rect of the param row it opened from. The list stacks
    /// directly below it, one row per option at the same height and width.
    pub(crate) anchor: Rect,
    /// `Some(outer_param_id)` when this dropdown was opened from a
    /// group-face mirror row (D6): a pick emits `SetOuterParam` (the card's
    /// own write path) instead of `SetGraphNodeParam`. `None` for an ordinary
    /// node-face enum row.
    pub(crate) outer_param_id: Option<String>,
}

impl EnumDropdown {
    /// Height of one option row — matches the param row it opened from.
    fn option_h(&self) -> f32 {
        self.anchor.h
    }

    /// Screen-space rect of the whole option list, below the anchor row.
    pub(crate) fn panel_rect(&self) -> Rect {
        Rect::new(
            self.anchor.x,
            self.anchor.y + self.anchor.h,
            self.anchor.w,
            self.option_h() * self.options.len() as f32,
        )
    }

    /// Screen-space rect of option `i` in the list.
    pub(crate) fn option_rect(&self, i: usize) -> Rect {
        let h = self.option_h();
        Rect::new(
            self.anchor.x,
            self.anchor.y + self.anchor.h + i as f32 * h,
            self.anchor.w,
            h,
        )
    }

    /// True when `(sx, sy)` is inside the open list.
    pub(crate) fn contains(&self, sx: f32, sy: f32) -> bool {
        let r = self.panel_rect();
        sx >= r.x && sx <= r.x + r.w && sy >= r.y && sy <= r.y + r.h
    }

    /// The option index under `(sx, sy)`, or `None` if the cursor isn't on the
    /// list (or the list is empty).
    pub(crate) fn option_at(&self, sx: f32, sy: f32) -> Option<usize> {
        if !self.contains(sx, sy) || self.options.is_empty() {
            return None;
        }
        let i = ((sy - (self.anchor.y + self.anchor.h)) / self.option_h()) as usize;
        (i < self.options.len()).then_some(i)
    }
}

/// An open Color / Vec channel editor on the node face (Phase 3). Clicking the
/// value of a `Color` / `Vec2..4` param row opens this panel directly under the
/// row: for a colour a swatch header, then one draggable channel row per
/// component (RGBA / XYZW). Dragging a channel row scrubs that component and
/// emits the WHOLE colour/vector as one `SetGraphNodeParam` (the other channels
/// held) — byte-for-byte the sidebar's channel scrub. Canvas-owned and modal in
/// `on_left_button_down`, same pattern as [`EnumDropdown`]; the live channel
/// values + swatch are read from the node's `ParamView` each frame, so an edit
/// round-tripping through the snapshot keeps the panel current.
// Single-host by design (UI_WIDGET_UNIFICATION D17) — same classification as
// `EnumDropdown`: chrome cards never render color/vec rows, so there is no
// twin to unify with.
#[derive(Debug, Clone)]
pub(crate) struct VecEditor {
    /// Runtime (doc) id of the node whose param this drives.
    pub(crate) node_id: u32,
    /// Inner param name — the `param_name` the emitted command carries.
    pub(crate) param_name: String,
    /// The param kind — picks the emitted `SerializedParamValue` variant and
    /// the channel labels.
    pub(crate) kind: crate::graph_view::ParamSnapshotKind,
    /// `true` for a `Color` (RGBA, 0..1 channels, swatch header), `false` for a
    /// plain vector (XYZW, ranged channels, no header).
    pub(crate) is_color: bool,
    /// Editable component count (2/3/4).
    pub(crate) components: usize,
    /// Screen-space rect of the param row it opened from. The panel stacks
    /// directly below it, one row per (header +) channel at the same height/width.
    pub(crate) anchor: Rect,
}

impl VecEditor {
    pub(crate) fn new(
        node_id: u32,
        param_name: String,
        kind: crate::graph_view::ParamSnapshotKind,
        anchor: Rect,
    ) -> Self {
        Self {
            node_id,
            param_name,
            kind,
            is_color: matches!(kind, crate::graph_view::ParamSnapshotKind::Color),
            components: model::vec_components(kind),
            anchor,
        }
    }

    /// Height of one panel row — matches the param row it opened from.
    fn row_h(&self) -> f32 {
        self.anchor.h
    }

    /// Non-channel header rows above the channel rows: the colour-swatch line for
    /// a `Color`, none for a plain vector.
    fn header_rows(&self) -> usize {
        self.is_color as usize
    }

    /// Screen-space rect of the whole panel, stacked below the anchor row.
    pub(crate) fn panel_rect(&self) -> Rect {
        let rows = self.header_rows() + self.components;
        Rect::new(
            self.anchor.x,
            self.anchor.y + self.anchor.h,
            self.anchor.w,
            self.row_h() * rows as f32,
        )
    }

    /// Screen-space rect of the colour-swatch header row, or `None` for a vector.
    pub(crate) fn swatch_rect(&self) -> Option<Rect> {
        self.is_color.then(|| {
            Rect::new(
                self.anchor.x,
                self.anchor.y + self.anchor.h,
                self.anchor.w,
                self.row_h(),
            )
        })
    }

    /// Screen-space rect of channel-row `ch` (0-based, past any header row).
    pub(crate) fn channel_rect(&self, ch: usize) -> Rect {
        let h = self.row_h();
        let top = self.anchor.y + self.anchor.h + (self.header_rows() + ch) as f32 * h;
        Rect::new(self.anchor.x, top, self.anchor.w, h)
    }

    /// True when `(sx, sy)` is anywhere inside the open panel.
    pub(crate) fn contains(&self, sx: f32, sy: f32) -> bool {
        let r = self.panel_rect();
        sx >= r.x && sx <= r.x + r.w && sy >= r.y && sy <= r.y + r.h
    }

    /// The channel index under `(sx, sy)`, or `None` if the cursor isn't on a
    /// channel row (header / outside the panel).
    pub(crate) fn channel_at(&self, sx: f32, sy: f32) -> Option<usize> {
        (0..self.components).find(|&ch| {
            let r = self.channel_rect(ch);
            sx >= r.x && sx <= r.x + r.w && sy >= r.y && sy <= r.y + r.h
        })
    }
}

/// An open `Table`-param grid editor on the node face (Phase 4). Clicking the
/// value of a `Table` param opens this panel under the row: a header line, then
/// the row-major grid of numeric cells. Clicking a cell emits
/// `EditGraphNodeTableCell` (the app opens its inline numeric editor over that
/// cell and, on commit, rebuilds the one changed cell into a whole `Table`
/// value) — byte-for-byte the sidebar's grid. Canvas-owned and modal in
/// `on_left_button_down`, same pattern as [`EnumDropdown`] / [`VecEditor`]; the
/// live cell values are read from the node's `ParamView` each frame, so an edit
/// round-tripping through the snapshot keeps the grid current. Dimensions
/// (`rows`/`cols`) are captured at open — a cell edit never reshapes the table,
/// and a structural reshape re-opens the editor.
// Single-host by design (UI_WIDGET_UNIFICATION D17) — same classification as
// `EnumDropdown`/`VecEditor`: chrome cards never render table rows, so there
// is no twin to unify with.
#[derive(Debug, Clone)]
pub(crate) struct TableEditor {
    /// Runtime (doc) id of the node whose param this drives.
    pub(crate) node_id: u32,
    /// Table param name — the `param_name` each emitted command carries.
    pub(crate) param_name: String,
    /// Grid dimensions captured at open (row count / max column count).
    pub(crate) rows: usize,
    pub(crate) cols: usize,
    /// Screen-space rect of the param row it opened from. The panel stacks
    /// directly below it: a header line, then the grid.
    pub(crate) anchor: Rect,
}

impl TableEditor {
    /// Height of one grid row / the header line — matches the param row it
    /// opened from, so the popover reads at the node's own row rhythm.
    fn row_h(&self) -> f32 {
        self.anchor.h
    }

    /// Width of one cell, dividing the panel width evenly across the columns
    /// (min 1 to avoid a divide-by-zero on a degenerate empty table).
    fn cell_w(&self) -> f32 {
        self.anchor.w / self.cols.max(1) as f32
    }

    /// Y of the grid's top edge — one header line below the anchor row.
    fn grid_top(&self) -> f32 {
        self.anchor.y + self.anchor.h + self.row_h()
    }

    /// Screen-space rect of the whole panel: header line + `rows` grid lines.
    pub(crate) fn panel_rect(&self) -> Rect {
        Rect::new(
            self.anchor.x,
            self.anchor.y + self.anchor.h,
            self.anchor.w,
            self.row_h() * (self.rows + 1) as f32,
        )
    }

    /// Screen-space rect of cell `(r, c)` in the grid.
    pub(crate) fn cell_rect(&self, r: usize, c: usize) -> Rect {
        let cw = self.cell_w();
        Rect::new(
            self.anchor.x + c as f32 * cw,
            self.grid_top() + r as f32 * self.row_h(),
            cw,
            self.row_h(),
        )
    }

    /// True when `(sx, sy)` is anywhere inside the open panel.
    pub(crate) fn contains(&self, sx: f32, sy: f32) -> bool {
        let p = self.panel_rect();
        sx >= p.x && sx <= p.x + p.w && sy >= p.y && sy <= p.y + p.h
    }

    /// The cell `(row, col)` under `(sx, sy)`, or `None` if the cursor isn't on a
    /// grid cell (header line / outside the panel).
    pub(crate) fn cell_at(&self, sx: f32, sy: f32) -> Option<(usize, usize)> {
        for r in 0..self.rows {
            for c in 0..self.cols {
                let rect = self.cell_rect(r, c);
                if sx >= rect.x && sx <= rect.x + rect.w && sy >= rect.y && sy <= rect.y + rect.h {
                    return Some((r, c));
                }
            }
        }
        None
    }
}
