//! View model: snapshot ingestion, the on-canvas node/param/wire view
//! structs and their geometry, value formatting, and scope/snapshot
//! resolution. Pure data shaping — no rendering, no input.

use super::*;

#[derive(Debug, Clone)]
pub(crate) struct PortView {
    pub(crate) name: String,
    pub(crate) color: [f32; 4],
    /// True for scalar (control/value) ports. Wires out of these are the
    /// "set once" driver bindings that dominate the spaghetti, so they get
    /// dimmed unless their node is focused.
    pub(crate) is_control: bool,
}

impl PortView {
    // Takes `&PortKindSnapshot` because the snapshot's `Array`
    // variant now carries owned channel metadata (post-Phase-6); a
    // by-value signature would force every caller to clone the
    // channels Vec just to read the tag.
    pub(crate) fn from_kind(name: String, kind: &PortKindSnapshot) -> Self {
        let color = match kind {
            PortKindSnapshot::Texture2D => PORT_TEXTURE2D_COLOR,
            // Typed Texture2D shares the texture-port colour — the
            // four-slot channel signature is a tooltip-level detail,
            // not a separate port category. See
            // `docs/CHANNEL_TYPE_SYSTEM.md` §17.
            PortKindSnapshot::Texture2DTyped { .. } => PORT_TEXTURE2D_COLOR,
            PortKindSnapshot::Texture3D => PORT_TEXTURE3D_COLOR,
            PortKindSnapshot::Scalar => PORT_SCALAR_COLOR,
            PortKindSnapshot::Array { .. } => PORT_ARRAY_COLOR,
            PortKindSnapshot::Camera => PORT_CAMERA_COLOR,
            PortKindSnapshot::Light => PORT_LIGHT_COLOR,
            PortKindSnapshot::Material => PORT_MATERIAL_COLOR,
        };
        let is_control = matches!(kind, PortKindSnapshot::Scalar);
        Self {
            name,
            color,
            is_control,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct NodeView {
    pub(crate) id: u32,
    /// Stable [`manifold_foundation::NodeId`] of this node — the addressing identity
    /// that survives grouping. Empty for anonymous boundary nodes. Used to match
    /// the per-frame live param tap (`ContentState::live_node_params`, keyed by
    /// node_id) onto this view so on-face values reflect live modulation.
    pub(crate) node_id: manifold_foundation::NodeId,
    /// Stable string handle from the def, if any (`None` for boundary /
    /// anonymous nodes). Used to mint a collision-free handle when this
    /// node's level gets a new group, and by Ctrl+G's payload.
    pub(crate) handle: Option<String>,
    pub(crate) title: String,
    /// The node's parameters, drawn as compact rows on the node face when
    /// the node is expanded, so you can read and tune each one in place.
    /// Empty if the node has no params.
    pub(crate) params: Vec<ParamView>,
    /// One-line summary of the node's key param (e.g. "Mode: FoldX"), shown
    /// when the node is collapsed so a folded node still tells you its most
    /// important value at a glance. `None` if the node has no params.
    pub(crate) summary: Option<String>,
    /// Whether this node is collapsed (header + one summary line) rather than
    /// expanded (every param row). Nodes default to collapsed so a complex
    /// graph reads cleanly; expand the one you're tuning. Mirrors
    /// `GraphCanvas::collapsed` for this node so layout/drawing skip the map.
    pub(crate) collapsed: bool,
    /// Header tint for this node's `Category` (Color & Tone, Noise, Distort,
    /// ...), so the graph reads by family at a glance. `NODE_HEADER_BG` for
    /// nodes with no descriptor / `Uncategorized`.
    pub(crate) header_color: [f32; 4],
    /// Top-left corner in graph-space (logical pixels, pre pan/zoom).
    pub(crate) pos_graph: (f32, f32),
    pub(crate) inputs: Vec<PortView>,
    pub(crate) outputs: Vec<PortView>,
    /// Mirrors `NodeSnapshot::breaks_dependency_cycle`. Wires terminating
    /// here close a feedback loop; `auto_layout` skips them so depth
    /// propagation doesn't accumulate around the loop.
    pub(crate) breaks_dependency_cycle: bool,
    /// True when this node is a group (subgraph) instance — `type_id ==
    /// GROUP_TYPE_ID`. Drives the distinct group rendering and the
    /// double-click-to-enter gesture. Its `inputs`/`outputs` are the group's
    /// interface ports; the body lives in the snapshot and is re-resolved by
    /// scope, not stored on the view.
    pub(crate) is_group: bool,
    /// Group accent colour (`GroupDef::tint`), painted on the group header in
    /// place of the default group tint. `None` for ordinary nodes and untinted
    /// groups. Cycled by the recolour gesture on a selected group.
    pub(crate) group_tint: Option<[f32; 4]>,
    /// Friendly one-line summary from the node's `NodeDescriptor`, shown
    /// as a hover tooltip over the node's header/body. `None` for groups
    /// (no descriptor) and for any node whose author left the summary
    /// blank. Resolved once on the topology rebuild — it never changes.
    pub(crate) tooltip: Option<String>,
    /// Stable [`NodeId`] whose captured output texture this node shows in its
    /// preview strip, or `None` for nodes that output no image (param
    /// distributors, the generator input). For an ordinary image node this is
    /// its own `node_id`; for a **group** it's the inner node producing the
    /// group's primary output (resolved once at build time), so a collapsed
    /// group still previews what it emits without any extra capture — that
    /// inner node is already a cell in the flattened atlas. The presence of a
    /// value is what gives the node a preview band ([`Self::preview_h`]).
    pub(crate) preview_node_id: Option<manifold_foundation::NodeId>,
}

/// Whether a port carries an image (the only kind the thumbnail atlas captures).
pub(crate) fn port_kind_is_image(kind: &PortKindSnapshot) -> bool {
    matches!(
        kind,
        PortKindSnapshot::Texture2D
            | PortKindSnapshot::Texture2DTyped { .. }
            | PortKindSnapshot::Texture3D
    )
}

/// The stable [`NodeId`] whose captured texture a node previews, or `None` for a
/// node that emits no image. Ordinary node → its own id (if it has an image
/// output); group → the inner producer of its primary output. See
/// [`NodeView::preview_node_id`]. `pub(crate)` so the host can build the
/// thumbnail-atlas visible set (the nodes the canvas asks thumbnails for) from
/// the current scope's nodes.
pub fn node_preview_target(n: &NodeSnapshot) -> Option<manifold_foundation::NodeId> {
    if let Some(body) = n.group.as_deref() {
        let port = group_primary_output_port(&n.outputs)?;
        group_output_producer(body, port)
    } else if !n.node_id.as_str().is_empty()
        && n.outputs.iter().any(|p| port_kind_is_image(&p.kind))
    {
        Some(n.node_id.clone())
    } else {
        None
    }
}

/// A group's primary image output port name — the first output that carries a
/// texture. Unlike `manifold_core::flatten::primary_output_port` (which falls
/// back to the first output of any kind), this is image-only: a group with no
/// image output gets no preview band, exactly like an ordinary scalar node.
pub(crate) fn group_primary_output_port(outputs: &[PortSnapshot]) -> Option<&str> {
    outputs
        .iter()
        .find(|p| port_kind_is_image(&p.kind))
        .map(|p| p.name.as_str())
}

/// The stable [`NodeId`] of the concrete inner node producing `port` of a group
/// `body`, resolving through nested sub-groups. Snapshot-side mirror of
/// `manifold_core::flatten::producer_for_output`. `None` if the port is fed by
/// the group's own input (an unsupported passthrough the flattener also rejects)
/// or has no producer.
pub(crate) fn group_output_producer(body: &GroupSnapshot, port: &str) -> Option<manifold_foundation::NodeId> {
    let out_boundary = body
        .nodes
        .iter()
        .find(|n| n.type_id == GROUP_OUTPUT_TYPE_ID)?;
    let wire = body
        .wires
        .iter()
        .find(|w| w.to_node == out_boundary.id && w.to_port == port)?;
    let producer = body.nodes.iter().find(|n| n.id == wire.from_node)?;
    if let Some(inner) = producer.group.as_deref() {
        group_output_producer(inner, &wire.from_port)
    } else if producer.type_id == GROUP_INPUT_TYPE_ID {
        None
    } else {
        Some(producer.node_id.clone())
    }
}

impl NodeView {
    pub(crate) fn height(&self) -> f32 {
        let port_rows = self.inputs.len().max(self.outputs.len()) as f32;
        NODE_HEADER_HEIGHT + self.preview_h() + self.body_h() + port_rows * PORT_ROW_HEIGHT + 6.0
    }

    /// Height of the output-preview band below the header: the 16:9 strip plus
    /// its padding, or `0` for a node that emits no image. Zoom-independent.
    pub(crate) fn preview_h(&self) -> f32 {
        if self.preview_node_id.is_some() {
            PREVIEW_BAND_H
        } else {
            0.0
        }
    }

    /// Height of the body block below the header: collapsed shows the single
    /// summary line (if any), expanded shows every param row. Zoom-independent
    /// so port positions stay put as you zoom (the LOD cull is draw-only).
    pub(crate) fn body_h(&self) -> f32 {
        if self.collapsed {
            if self.summary.is_some() {
                PARAM_ROW_H
            } else {
                0.0
            }
        } else {
            self.params.len() as f32 * PARAM_ROW_H
        }
    }

    /// Y offset where port rows start, below the header, preview band, and body
    /// block. Ports live in their own band beneath the preview, so the strip and
    /// the port dots/labels never overlap.
    pub(crate) fn ports_y_offset(&self) -> f32 {
        NODE_HEADER_HEIGHT + self.preview_h() + self.body_h()
    }

    pub(crate) fn input_port_pos_graph(&self, idx: usize) -> (f32, f32) {
        let (x, y) = self.pos_graph;
        (
            x,
            y + self.ports_y_offset() + idx as f32 * PORT_ROW_HEIGHT + PORT_ROW_HEIGHT * 0.5,
        )
    }

    pub(crate) fn output_port_pos_graph(&self, idx: usize) -> (f32, f32) {
        let (x, y) = self.pos_graph;
        (
            x + NODE_WIDTH,
            y + self.ports_y_offset() + idx as f32 * PORT_ROW_HEIGHT + PORT_ROW_HEIGHT * 0.5,
        )
    }

    /// Y-offset (from the node's top edge) of the named input port's centre.
    /// Used by auto-layout to align a node so this wire's two ports line up,
    /// rather than aligning box-centre to box-centre. Falls back to the node
    /// mid-height for an unknown name (shouldn't happen for a live wire).
    pub(crate) fn input_port_offset(&self, name: &str) -> f32 {
        match self.inputs.iter().position(|p| p.name == name) {
            Some(idx) => {
                self.ports_y_offset() + idx as f32 * PORT_ROW_HEIGHT + PORT_ROW_HEIGHT * 0.5
            }
            None => self.height() * 0.5,
        }
    }

    /// Y-offset (from the node's top edge) of the named output port's centre.
    /// Companion to [`input_port_offset`](Self::input_port_offset).
    pub(crate) fn output_port_offset(&self, name: &str) -> f32 {
        match self.outputs.iter().position(|p| p.name == name) {
            Some(idx) => {
                self.ports_y_offset() + idx as f32 * PORT_ROW_HEIGHT + PORT_ROW_HEIGHT * 0.5
            }
            None => self.height() * 0.5,
        }
    }
}

/// One parameter as shown on the node face: its label, the formatted
/// current value, and an optional 0..1 fill fraction for ranged values
/// (drives the thin bar under the row). Owned so it survives the
/// content/UI snapshot boundary.
#[derive(Debug, Clone)]
pub(crate) struct ParamView {
    /// Inner-param name, used as `param_name` when a scrub emits
    /// `SetGraphNodeParam`.
    pub(crate) name: String,
    pub(crate) label: String,
    /// Snapshot kind + declared range, retained so a per-frame live value
    /// ([`GraphCanvas::apply_live_values`]) can reformat the value string and
    /// fill bar exactly as the structural snapshot did, without re-snapshotting.
    pub(crate) kind: crate::graph_view::ParamSnapshotKind,
    pub(crate) range: Option<(f32, f32)>,
    pub(crate) value: String,
    /// `Some(0..1)` position of the current value within its declared
    /// range, for the fill bar. `None` for params with no numeric range
    /// (enums, bools, triggers, or floats whose ParamDef declared none).
    pub(crate) fill: Option<f32>,
    /// Scrub metadata for in-place editing. `Some` only for numeric params
    /// (Float/Angle/Frequency/Int) that declared a range — those can be
    /// dragged on the node face. `None` params stay read-only on the canvas
    /// (still editable via the inspector sidebar).
    pub(crate) scrub: Option<ScrubInfo>,
    /// Plain-English help line for this param, from the `param_doc`
    /// side-channel keyed by `(node type_id, param name)`. Shown as a
    /// hover tooltip over the param row. `None` if the node author didn't
    /// register one. Static per `(type_id, name)`, so it's resolved once
    /// on the topology rebuild and carried forward on value-only refreshes.
    pub(crate) tooltip: Option<String>,
}

/// What a draggable on-node param needs to turn a horizontal drag into a
/// new value: its range, the value at press time, and whether to round.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScrubInfo {
    pub(crate) range: (f32, f32),
    pub(crate) current_value: f32,
    pub(crate) is_int: bool,
}

/// Format one parameter snapshot for on-node display: a short value string
/// plus, when the param has a numeric range, the 0..1 position of the
/// current value within it. Value formatting mirrors the inspector
/// (degrees for angles, Hz for frequencies, enum labels, On/Off).
pub(crate) fn format_param_for_node(p: &crate::graph_view::ParamSnapshot) -> ParamView {
    use crate::graph_view::ParamSnapshotKind;
    // Numeric / bool kinds share their formatting + fill with the per-frame
    // live-value path; enum / trigger / other are display-only here.
    let (value, fill) = match numeric_value_fill(p.kind, p.current_value, p.range) {
        Some(vf) => vf,
        None => {
            let v = match p.kind {
                ParamSnapshotKind::Enum => p
                    .enum_labels
                    .as_ref()
                    .and_then(|labels| labels.get(p.current_value as usize).cloned())
                    .unwrap_or_else(|| format!("{}", p.current_value as i64)),
                ParamSnapshotKind::Trigger => format!("{}", p.current_value as i64),
                // Colour reads as a hex string on the face; the swatch + channel
                // editor lives in the inspector sidebar.
                ParamSnapshotKind::Color => p
                    .vec_value
                    .map(format_color_hex)
                    .unwrap_or_else(|| "—".to_string()),
                ParamSnapshotKind::Vec2 => p
                    .vec_value
                    .map(|v| format!("{:.2}, {:.2}", v[0], v[1]))
                    .unwrap_or_else(|| "—".to_string()),
                ParamSnapshotKind::Vec3 => p
                    .vec_value
                    .map(|v| format!("{:.2}, {:.2}, {:.2}", v[0], v[1], v[2]))
                    .unwrap_or_else(|| "—".to_string()),
                ParamSnapshotKind::Vec4 => p
                    .vec_value
                    .map(|v| format!("{:.2}, {:.2}, {:.2}, {:.2}", v[0], v[1], v[2], v[3]))
                    .unwrap_or_else(|| "—".to_string()),
                // String + Table both read out of `summary` (the string value /
                // the table dimensions).
                ParamSnapshotKind::String | ParamSnapshotKind::Other => {
                    p.summary.clone().unwrap_or_else(|| "—".to_string())
                }
                // Numeric/bool kinds are handled by numeric_value_fill above.
                _ => String::new(),
            };
            (v, None)
        }
    };
    let scrub = scrub_for(p.kind, p.current_value, p.range);
    ParamView {
        name: p.name.clone(),
        label: p.label.clone(),
        kind: p.kind,
        range: p.range,
        value,
        fill,
        scrub,
        // Baked into the snapshot at translation time (the renderer's
        // `tooltip_for`), so the formatter carries it straight through.
        tooltip: p.tooltip.clone(),
    }
}

/// Format the value string + fill fraction for the param kinds the on-node face
/// can render and the live tap can drive: continuous numerics (Float / Angle /
/// Frequency / Int) and bools. `None` for kinds whose display needs more than a
/// scalar (enum labels, trigger, multi-component "Other"). The single source of
/// truth for both the structural snapshot ([`format_param_for_node`]) and the
/// per-frame live refresh ([`GraphCanvas::apply_live_values`]), so a frozen and
/// a modulated value format identically.
pub(crate) fn numeric_value_fill(
    kind: crate::graph_view::ParamSnapshotKind,
    value: f32,
    range: Option<(f32, f32)>,
) -> Option<(String, Option<f32>)> {
    use crate::graph_view::ParamSnapshotKind;
    let s = match kind {
        ParamSnapshotKind::Float => format!("{value:.2}"),
        // Stored radians, shown as degrees (see ParamType::Angle).
        ParamSnapshotKind::Angle => format!("{:.0}°", value.to_degrees()),
        // Stored rad/s, shown as Hz (see ParamType::Frequency).
        ParamSnapshotKind::Frequency => format!("{:.2} Hz", value / std::f32::consts::TAU),
        ParamSnapshotKind::Int => format!("{}", value as i64),
        ParamSnapshotKind::Bool => if value >= 0.5 { "On" } else { "Off" }.to_string(),
        _ => return None,
    };
    let fill = match kind {
        ParamSnapshotKind::Float
        | ParamSnapshotKind::Angle
        | ParamSnapshotKind::Frequency
        | ParamSnapshotKind::Int => range.map(|(lo, hi)| {
            if hi > lo {
                ((value - lo) / (hi - lo)).clamp(0.0, 1.0)
            } else {
                0.0
            }
        }),
        _ => None,
    };
    Some((s, fill))
}

/// Scrub metadata for the draggable numeric kinds (Float / Angle / Frequency /
/// Int) that declared a range. `None` otherwise.
pub(crate) fn scrub_for(
    kind: crate::graph_view::ParamSnapshotKind,
    value: f32,
    range: Option<(f32, f32)>,
) -> Option<ScrubInfo> {
    use crate::graph_view::ParamSnapshotKind;
    match kind {
        ParamSnapshotKind::Float
        | ParamSnapshotKind::Angle
        | ParamSnapshotKind::Frequency
        | ParamSnapshotKind::Int => range.map(|(lo, hi)| ScrubInfo {
            range: (lo, hi),
            current_value: value,
            is_int: matches!(kind, ParamSnapshotKind::Int),
        }),
        _ => None,
    }
}

/// Format an RGBA colour (0..1 components) as `#RRGGBB` for the node-face value
/// cell. Alpha is dropped from the compact face string (it's still editable on
/// the inspector's A channel). Shared with the inspector's swatch path.
pub(crate) fn format_color_hex(c: [f32; 4]) -> String {
    let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02X}{:02X}{:02X}", to_u8(c[0]), to_u8(c[1]), to_u8(c[2]))
}

/// Whether a sparkline history actually moves — `true` only if its range spans
/// more than a hair. A dead-flat trace (a static, unmodulated knob) isn't worth
/// the ink, so the node face stays clean until something drives the param.
pub(crate) fn spark_has_variation(hist: &std::collections::VecDeque<f32>) -> bool {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for &v in hist {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    hi - lo > 0.01
}

/// Pick the node's most informative param and format it as a one-line
/// summary ("Mode: FoldX", "Scale: 0.02") shown on the collapsed node face.
/// Prefers an enum (its label is descriptive), then a numeric, else the
/// first param. `None` for param-less nodes.
pub(crate) fn node_summary(params: &[crate::graph_view::ParamSnapshot]) -> Option<String> {
    use crate::graph_view::ParamSnapshotKind;
    let pick = params
        .iter()
        .find(|p| p.kind == ParamSnapshotKind::Enum)
        .or_else(|| {
            params.iter().find(|p| {
                matches!(
                    p.kind,
                    ParamSnapshotKind::Float
                        | ParamSnapshotKind::Angle
                        | ParamSnapshotKind::Frequency
                        | ParamSnapshotKind::Int
                )
            })
        })
        .or_else(|| params.first())?;
    let pv = format_param_for_node(pick);
    Some(format!("{}: {}", pv.label, pv.value))
}

/// Wrap `text` to lines no wider than `max_chars`, breaking on spaces.
/// A single word longer than the limit is left whole — it overflows the
/// box a touch rather than being chopped mid-word. Only the hover tooltip
/// calls this, so the per-call allocation is off any hot path.
pub(crate) fn wrap_text(text: &str, max_chars: usize) -> Vec<String> {
    let max = max_chars.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if line.is_empty() {
            line.push_str(word);
        } else if line.chars().count() + 1 + word.chars().count() <= max {
            line.push(' ');
            line.push_str(word);
        } else {
            lines.push(std::mem::take(&mut line));
            line.push_str(word);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Muted header tint per node `Category`, so the graph reads at a glance by
/// family (Color & Tone warm, Noise teal, Distort purple, ...). Kept low in
/// saturation and brightness so headers stay subtle on the dark canvas; an
/// exhaustive match means a new `Category` variant forces a colour choice
/// here rather than silently defaulting.
pub(crate) fn category_header_color(cat: crate::graph_view::Category) -> [f32; 4] {
    use crate::graph_view::Category as C;
    match cat {
        C::ColorAndTone => [0.40, 0.30, 0.22, 1.0],
        C::BlurAndSharpen => [0.22, 0.30, 0.40, 1.0],
        C::DistortAndWarp => [0.34, 0.24, 0.40, 1.0],
        C::Stylize => [0.40, 0.24, 0.34, 1.0],
        C::Generate => [0.24, 0.36, 0.28, 1.0],
        C::Noise => [0.22, 0.36, 0.36, 1.0],
        C::Mask => [0.30, 0.30, 0.34, 1.0],
        C::Composite => [0.26, 0.28, 0.42, 1.0],
        C::Geometry3D => [0.30, 0.26, 0.42, 1.0],
        C::MaterialsAndLighting => [0.38, 0.32, 0.22, 1.0],
        C::Particles2D => [0.24, 0.34, 0.40, 1.0],
        C::Particles3D => [0.22, 0.32, 0.42, 1.0],
        C::Control => [0.36, 0.34, 0.22, 1.0],
        C::DetectionAndSampling => [0.40, 0.26, 0.26, 1.0],
        C::MathAndConvert => [0.30, 0.30, 0.30, 1.0],
        C::Routing => [0.26, 0.30, 0.38, 1.0],
        C::FieldsAndCoordinates => [0.24, 0.34, 0.34, 1.0],
        C::Uncategorized => NODE_HEADER_BG,
    }
}

#[derive(Debug, Clone)]
pub(crate) struct WireView {
    pub(crate) from_node: u32,
    pub(crate) from_port: String,
    pub(crate) to_node: u32,
    pub(crate) to_port: String,
}

/// A port resolved from a screen-space cursor position. Used by the
/// wire-drag hit test.
#[derive(Debug, Clone)]
pub(crate) struct PortHit {
    pub(crate) node_id: u32,
    pub(crate) port_name: String,
    pub(crate) is_output: bool,
}

impl GraphCanvas {

    /// Visible previewable nodes as `(capture node_id, strip_x, strip_y,
    /// strip_w, strip_h)` in screen space — the 16:9 preview-strip region the
    /// present pass blits each node's atlas thumbnail into. The `node_id` is the
    /// *capture* id ([`NodeView::preview_node_id`]): a node's own id, or for a
    /// group the inner node producing its output — so groups preview too,
    /// reusing the producer's existing atlas cell. Nodes with no image output
    /// emit nothing. Culls off-canvas nodes.
    pub fn visible_node_thumbnails(
        &self,
        viewport: Rect,
    ) -> Vec<(manifold_foundation::NodeId, f32, f32, f32, f32)> {
        let mut out = Vec::new();
        let header = NODE_HEADER_HEIGHT * self.zoom;
        let pad = PREVIEW_PAD * self.zoom;
        for node in &self.nodes {
            let Some(capture_id) = node.preview_node_id.clone() else {
                continue;
            };
            let (sx, sy) = self.to_screen(viewport, node.pos_graph.0, node.pos_graph.1);
            let sw = NODE_WIDTH * self.zoom;
            let sh = node.height() * self.zoom;
            if sx + sw < viewport.x
                || sx > viewport.x + viewport.w
                || sy + sh < viewport.y
                || sy > viewport.y + viewport.h
            {
                continue;
            }
            let strip_w = PREVIEW_IMG_W * self.zoom;
            let strip_h = PREVIEW_IMG_H * self.zoom;
            if strip_h > 1.0 {
                out.push((capture_id, sx + pad, sy + header + pad, strip_w, strip_h));
            }
        }
        out
    }

    /// Whether `node` matches the active search — its title or handle contains
    /// the query. Always true when no search is active.
    pub(crate) fn node_matches_search(&self, node: &NodeView) -> bool {
        if self.node_search.is_empty() {
            return true;
        }
        node.title.to_ascii_lowercase().contains(&self.node_search)
            || node
                .handle
                .as_deref()
                .is_some_and(|h| h.to_ascii_lowercase().contains(&self.node_search))
    }


    /// For a single selected group: its id, current display name, and the
    /// screen-space rect of its header (where the rename field anchors). `None`
    /// unless exactly one group node is selected. Drives F2-to-rename.
    pub fn group_rename_target(&self, viewport: Rect) -> Option<(u32, String, f32, f32, f32, f32)> {
        let gid = self.single_selected_group()?;
        let node = self.nodes.iter().find(|n| n.id == gid)?;
        let (sx, sy) = self.to_screen(viewport, node.pos_graph.0, node.pos_graph.1);
        Some((
            gid,
            node.title.clone(),
            sx,
            sy,
            NODE_WIDTH * self.zoom,
            NODE_HEADER_HEIGHT * self.zoom,
        ))
    }

    /// Push the latest snapshot. Rebuilds nodes+wires; recomputes
    /// auto-layout only when topology changed.
    pub fn set_snapshot(&mut self, snap: &GraphSnapshot) {
        // Resolve which level the current scope addresses. If the path no
        // longer resolves — the group was deleted, ungrouped, or an undo
        // pulled it out from under us — drop back to the document root.
        if !self.scope.is_empty() && resolve_level(snap, &self.scope).is_none() {
            group_log!("scope {:?} no longer resolves — returning to root", self.scope);
            self.scope.clear();
            self.scope_titles.clear();
        }
        let (level_nodes, level_wires) =
            resolve_level(snap, &self.scope).unwrap_or((&snap.nodes, &snap.wires));

        // Hash the resolved level (not the whole snapshot) plus the scope, so
        // entering or leaving a group re-runs layout even though the
        // underlying snapshot is byte-for-byte the same document.
        let new_hash = hash_level(&self.scope, level_nodes, level_wires);
        if new_hash == self.topology_hash && !self.nodes.is_empty() {
            // Topology unchanged — keep the existing layout, but refresh
            // each node's on-face param values in place. They show live
            // values now, so a param-only change (a driver moving a knob,
            // an inspector edit) must update them without re-running
            // auto-layout.
            for node in &mut self.nodes {
                if let Some(sn) = level_nodes.iter().find(|s| s.id == node.id) {
                    // Param tooltips ride the snapshot (baked at translation),
                    // so `format_param_for_node` carries them straight through —
                    // no re-resolve, no carry-forward-by-index dance.
                    node.params = sn.parameters.iter().map(format_param_for_node).collect();
                    node.summary = node_summary(&sn.parameters);
                }
            }
            return;
        }
        self.topology_hash = new_hash;

        // Preserve positions for nodes that already existed before the
        // topology change. Without this, every wire connection would
        // re-run depth-based auto-layout against the new topology,
        // shifting unrelated nodes into different columns — looked
        // like the graph "snapping to weird positions" each time.
        let prev_positions: ahash::AHashMap<u32, (f32, f32)> = self
            .nodes
            .iter()
            .map(|n| (n.id, n.pos_graph))
            .collect();

        let new_nodes: Vec<NodeView> = level_nodes
            .iter()
            .map(|n| NodeView {
                id: n.id,
                node_id: n.node_id.clone(),
                handle: n.node_handle.clone(),
                title: n.title.clone(),
                params: n.parameters.iter().map(format_param_for_node).collect(),
                summary: node_summary(&n.parameters),
                collapsed: self.collapsed.get(&n.id).copied().unwrap_or(true),
                // Category + tooltips are baked into the snapshot at translation
                // time (the renderer's `descriptor_for`/`tooltip_for`), so the
                // catalog stays renderer-side and the canvas just reads them.
                header_color: category_header_color(n.category),
                pos_graph: prev_positions
                    .get(&n.id)
                    .copied()
                    .unwrap_or((f32::NAN, f32::NAN)),
                inputs: n
                    .inputs
                    .iter()
                    .map(|p| PortView::from_kind(p.name.clone(), &p.kind))
                    .collect(),
                outputs: n
                    .outputs
                    .iter()
                    .map(|p| PortView::from_kind(p.name.clone(), &p.kind))
                    .collect(),
                breaks_dependency_cycle: n.breaks_dependency_cycle,
                is_group: n.type_id == GROUP_TYPE_ID,
                group_tint: n.group.as_ref().and_then(|g| g.tint),
                tooltip: n.tooltip.clone(),
                preview_node_id: node_preview_target(n),
            })
            .collect();
        self.nodes = new_nodes;
        self.wires = level_wires
            .iter()
            .map(|w| WireView {
                from_node: w.from_node,
                from_port: w.from_port.clone(),
                to_node: w.to_node,
                to_port: w.to_port.clone(),
            })
            .collect();

        // Drop sparkline histories for nodes that left this level (group
        // navigation, delete, ungroup) so the map can't grow unbounded across a
        // long authoring session. Param-only edits keep the same topology hash
        // and take the early-return path above, so traces survive a knob tweak.
        if !self.spark_history.is_empty() {
            self.spark_history
                .retain(|id, _| self.nodes.iter().any(|n| &n.node_id == id));
        }

        // Two-step position assignment:
        //   1. Auto-layout writes columns/rows for every node, but
        //      we only keep its result for nodes that didn't have a
        //      previous position (the freshly added ones).
        //   2. Stored `editor_pos` from the def overrides on top for
        //      any node the user has explicitly moved.
        let unplaced_ids: Vec<u32> = self
            .nodes
            .iter()
            .filter(|n| !n.pos_graph.0.is_finite())
            .map(|n| n.id)
            .collect();
        if !unplaced_ids.is_empty() {
            // Save and restore positions of already-placed nodes so
            // auto_layout (which writes to every node) doesn't disturb
            // them. Cheap — graphs are small.
            let saved: Vec<((f32, f32), u32)> = self
                .nodes
                .iter()
                .filter(|n| n.pos_graph.0.is_finite())
                .map(|n| (n.pos_graph, n.id))
                .collect();
            self.auto_layout();
            for (pos, id) in saved {
                if let Some(n) = self.nodes.iter_mut().find(|n| n.id == id) {
                    n.pos_graph = pos;
                }
            }
        }
        for (view, snap_node) in self.nodes.iter_mut().zip(level_nodes.iter()) {
            if let Some(p) = snap_node.editor_pos {
                view.pos_graph = p;
            }
        }

        // Auto-format on first entry into a never-laid-out group. `auto_layout`
        // above already positioned the nodes (all were unplaced); persist those
        // positions so the tidy layout sticks. Only fires when EVERY node lacks
        // a saved `editor_pos`, so a manual arrangement is never overwritten.
        // RelayoutGraph routes through the non-structural layout command, so it
        // doesn't rebuild the chain or reset state.
        if std::mem::take(&mut self.format_on_enter)
            && !self.nodes.is_empty()
            && level_nodes.iter().all(|n| n.editor_pos.is_none())
        {
            let positions: Vec<(u32, (f32, f32))> =
                self.nodes.iter().map(|n| (n.id, n.pos_graph)).collect();
            self.pending_actions.push(GraphEditCommand::RelayoutGraph {
                scope_path: self.scope.clone(),
                positions,
            });
        }
    }


    /// Overlay this frame's live (post-modulation) param values onto the node
    /// faces. The structural snapshot ([`Self::set_snapshot`]) only rebuilds on
    /// a `graph_version` bump, so a driver / Ableton / envelope / card slider
    /// moving a knob never reached the canvas — this closes that gap by
    /// refreshing each on-face value string, fill bar, and scrub anchor every
    /// frame from `ContentState::live_node_params`, matched by stable `NodeId`.
    ///
    /// Only continuous-numeric and bool params are touched (enums, triggers, and
    /// multi-component params keep their snapshot display, which an edit already
    /// refreshes via `graph_version`). The param the user is actively scrubbing
    /// is skipped so the live feed never fights a drag. No-op when `live` is
    /// empty (no editor watching), so the closed-editor path pays nothing.
    pub fn apply_live_values(&mut self, live: &crate::graph_view::LiveNodeParams) {
        if live.is_empty() {
            return;
        }
        let by_id: ahash::AHashMap<&manifold_foundation::NodeId, &Vec<(&'static str, f32)>> =
            live.iter().map(|(id, vals)| (id, vals)).collect();
        // The param the user is mid-scrub on stays the source of truth until
        // release; cloned so the per-node mutable walk below has no live borrow
        // of `self.drag_mode`.
        let scrubbing: Option<(u32, String)> = match &self.drag_mode {
            DragMode::ParamScrub {
                node_id,
                param_name,
                ..
            } => Some((*node_id, param_name.clone())),
            _ => None,
        };
        // Sparkline samples gathered during the mutable node walk, applied to
        // `spark_history` afterwards (can't touch `self.spark_history` while
        // `self.nodes` is borrowed mutably).
        let mut spark_updates: Vec<(manifold_foundation::NodeId, f32)> = Vec::new();
        for node in &mut self.nodes {
            if node.node_id.is_empty() {
                continue;
            }
            let Some(vals) = by_id.get(&node.node_id) else {
                continue;
            };
            let node_id = node.id;
            // The node's first ranged numeric param drives its sparkline — the
            // same "primary" pick the collapsed summary uses, so the trace and
            // the summary read the same knob.
            let mut primary_fill: Option<f32> = None;
            for pv in &mut node.params {
                if scrubbing
                    .as_ref()
                    .is_some_and(|(sn, sp)| *sn == node_id && sp.as_str() == pv.name)
                {
                    continue;
                }
                let Some(&(_, value)) = vals.iter().find(|(name, _)| *name == pv.name) else {
                    continue;
                };
                let Some((value_str, fill)) = numeric_value_fill(pv.kind, value, pv.range) else {
                    continue;
                };
                pv.value = value_str;
                pv.fill = fill;
                if let Some(scrub) = pv.scrub.as_mut() {
                    scrub.current_value = value;
                }
                if primary_fill.is_none()
                    && let Some(f) = fill
                {
                    primary_fill = Some(f);
                }
            }
            if let Some(f) = primary_fill {
                spark_updates.push((node.node_id.clone(), f));
            }
        }
        for (id, f) in spark_updates {
            let hist = self.spark_history.entry(id).or_default();
            if hist.len() >= SPARK_CAPACITY {
                hist.pop_front();
            }
            hist.push_back(f);
        }
    }

    pub(crate) fn find_node(&self, id: u32) -> Option<&NodeView> {
        self.nodes.iter().find(|n| n.id == id)
    }
}

/// Walk `scope` (a path of group node ids) into `snap`, returning the
/// `(nodes, wires)` of the addressed level. Empty scope → the document root.
/// `None` if any id in the path isn't a group at its level — e.g. the group
/// was deleted or ungrouped out from under the canvas. Pure; unit-tested.
pub fn resolve_level<'a>(
    snap: &'a GraphSnapshot,
    scope: &[u32],
) -> Option<(&'a [NodeSnapshot], &'a [WireSnapshot])> {
    let mut nodes: &[NodeSnapshot] = &snap.nodes;
    let mut wires: &[WireSnapshot] = &snap.wires;
    for &gid in scope {
        let group = nodes.iter().find(|n| n.id == gid)?.group.as_deref()?;
        nodes = &group.nodes;
        wires = &group.wires;
    }
    Some((nodes, wires))
}

/// Locate a node by stable [`NodeId`](manifold_foundation::NodeId) anywhere in the
/// hierarchical snapshot. Returns the scope path of group runtime ids to its
/// level, those groups' titles (for the breadcrumb), and the node's runtime id.
/// `None` if no node carries that id. Used by jump-to-node to navigate the
/// canvas to a card param's defining node, even when it lives inside a group.
pub(crate) fn find_node_scope(
    snap: &GraphSnapshot,
    target: &manifold_foundation::NodeId,
) -> Option<(Vec<u32>, Vec<String>, u32)> {
    fn search(
        nodes: &[NodeSnapshot],
        target: &manifold_foundation::NodeId,
        path: &mut Vec<u32>,
        titles: &mut Vec<String>,
    ) -> Option<u32> {
        // Prefer a direct hit at this level over descending.
        if let Some(n) = nodes.iter().find(|n| &n.node_id == target) {
            return Some(n.id);
        }
        for n in nodes {
            if let Some(group) = n.group.as_deref() {
                path.push(n.id);
                titles.push(n.title.clone());
                if let Some(rid) = search(&group.nodes, target, path, titles) {
                    return Some(rid);
                }
                path.pop();
                titles.pop();
            }
        }
        None
    }
    if target.is_empty() {
        return None;
    }
    let mut path = Vec::new();
    let mut titles = Vec::new();
    search(&snap.nodes, target, &mut path, &mut titles).map(|rid| (path, titles, rid))
}

/// Resolve a card param id (a binding's `outer_param_id`) to the stable
/// [`NodeId`](manifold_foundation::NodeId) of the node it's exposed from, using the
/// snapshot's `outer_routings` (the same map for effects and generators). The
/// routing carries the node *handle*; we resolve that to the node's id so
/// jump-to-node addresses by the grouping-invariant identity. `None` when the
/// param isn't a routed binding or its node isn't in the snapshot.
pub fn resolve_card_param_node_id(
    snap: &GraphSnapshot,
    param_id: &str,
) -> Option<manifold_foundation::NodeId> {
    let handle = snap
        .outer_routings
        .iter()
        .find(|r| r.outer_param_id == param_id)
        .map(|r| r.node_handle.clone())?;
    node_id_for_handle(snap, &handle)
}

/// The stable `NodeId` of the node whose handle is `handle`, searched through
/// the full nested snapshot. `None` if no such (id-bearing) node exists.
pub(crate) fn node_id_for_handle(snap: &GraphSnapshot, handle: &str) -> Option<manifold_foundation::NodeId> {
    fn search(nodes: &[NodeSnapshot], handle: &str) -> Option<manifold_foundation::NodeId> {
        for n in nodes {
            if n.node_handle.as_deref() == Some(handle) && !n.node_id.is_empty() {
                return Some(n.node_id.clone());
            }
            if let Some(group) = n.group.as_deref()
                && let Some(id) = search(&group.nodes, handle)
            {
                return Some(id);
            }
        }
        None
    }
    search(&snap.nodes, handle)
}

/// Topology hash of one resolved level plus the scope path, so the canvas
/// re-runs layout when the displayed level changes (enter/leave a group)
/// even though the underlying snapshot document is byte-for-byte the same.
/// Param values are deliberately excluded — they refresh in place without a
/// relayout (see the param-only fast path in `set_snapshot`).
pub(crate) fn hash_level(scope: &[u32], nodes: &[NodeSnapshot], wires: &[WireSnapshot]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = ahash::AHasher::default();
    scope.hash(&mut h);
    nodes.len().hash(&mut h);
    for n in nodes {
        n.id.hash(&mut h);
        n.type_id.hash(&mut h);
    }
    wires.len().hash(&mut h);
    for w in wires {
        w.from_node.hash(&mut h);
        w.from_port.hash(&mut h);
        w.to_node.hash(&mut h);
        w.to_port.hash(&mut h);
    }
    h.finish()
}
