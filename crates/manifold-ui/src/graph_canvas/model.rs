//! View model: snapshot ingestion, the on-canvas node/param/wire view
//! structs and their geometry, value formatting, and scope/snapshot
//! resolution. Pure data shaping — no rendering, no input.

use super::*;

#[derive(Debug, Clone)]
pub(crate) struct PortView {
    pub(crate) name: String,
    pub(crate) color: Color32,
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
            PortKindSnapshot::Transform => PORT_TRANSFORM_COLOR,
            PortKindSnapshot::Atmosphere => PORT_ATMOSPHERE_COLOR,
            PortKindSnapshot::Object => PORT_OBJECT_COLOR,
        };
        let is_control = matches!(kind, PortKindSnapshot::Scalar);
        Self {
            name,
            color,
            is_control,
        }
    }
}

/// One row in an **expanded** node's body, in top-to-bottom draw order. The
/// single geometry source: render draws it, hit-test reads it, and the port
/// position helpers resolve a socket's y from it — so a click target, a drawn
/// dot, and a wire endpoint can never disagree. Blender-style: outputs first
/// (dot on the right), then each param on its own row (with its shadowing input
/// socket inline on the left when one exists), then any input ports that don't
/// shadow a param (textures / arrays) as their own rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeRow {
    /// An output port: dot on the right edge, name right-aligned. Field is the
    /// index into `NodeView::outputs`.
    Output { port: usize },
    /// A parameter row. `param` indexes `NodeView::params`; `input_port` is the
    /// index into `NodeView::inputs` of the same-named scalar input that shadows
    /// it (port-shadows-param), drawn as a socket dot on the row's left edge, or
    /// `None` for a param with no input port (e.g. an enum).
    Param {
        param: usize,
        input_port: Option<usize>,
    },
    /// A non-param input port (texture / array / any input with no same-named
    /// param): dot on the left edge, name. Field indexes `NodeView::inputs`.
    Input { port: usize },
    /// A one-click gesture button occupying its own row
    /// (`docs/SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md` §2 D7/D7a) — currently
    /// only "+ Object" / "+ Light" on `render_scene`'s face, spliced in right
    /// after the `objects`/`lights` param rows by `GraphCanvas::rebuild_rows`.
    /// No port, no param index; a click anywhere on the row fires the
    /// gesture. Never produced by [`compute_node_rows`] itself (which knows
    /// nothing of node type) — see the render_scene-specific splice.
    Action(NodeActionKind),
}

/// Which one-click scene-build gesture a [`NodeRow::Action`] row triggers.
/// Copy so `NodeRow` (which derives Copy) can carry it directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeActionKind {
    /// D7: bump `objects` + build a placeholder cube+material+transform
    /// group wired into the new `mesh_k`/`material_k`/`transform_k` ports.
    AddSceneObject,
    /// D7a: bump `lights` + spawn a bare `node.light` wired into the new
    /// `light_k` port.
    AddSceneLight,
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
    /// Expanded-body row layout (outputs, param rows with inline input sockets,
    /// leftover inputs) — see [`NodeRow`]. Recomputed on topology rebuild from
    /// `inputs`/`outputs`/`params`; unused while collapsed (which keeps its
    /// compact port band). The one source render / hit / port-position read.
    pub(crate) rows: Vec<NodeRow>,
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
    pub(crate) header_color: Color32,
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
    pub(crate) group_tint: Option<Color32>,
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
    /// Size `(w, h)` in graph units of the recessed preview screen, at the
    /// project aspect ratio (see [`preview_screen_size`]). `Some` exactly when
    /// `preview_node_id` is `Some`. Recomputed on a topology rebuild and
    /// whenever the project aspect changes ([`GraphCanvas::set_preview_aspect`]).
    pub(crate) preview_screen: Option<(f32, f32)>,
    /// Custom WGSL kernel source for a `wgsl_compute*` node, or `None` for every
    /// other node. Its presence gives an expanded node an "Edit Code…" footer
    /// strip whose click opens the multiline kernel editor (`EditGraphNodeWgsl`).
    /// Mirrors [`crate::graph_view::NodeSnapshot::wgsl_source`]; stable per node,
    /// so it's set once on the topology rebuild. (Phase 4.)
    pub(crate) wgsl_source: Option<String>,
    /// Count of ports that CAN be hidden as unused (reveal-independent) — an
    /// unwired socket whose same-kind sibling is wired. Drives the header chip:
    /// `> 0` means the chip shows ("+N" to reveal when hidden, "▾" to re-hide when
    /// revealed). `0` means no chip (a fresh node, or every socket wired). Set by
    /// [`GraphCanvas::rebuild_rows`] alongside [`Self::rows`].
    pub(crate) hideable_ports: usize,
    /// Whether this node is currently showing its hideable sockets (the reveal
    /// chip was toggled on). Mirrors `GraphCanvas::revealed_ports` for this node
    /// so render/hit read it off the view. Set by [`GraphCanvas::rebuild_rows`].
    pub(crate) revealed: bool,
    /// `true` when `type_id == "node.render_scene"` — drives the "+ Object" /
    /// "+ Light" gesture-button rows spliced onto this node's face
    /// (`docs/SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md` §2 D7/D7a). The literal
    /// mirrors `manifold_renderer::node_graph::primitives::render_scene::RENDER_SCENE_TYPE_ID`
    /// — `manifold-ui` doesn't depend on `manifold-renderer`, so this is a
    /// same-string re-derivation, not a shared constant. Resolved once on the
    /// topology rebuild — it never changes for a given node id.
    pub(crate) is_render_scene: bool,
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
        let base = NODE_HEADER_HEIGHT + self.preview_h();
        if self.collapsed {
            // Collapsed: summary line + the compact port band (inputs left /
            // outputs right, one row per port index).
            let port_rows = self.inputs.len().max(self.outputs.len()) as f32;
            base + self.body_h() + port_rows * PORT_ROW_HEIGHT + 6.0
        } else {
            // Expanded: one uniform row per NodeRow — ports live inline, so no
            // separate band. A `wgsl_compute` node adds an "Edit Code…" footer
            // strip below the rows (opens the kernel editor).
            base + self.rows.len() as f32 * PARAM_ROW_H
                + self.wgsl_footer_h()
                + 6.0
        }
    }

    /// Height of the expanded "Edit Code…" footer strip, or `0` for a node with
    /// no custom WGSL kernel. Only an expanded node draws the footer; a collapsed
    /// node hides it (expand to edit the kernel).
    pub(crate) fn wgsl_footer_h(&self) -> f32 {
        if self.wgsl_source.is_some() && !self.collapsed {
            WGSL_FOOTER_H
        } else {
            0.0
        }
    }

    /// Y offset (from the node top) of the "Edit Code…" footer strip — below the
    /// header, preview band, and all param rows. `None` unless this is an
    /// expanded node carrying a custom WGSL kernel. The single geometry source
    /// the renderer draws and the hit-test clicks, so they can't drift.
    pub(crate) fn wgsl_footer_offset(&self) -> Option<f32> {
        (self.wgsl_source.is_some() && !self.collapsed).then(|| {
            NODE_HEADER_HEIGHT + self.preview_h() + self.rows.len() as f32 * PARAM_ROW_H
        })
    }

    /// Height of the output-preview band below the header: the project-aspect
    /// screen plus its padding, or `0` for a node that emits no image.
    /// Zoom-independent.
    pub(crate) fn preview_h(&self) -> f32 {
        match self.preview_screen {
            Some((_, h)) => h + 2.0 * PREVIEW_PAD,
            None => 0.0,
        }
    }

    /// Height of the collapsed body block below the header: the single summary
    /// line (if any). Expanded bodies lay out per-row via [`Self::rows`], so this
    /// is only consulted on the collapsed path (the port band sits below it).
    pub(crate) fn body_h(&self) -> f32 {
        if self.summary.is_some() {
            PARAM_ROW_H
        } else {
            0.0
        }
    }

    /// Y offset (from the node top) where the **collapsed** port band starts —
    /// below the header, preview band, and summary line.
    pub(crate) fn ports_y_offset(&self) -> f32 {
        NODE_HEADER_HEIGHT + self.preview_h() + self.body_h()
    }

    /// Y offset (from the node top) of the centre of expanded body row `row`.
    /// Rows begin right below the preview band (no separate param/port split).
    pub(crate) fn expanded_row_center(&self, row: usize) -> f32 {
        NODE_HEADER_HEIGHT + self.preview_h() + row as f32 * PARAM_ROW_H + PARAM_ROW_H * 0.5
    }

    /// Row index of input port `idx` in the expanded layout — the param row it
    /// shadows, or its own leftover-input row. `None` if it isn't laid out
    /// (shouldn't happen for a live port).
    pub(crate) fn input_row_of(&self, idx: usize) -> Option<usize> {
        self.rows.iter().position(|r| match r {
            NodeRow::Param {
                input_port: Some(ii),
                ..
            } => *ii == idx,
            NodeRow::Input { port } => *port == idx,
            _ => false,
        })
    }

    /// Row index of output port `idx` in the expanded layout.
    pub(crate) fn output_row_of(&self, idx: usize) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| matches!(r, NodeRow::Output { port } if *port == idx))
    }

    pub(crate) fn input_port_pos_graph(&self, idx: usize) -> (f32, f32) {
        let (x, y) = self.pos_graph;
        if self.collapsed {
            (
                x,
                y + self.ports_y_offset() + idx as f32 * PORT_ROW_HEIGHT + PORT_ROW_HEIGHT * 0.5,
            )
        } else {
            let row = self.input_row_of(idx).unwrap_or(0);
            (x, y + self.expanded_row_center(row))
        }
    }

    pub(crate) fn output_port_pos_graph(&self, idx: usize) -> (f32, f32) {
        let (x, y) = self.pos_graph;
        if self.collapsed {
            (
                x + NODE_WIDTH,
                y + self.ports_y_offset() + idx as f32 * PORT_ROW_HEIGHT + PORT_ROW_HEIGHT * 0.5,
            )
        } else {
            let row = self.output_row_of(idx).unwrap_or(0);
            (x + NODE_WIDTH, y + self.expanded_row_center(row))
        }
    }

    /// Y-offset (from the node's top edge) of the named input port's centre.
    /// Used by auto-layout to align a node so this wire's two ports line up,
    /// rather than aligning box-centre to box-centre. Falls back to the node
    /// mid-height for an unknown name (shouldn't happen for a live wire).
    ///
    /// Computed from the row layout directly, NEVER via `pos_graph` — auto-layout
    /// calls this while positions are still `NaN` (it's computing them), and
    /// routing the offset through the node origin would make `NaN - NaN = NaN`
    /// poison the whole layout.
    pub(crate) fn input_port_offset(&self, name: &str) -> f32 {
        match self.inputs.iter().position(|p| p.name == name) {
            Some(idx) => {
                if self.collapsed {
                    self.ports_y_offset() + idx as f32 * PORT_ROW_HEIGHT + PORT_ROW_HEIGHT * 0.5
                } else {
                    self.expanded_row_center(self.input_row_of(idx).unwrap_or(0))
                }
            }
            None => self.height() * 0.5,
        }
    }

    /// Y-offset (from the node's top edge) of the named output port's centre.
    /// Companion to [`input_port_offset`](Self::input_port_offset) — same
    /// `pos_graph`-free rule.
    pub(crate) fn output_port_offset(&self, name: &str) -> f32 {
        match self.outputs.iter().position(|p| p.name == name) {
            Some(idx) => {
                if self.collapsed {
                    self.ports_y_offset() + idx as f32 * PORT_ROW_HEIGHT + PORT_ROW_HEIGHT * 0.5
                } else {
                    self.expanded_row_center(self.output_row_of(idx).unwrap_or(0))
                }
            }
            None => self.height() * 0.5,
        }
    }
}

/// Compute the expanded-body [`NodeRow`] layout for a node: outputs first, then
/// one row per param (carrying the same-named input port as its inline socket),
/// then any input ports that don't shadow a param. Port-shadows-param is matched
/// by exact name — the renderer's convention (see the `node.math` primitive:
/// input ports `a`/`b` shadow params `a`/`b`).
/// Compute the expanded body layout. A port gets a row when it's *visible* —
/// `output_visible[i]` / `input_visible[ii]`, index-aligned to the port lists
/// (the caller folds wired-ness + reveal into these). Params always get a row
/// (their shadowing input socket rides the param row, wired or not); a leftover
/// input (no param) or an output is dropped when not visible. Pure over its
/// inputs, so it's unit-tested directly.
pub(crate) fn compute_node_rows(
    inputs: &[PortView],
    outputs: &[PortView],
    params: &[ParamView],
    output_visible: &[bool],
    input_visible: &[bool],
) -> Vec<NodeRow> {
    let mut rows = Vec::with_capacity(outputs.len() + params.len() + inputs.len());
    for i in 0..outputs.len() {
        if output_visible.get(i).copied().unwrap_or(true) {
            rows.push(NodeRow::Output { port: i });
        }
    }
    let mut shadowed = vec![false; inputs.len()];
    for (pi, p) in params.iter().enumerate() {
        let input_port = inputs.iter().position(|ip| ip.name == p.name);
        if let Some(ii) = input_port {
            shadowed[ii] = true;
        }
        rows.push(NodeRow::Param {
            param: pi,
            input_port,
        });
    }
    for (ii, sh) in shadowed.iter().enumerate() {
        // Shadowed inputs ride their param row (always shown); a leftover input is
        // dropped when not visible.
        if !sh && input_visible.get(ii).copied().unwrap_or(true) {
            rows.push(NodeRow::Input { port: ii });
        }
    }
    rows
}

/// Insert the "+ Object" / "+ Light" [`NodeRow::Action`] rows right after
/// `render_scene`'s `objects`/`lights` param rows (D7/D7a). Pure over its
/// input (row list + param list only, no node/wire access), so it's
/// unit-tested directly rather than only through the full `rebuild_rows`
/// pipeline. Lights spliced first (higher row index) so its insertion
/// doesn't shift the objects row's index out from under the second splice.
/// A no-op (both rows absent) if `rows` carries no `objects`/`lights` param
/// row — callers gate on `NodeView::is_render_scene` first, so this should
/// always find both, but the function stays defensive either way.
pub(crate) fn splice_render_scene_action_rows(rows: &mut Vec<NodeRow>, params: &[ParamView]) {
    let row_for = |rows: &[NodeRow], name: &str| {
        rows.iter()
            .position(|r| matches!(r, NodeRow::Param { param, .. } if params[*param].name == name))
    };
    if let Some(li) = row_for(rows, "lights") {
        rows.insert(li + 1, NodeRow::Action(NodeActionKind::AddSceneLight));
    }
    if let Some(oi) = row_for(rows, "objects") {
        rows.insert(oi + 1, NodeRow::Action(NodeActionKind::AddSceneObject));
    }
}

/// Group `wires` by their `(from_node, to_node)` pair, preserving first-seen
/// order — the pure shape behind D8's same-pair ribbon collapse
/// (`docs/SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md` §2). A pair with ≥2
/// members ribbons in `draw_wire_tier`; a pair with exactly 1 draws
/// normally. Pure over its input (no rendering, no `Painter`), so it's
/// unit-tested directly.
pub(crate) fn group_wires_by_pair<'a>(
    wires: impl Iterator<Item = &'a WireView>,
) -> Vec<((u32, u32), Vec<&'a WireView>)> {
    let mut groups: Vec<((u32, u32), Vec<&'a WireView>)> = Vec::new();
    for wire in wires {
        let key = (wire.from_node, wire.to_node);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some(g) => g.1.push(wire),
            None => groups.push((key, vec![wire])),
        }
    }
    groups
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
    /// Raw current value as an `f32`, kept alongside the formatted `value`
    /// string so a discrete on-face edit (bool toggle, trigger fire, enum
    /// dropdown highlight) can read the number without re-parsing the label.
    /// Refreshed by [`GraphCanvas::apply_live_values`] for the kinds it touches.
    pub(crate) current_value: f32,
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
    /// Whether this param is currently exposed on the outer performance card
    /// ([`crate::graph_view::ParamSnapshot::exposed`]). Drives the filled /
    /// hollow expose glyph at the row's left edge; a click on the glyph flips it.
    pub(crate) exposed: bool,
    /// Declared default, carried so a click that exposes this param hands the
    /// new outer-card binding its default — parity with the sidebar's expose
    /// path (`ps.default_value`).
    pub(crate) default_value: f32,
    /// Enum option labels, needed as the outer binding's `value_labels` when an
    /// enum param is exposed. Empty for non-enum params (`unwrap_or_default`).
    pub(crate) enum_labels: Vec<String>,
    /// Live multi-component value (RGBA / XYZW, zero-padded tail) for
    /// `Color` / `Vec2..4` params — the source both the on-face swatch and the
    /// on-node channel editor ([`GraphCanvas::vec_editor`]) read. `[0.0; 4]` for
    /// scalar kinds.
    pub(crate) vec_value: [f32; 4],
    /// Raw untruncated value for a `String` param — the `current` handed to the
    /// on-node text editor (`EditGraphNodeStringParam`). `None` for non-String
    /// params. (Phase 4.)
    pub(crate) string_value: Option<String>,
    /// Row-major cell values for a `Table` param — the grid the on-node
    /// [`TableEditor`](crate::graph_canvas::TableEditor) draws and the `rows`
    /// stashed on each `EditGraphNodeTableCell`. `None` for non-Table params.
    /// (Phase 4.)
    pub(crate) table_value: Option<Vec<Vec<f32>>>,
    /// `true` when this is a path-like `String` param (folder / file / dir /
    /// path) — clicking its value opens the native folder picker
    /// (`BrowseGraphNodePath`) instead of the inline text editor. Always `false`
    /// for non-String kinds. (Phase 4.)
    pub(crate) is_path: bool,
    /// `true` when a wire on this node's same-named scalar input port shadows the
    /// param every frame (port-shadows-param). The row is then **read-only** — a
    /// value scrub / editor and the expose toggle all no-op, since a local edit
    /// would lie about what drives the param — and the label carries a "← wired"
    /// hint. Removing the wire is the only way to reclaim control. Recomputed per
    /// snapshot from the level's wires (`apply_driven_state`). (Phase 5.)
    pub(crate) wire_driven: bool,
    /// `Some("driven by <node>.<port>")` when [`wire_driven`](Self::wire_driven)
    /// is set — the feeding wire's source, resolved once here so the hover
    /// tooltip (`draw_hover_tooltip`) doesn't need its own wire/node join. The
    /// row's normal help text (`tooltip`) still exists but is shadowed while
    /// driven — knowing *what* drives the row is more useful than the param's
    /// static doc line once it's read-only anyway (D5). `None` when not
    /// wire-driven, and for group-face mirror rows ([`build_group_param_rows`]),
    /// which don't have the feeding wire's *level* in scope to resolve a source
    /// title from.
    pub(crate) driven_by: Option<String>,
    /// `Some(outer_label)` when an outer performance-card slider routes into this
    /// inner param every frame. The row **stays editable** (the binding apply
    /// path skips when the outer slot is unchanged, so inline edits survive) but
    /// carries a "↳ <outer>" hint so the user knows which card slider will
    /// reclaim control if moved. `None` for un-routed params. Wire-driven wins
    /// when both apply. Recomputed per snapshot from `outer_routings`. (Phase 5.)
    pub(crate) outer_driver: Option<String>,
    /// `Some(outer_param_id)` for a **group-face mirror row** (D6,
    /// `docs/SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md` §2): this `ParamView`
    /// isn't an inner node's own param row, it's a group box's live copy of an
    /// already-exposed card param whose binding target resolves inside the
    /// group. `None` for every ordinary node-face row. Drives the scrub/click
    /// dispatch: a `Some` row emits `GraphEditCommand::SetOuterParam` (the
    /// card's own write path — the parity invariant) instead of
    /// `SetGraphNodeParam`, and it never draws/hit-tests an expose glyph (the
    /// group face shows the card surface, not an authoring picker). Built by
    /// [`build_group_param_rows`], never by [`format_param_for_node`].
    pub(crate) outer_param_id: Option<String>,
    /// `Some((inner_node_id, inner_param_name))` for a **group-face mirror
    /// row** ([`build_group_param_rows`]): the live feed
    /// (`ContentState::live_node_params`) is keyed by the *inner* node's
    /// stable [`manifold_foundation::NodeId`] and its own param name, not by
    /// this row's `name` (renamed to `outer_param_id`) or by the group
    /// node's own `node_id` (empty — groups are structural, not live nodes).
    /// [`GraphCanvas::apply_live_values`] uses this to look the row up in the
    /// live feed directly instead of matching on the enclosing node.
    /// `None` for every ordinary node-face row, where the enclosing node's
    /// own `node_id` + this row's `name` are already the right key.
    pub(crate) live_source: Option<(manifold_foundation::NodeId, String)>,
}

/// Whether a `String` param names a filesystem path — folder / file / dir /
/// path. Path params browse via the native picker; other strings edit inline.
/// The single definition shared by the node canvas and the (Phase-6-doomed)
/// sidebar so both classify a param the same way.
pub(crate) fn is_path_param(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["folder", "path", "file", "dir"].iter().any(|k| n.contains(k))
}

/// Compact display for a `Table` cell — integers without a decimal point,
/// fractionals to three trimmed places, so a grid of cells stays legible.
/// The single definition shared by the node canvas grid and the sidebar.
pub(crate) fn fmt_table_cell(v: f32) -> String {
    if v == v.trunc() && v.abs() < 1.0e6 {
        format!("{}", v as i64)
    } else {
        crate::fmt::fmt_trimmed(v, 3)
    }
}

/// The param kinds that can be exposed onto the outer performance card — the
/// single-slot scalar-ish family. Mirrors the sidebar's `supported` gate
/// (Color / Vec / String / Table take dedicated editors and are never
/// single-slot card-exposable). Only these draw an interactive expose glyph on
/// the node face.
pub(crate) fn kind_is_exposable(kind: crate::graph_view::ParamSnapshotKind) -> bool {
    use crate::graph_view::ParamSnapshotKind;
    matches!(
        kind,
        ParamSnapshotKind::Float
            | ParamSnapshotKind::Angle
            | ParamSnapshotKind::Frequency
            | ParamSnapshotKind::Int
            | ParamSnapshotKind::Bool
            | ParamSnapshotKind::Enum
            | ParamSnapshotKind::Trigger
    )
}

/// Map a param kind to the outer-binding [`crate::types::ParamConvert`] used
/// when it's exposed. Mirrors the sidebar's mapping (`graph_editor.rs`) exactly,
/// so an on-node expose produces a byte-identical outer-card binding.
pub(crate) fn param_convert_for_kind(
    kind: crate::graph_view::ParamSnapshotKind,
) -> crate::types::ParamConvert {
    use crate::graph_view::ParamSnapshotKind as K;
    use crate::types::ParamConvert as C;
    match kind {
        K::Int => C::IntRound,
        K::Bool => C::BoolThreshold,
        K::Enum => C::EnumRound,
        K::Trigger => C::Trigger,
        // Float / Angle / Frequency and the never-exposable fallbacks → Float.
        _ => C::Float,
    }
}

/// Screen-space bounds `(x, y, diameter)` of a param row's expose glyph, given
/// the row's screen-space top-left (`row_x`, `row_top`), the row height, and the
/// zoom. The single source both the renderer (draw) and the hit-test (click)
/// read, so the toggle target can never drift from what's drawn.
pub(crate) fn expose_glyph_bounds(
    row_x: f32,
    row_top: f32,
    row_h: f32,
    zoom: f32,
) -> (f32, f32, f32) {
    let d = super::PARAM_EXPOSE_D * zoom;
    let gx = row_x + super::PARAM_PAD_X * zoom;
    let gy = row_top + (row_h - d) * 0.5;
    (gx, gy, d)
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
        current_value: p.current_value,
        value,
        fill,
        scrub,
        // Baked into the snapshot at translation time (the renderer's
        // `tooltip_for`), so the formatter carries it straight through.
        tooltip: p.tooltip.clone(),
        exposed: p.exposed,
        default_value: p.default_value,
        enum_labels: p.enum_labels.clone().unwrap_or_default(),
        vec_value: p.vec_value.unwrap_or([0.0; 4]),
        string_value: p.string_value.clone(),
        table_value: p.table_value.clone(),
        is_path: p.kind == ParamSnapshotKind::String && is_path_param(&p.name),
        // Filled by `apply_driven_state` once the node's wires + the snapshot's
        // outer routings are in scope (they aren't per-param on the snapshot).
        wire_driven: false,
        driven_by: None,
        outer_driver: None,
        // Ordinary node-face row — never a group-face mirror. See
        // `build_group_param_rows` for the other constructor.
        outer_param_id: None,
        live_source: None,
    }
}

/// Recursively find the [`crate::graph_view::NodeSnapshot`] with the given
/// author-assigned `handle` inside `body` — descending into nested group
/// bodies too, since a card param can be exposed arbitrarily deep. Returns
/// the node alongside the [`crate::graph_view::WireSnapshot`]s of the level
/// it actually lives at (wires address by structural id *within their own
/// level*, so wire-driven detection must use that level's wires, not the
/// caller's). `None` if no node in the (sub)tree carries this handle.
fn find_node_by_handle<'a>(
    body: &'a crate::graph_view::GroupSnapshot,
    handle: &str,
) -> Option<(&'a crate::graph_view::NodeSnapshot, &'a [crate::graph_view::WireSnapshot])> {
    for n in &body.nodes {
        if n.node_handle.as_deref() == Some(handle) {
            return Some((n, &body.wires));
        }
        if let Some(inner) = n.group.as_deref()
            && let Some(found) = find_node_by_handle(inner, handle)
        {
            return Some(found);
        }
    }
    None
}

/// Build a group `NodeView`'s on-face param rows (D6,
/// `docs/SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md` §2): one [`ParamView`] per
/// `outer_routings` entry whose binding target (`node_handle`, `inner_param`)
/// resolves to a node inside `body`, transitively through nested groups —
/// the same join the canvas already computes for the "↳ outer-driven" hint
/// (`apply_driven_state`), just walked from the group's own body instead of
/// the current level's flat node list. Reuses [`format_param_for_node`]'s
/// value/fill/scrub formatting verbatim, so a group-face row looks and drags
/// identically to the same param's ordinary node-face row; only the label
/// (the outer card's own label), the row identity (`name` — set to the
/// routing's `outer_param_id` so it can never collide with one of the
/// group's own interface port names in `compute_node_rows`'s shadow-match),
/// and `outer_param_id` itself (the scrub-dispatch marker) are overwritten.
/// Called fresh on every `set_snapshot` push — both the full rebuild and the
/// topology-unchanged param refresh — so wire-driven state and live values
/// never go stale even though this group's inner wires live at a level
/// that's never `self.wires` (the currently *viewed* level's wires) unless
/// you happen to be inside this exact group.
pub(crate) fn build_group_param_rows(
    body: &crate::graph_view::GroupSnapshot,
    outer_routings: &[crate::graph_view::OuterParamRouting],
) -> Vec<ParamView> {
    let mut out = Vec::with_capacity(outer_routings.len());
    for r in outer_routings {
        let Some((node, wires)) = find_node_by_handle(body, &r.node_handle) else {
            continue;
        };
        let Some(ps) = node.parameters.iter().find(|p| p.name == r.inner_param) else {
            continue;
        };
        let mut pv = format_param_for_node(ps);
        pv.name = r.outer_param_id.clone();
        pv.label = r.outer_label.clone();
        pv.wire_driven = wires
            .iter()
            .any(|w| w.to_node == node.id && w.to_port == r.inner_param);
        // The row itself IS the outer routing — a second "↳ <outer>" hint on
        // top of itself would be noise, not information.
        pv.outer_driver = None;
        pv.outer_param_id = Some(r.outer_param_id.clone());
        // Live values for this row come from the inner node the routing
        // targets, not from `pv.name` (already overwritten with
        // `outer_param_id` above) or the enclosing group's own `node_id`
        // (empty — see `apply_live_values`).
        pv.live_source = Some((node.node_id.clone(), r.inner_param.clone()));
        out.push(pv);
    }
    out
}

/// The collapsed-face summary for a group carrying [`build_group_param_rows`]
/// rows — a compact "N params" chip drawn by the existing collapsed-summary
/// line (`NodeView::summary`), so a folded group with exposed inner params
/// still tells you it has knobs instead of going silent. `None` for a group
/// with no mirrored rows (renders exactly like any other empty collapsed
/// node) — no size threshold, collapse state is the only switch (D6).
pub(crate) fn group_param_summary(rows: &[ParamView]) -> Option<String> {
    (!rows.is_empty()).then(|| format!("{} params", rows.len()))
}

/// Component count for a multi-component param kind: `Color`/`Vec4` → 4,
/// `Vec3` → 3, `Vec2` → 2, `0` for scalar kinds. Mirrors the sidebar's
/// `GraphEditorParamKind::vec_components`, so the on-node channel editor and the
/// sidebar's agree on how many rows a colour/vector has.
pub(crate) fn vec_components(kind: crate::graph_view::ParamSnapshotKind) -> usize {
    use crate::graph_view::ParamSnapshotKind as K;
    match kind {
        K::Color | K::Vec4 => 4,
        K::Vec3 => 3,
        K::Vec2 => 2,
        _ => 0,
    }
}

/// Per-channel labels for the multi-component editor: `Color` reads RGBA,
/// vectors XYZW. Empty for scalar kinds. Mirrors the sidebar's
/// `GraphEditorParamKind::channel_labels`.
pub(crate) fn vec_channel_labels(
    kind: crate::graph_view::ParamSnapshotKind,
) -> &'static [&'static str] {
    use crate::graph_view::ParamSnapshotKind as K;
    match kind {
        K::Color => &["R", "G", "B", "A"],
        K::Vec2 => &["X", "Y"],
        K::Vec3 => &["X", "Y", "Z"],
        K::Vec4 => &["X", "Y", "Z", "W"],
        _ => &[],
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
pub(crate) fn category_header_color(cat: crate::graph_view::Category) -> Color32 {
    use crate::graph_view::Category as C;
    match cat {
        C::ColorAndTone => Color32::new(102, 76, 56, 255),
        C::BlurAndSharpen => Color32::new(56, 76, 102, 255),
        C::DistortAndWarp => Color32::new(87, 61, 102, 255),
        C::Stylize => Color32::new(102, 61, 87, 255),
        C::Generate => Color32::new(61, 92, 71, 255),
        C::Noise => Color32::new(56, 92, 92, 255),
        C::Mask => Color32::new(76, 76, 87, 255),
        C::Composite => Color32::new(66, 71, 107, 255),
        C::Geometry3D => Color32::new(76, 66, 107, 255),
        C::MaterialsAndLighting => Color32::new(97, 82, 56, 255),
        C::Particles2D => Color32::new(61, 87, 102, 255),
        C::Particles3D => Color32::new(56, 82, 107, 255),
        C::Control => Color32::new(92, 87, 56, 255),
        C::DetectionAndSampling => Color32::new(102, 66, 66, 255),
        C::MathAndConvert => Color32::new(76, 76, 76, 255),
        C::Routing => Color32::new(66, 76, 97, 255),
        C::FieldsAndCoordinates => Color32::new(61, 87, 87, 255),
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
    /// Replace the per-node preview sources for this frame (keyed by
    /// `preview_node_id`). The render host computes each visible node's atlas
    /// cell (or output texture) + UV and hands it here; `render` then paints the
    /// preview inline at the node's depth. See [`GraphCanvas::node_preview_src`].
    pub fn set_node_preview_src(
        &mut self,
        src: ahash::AHashMap<manifold_foundation::NodeId, (crate::node::TextureHandle, [f32; 4])>,
    ) {
        self.node_preview_src = src;
    }

    pub fn visible_node_thumbnails(
        &self,
        viewport: Rect,
    ) -> Vec<(manifold_foundation::NodeId, f32, f32, f32, f32)> {
        let mut out = Vec::new();
        let header = NODE_HEADER_HEIGHT * self.zoom;
        let pad = PREVIEW_PAD * self.zoom;
        for node in &self.nodes {
            let (Some(capture_id), Some((screen_w, screen_h))) =
                (node.preview_node_id.clone(), node.preview_screen)
            else {
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
            let strip_w = screen_w * self.zoom;
            let strip_h = screen_h * self.zoom;
            // Centre a narrower-than-full-width screen (portrait) in the band.
            let strip_x = sx + pad + (PREVIEW_IMG_W * self.zoom - strip_w) * 0.5;
            if strip_h > 1.0 {
                out.push((capture_id, strip_x, sy + header + pad, strip_w, strip_h));
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

    /// Set the project aspect ratio (output width / height) the per-node preview
    /// screens are sized to. Cheap no-op when unchanged. On a change, every
    /// previewable node's screen is resized in place — node heights move with it,
    /// but positions are preserved: an aspect change is rare (a resolution
    /// switch), and silently re-running auto-layout would scramble a hand-arranged
    /// graph. Call before [`Self::set_snapshot`] so the first layout of a level
    /// already uses the right node heights.
    pub fn set_preview_aspect(&mut self, aspect: f32) {
        if !(aspect.is_finite() && aspect > 0.0)
            || (aspect - self.preview_aspect).abs() < 1e-4
        {
            return;
        }
        self.preview_aspect = aspect;
        let screen = crate::graph_canvas::preview_screen_size(aspect);
        for node in &mut self.nodes {
            if node.preview_screen.is_some() {
                node.preview_screen = Some(screen);
            }
        }
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
                    if sn.type_id == GROUP_TYPE_ID {
                        // D6: a group's face rows are the live mirror of
                        // whichever card params route inside it — that set
                        // (and the values/wire-driven state on it) can change
                        // without this level's own topology hash moving
                        // (exposing/un-exposing an inner param is a manifest
                        // edit, not a wire/node edit), so it's recomputed here
                        // too, every push, not just on a full rebuild.
                        let rows = sn
                            .group
                            .as_deref()
                            .map(|body| build_group_param_rows(body, &snap.outer_routings))
                            .unwrap_or_default();
                        node.summary = group_param_summary(&rows);
                        node.params = rows;
                    } else {
                        // Param tooltips ride the snapshot (baked at translation),
                        // so `format_param_for_node` carries them straight through —
                        // no re-resolve, no carry-forward-by-index dance.
                        node.params = sn.parameters.iter().map(format_param_for_node).collect();
                        node.summary = node_summary(&sn.parameters);
                    }
                }
            }
            // Row *count* can change here too now (a group's mirrored rows
            // appear/disappear with expose state, not just topology), so the
            // row layout — and the heights/hit-rects derived from it — must
            // be rebuilt alongside the values, not just on a full rebuild.
            self.rebuild_rows();
            // Wire / outer-driven state is a topology property, but exposing a
            // param (adding an outer routing) can leave the level hash unchanged,
            // so recompute it here too — cheap, and it keeps the "↳ outer" hint
            // live without a full relayout.
            self.apply_driven_state(&snap.outer_routings);
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

        let preview_aspect = self.preview_aspect;
        let new_nodes: Vec<NodeView> = level_nodes
            .iter()
            .map(|n| {
                // D6: a group's face rows are the live mirror of whichever
                // card params route to a node inside it, not its own
                // (nonexistent) `parameters` — build those instead of the
                // ordinary per-node formatter.
                let (params, summary) = if n.type_id == GROUP_TYPE_ID {
                    let rows = n
                        .group
                        .as_deref()
                        .map(|body| build_group_param_rows(body, &snap.outer_routings))
                        .unwrap_or_default();
                    let summary = group_param_summary(&rows);
                    (rows, summary)
                } else {
                    (
                        n.parameters.iter().map(format_param_for_node).collect(),
                        node_summary(&n.parameters),
                    )
                };
                NodeView {
                id: n.id,
                node_id: n.node_id.clone(),
                handle: n.node_handle.clone(),
                title: n.title.clone(),
                params,
                // Filled from inputs/outputs/params once the struct is built
                // (they aren't in scope as slices inside this literal).
                rows: Vec::new(),
                summary,
                collapsed: self
                    .collapsed
                    .get(&n.id)
                    .copied()
                    .unwrap_or(self.default_collapsed),
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
                // The def stores a group's tint as a plain sRGB float array; the
                // canvas works in sRGB `Color32`. Convert at this boundary (no
                // gamma — `from_f32` is the byte↔float map, not `to_f32`).
                group_tint: n
                    .group
                    .as_ref()
                    .and_then(|g| g.tint)
                    .map(|t| Color32::from_f32(t[0], t[1], t[2], t[3])),
                tooltip: n.tooltip.clone(),
                preview_node_id: node_preview_target(n),
                preview_screen: node_preview_target(n)
                    .map(|_| crate::graph_canvas::preview_screen_size(preview_aspect)),
                wgsl_source: n.wgsl_source.clone(),
                // Filled by `rebuild_rows` below (needs the level's wires).
                hideable_ports: 0,
                revealed: false,
                is_render_scene: n.type_id == "node.render_scene",
                }
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
        // Expanded-body row layout (outputs / param+socket / leftover inputs) with
        // unused (unwired, unrevealed) sockets hidden. Needs `self.wires`, so it
        // runs after they're built.
        self.rebuild_rows();
        // Now that this level's wires are in `self.wires`, tag each param's
        // wire / outer-driven state (drives the read-only lockout + row hints).
        self.apply_driven_state(&snap.outer_routings);

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

    /// Tag every on-face param with its **wire-driven** and **outer-driven**
    /// state, the same two facts the sidebar showed:
    /// - **wire-driven** — a wire lands on this node's same-named scalar input
    ///   port (port-shadows-param), so the wire feeds the param each frame. The
    ///   row goes read-only and shows "← wired". Read from the current level's
    ///   `self.wires` (so a wire *inside* a group is seen when you're in it).
    /// - **outer-driven** — an outer performance-card slider routes into
    ///   `(node_handle, param)`; the row stays editable but shows "↳ <label>".
    ///   Read from the snapshot-global `outer_routings`.
    ///
    /// Byte-for-byte the app's `build_wire_driven_keys` / `build_outer_driven_map`
    /// join, but computed canvas-side from data the snapshot already carries — no
    /// new plumbing. Called from both `set_snapshot` paths (full rebuild + the
    /// param-only refresh), so exposing a param refreshes the hint without a
    /// relayout. Wire-driven wins when both apply (the wire short-circuits the
    /// binding apply path), matching the sidebar's precedence.
    fn apply_driven_state(&mut self, outer_routings: &[crate::graph_view::OuterParamRouting]) {
        // Split the borrow: read `wires`, mutate `nodes`.
        let Self { wires, nodes, .. } = self;
        // Node id → title, so a wire-driven param's hover hint (`driven_by`,
        // D5) can name its source without a second pass over `nodes` once the
        // mutable walk below starts. Collected (not borrowed) so it survives
        // past the immutable borrow of `nodes` this statement takes.
        let titles: ahash::AHashMap<u32, String> =
            nodes.iter().map(|n| (n.id, n.title.clone())).collect();
        for node in nodes.iter_mut() {
            // Group-face mirror rows (D6) already carry their own correct
            // `wire_driven` — computed by `build_group_param_rows` against the
            // wires of whatever *inner* level the target node actually lives
            // at, which is never `self.wires` here (the currently viewed
            // level) unless the target happens to live at the group's own
            // top level. Same-level `to_node == node.id` matching (below)
            // would either miss entirely or, worse, coincidentally hit an
            // unrelated wire landing on the group's own interface input of
            // the same structural id — so groups are skipped, not folded into
            // the general loop.
            if node.is_group {
                continue;
            }
            let handle = node.handle.clone();
            for p in &mut node.params {
                let feeding_wire =
                    wires.iter().find(|w| w.to_node == node.id && w.to_port == p.name);
                p.wire_driven = feeding_wire.is_some();
                p.driven_by = feeding_wire.map(|w| {
                    let src = titles.get(&w.from_node).map(String::as_str).unwrap_or("?");
                    format!("driven by {src}.{}", w.from_port)
                });
                p.outer_driver = handle.as_deref().and_then(|h| {
                    outer_routings
                        .iter()
                        .find(|r| r.node_handle == h && r.inner_param == p.name)
                        .map(|r| r.outer_label.clone())
                });
            }
        }
    }

    /// Recompute every node's expanded-body [`NodeRow`] layout from the current
    /// wires + per-node reveal state, hiding ports that carry no wire (unless the
    /// node is revealed). Sets each node's `rows` + `hidden_ports`. Reads
    /// `self.wires`, so call it after they're set. Cheap enough to re-run on a
    /// reveal-chip toggle (which doesn't change topology, so `set_snapshot`'s
    /// hash-gate wouldn't otherwise rebuild the rows).
    pub(crate) fn rebuild_rows(&mut self) {
        // Split the borrow: read `wires` / `revealed_ports`, mutate `nodes`.
        let Self {
            wires,
            nodes,
            revealed_ports,
            ..
        } = self;
        for node in nodes.iter_mut() {
            let reveal = revealed_ports.get(&node.id).copied().unwrap_or(false);
            let id = node.id;
            let out_wired = |name: &str| wires.iter().any(|w| w.from_node == id && w.from_port == name);
            let in_wired = |name: &str| wires.iter().any(|w| w.to_node == id && w.to_port == name);
            // Hide unwired sockets only once a sibling of the same kind is already
            // wired — you've shown your intent, so the rest is noise (a distributor
            // like Generator Input drops to its 2 used outputs). A fresh node with
            // nothing wired shows every socket so it can be connected in the first
            // place. `reveal` overrides and shows all.
            let any_out_wired = node.outputs.iter().any(|p| out_wired(&p.name));
            let any_in_wired = node.inputs.iter().any(|p| in_wired(&p.name));
            let output_visible: Vec<bool> = node
                .outputs
                .iter()
                .map(|p| reveal || !any_out_wired || out_wired(&p.name))
                .collect();
            let input_visible: Vec<bool> = node
                .inputs
                .iter()
                .map(|p| reveal || !any_in_wired || in_wired(&p.name))
                .collect();
            node.rows = compute_node_rows(
                &node.inputs,
                &node.outputs,
                &node.params,
                &output_visible,
                &input_visible,
            );
            // D7/D7a: splice the "+ Object" / "+ Light" gesture-button rows
            // onto `render_scene`'s face. `compute_node_rows` is generic
            // (knows nothing of node type), so this is a targeted post-pass.
            if node.is_render_scene {
                splice_render_scene_action_rows(&mut node.rows, &node.params);
            }
            // Reveal-independent count of sockets the node CAN hide (for the chip):
            // an unwired output whose sibling is wired, plus an unwired *leftover*
            // input (one that doesn't ride a param row) whose sibling is wired.
            let hideable_out = node
                .outputs
                .iter()
                .filter(|p| any_out_wired && !out_wired(&p.name))
                .count();
            let hideable_in = node
                .inputs
                .iter()
                .filter(|p| {
                    let shadowed = node.params.iter().any(|pr| pr.name == p.name);
                    !shadowed && any_in_wired && !in_wired(&p.name)
                })
                .count();
            node.hideable_ports = hideable_out + hideable_in;
            node.revealed = reveal;
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
        let scrubbing: Option<(u32, String)> = match self.drag.payload() {
            Some(CanvasDrag::ParamScrub {
                node_id,
                param_name,
                ..
            }) => Some((*node_id, param_name.clone())),
            _ => None,
        };
        // Sparkline samples gathered during the mutable node walk, applied to
        // `spark_history` afterwards (can't touch `self.spark_history` while
        // `self.nodes` is borrowed mutably).
        let mut spark_updates: Vec<(manifold_foundation::NodeId, f32)> = Vec::new();
        for node in &mut self.nodes {
            // Ordinary rows match against the enclosing node's own live
            // entry; group-face mirror rows (`live_source`, D6) carry their
            // own inner (node_id, param) key instead and are matched
            // per-row below, so a group with an empty `node_id` (it's
            // structural, not a live node) isn't skipped wholesale — only
            // its non-mirror rows (none, today) would have nothing to match.
            let node_vals = if node.node_id.is_empty() {
                None
            } else {
                by_id.get(&node.node_id)
            };
            if node_vals.is_none() && !node.params.iter().any(|p| p.live_source.is_some()) {
                continue;
            }
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
                let found = if let Some((src_id, src_name)) = &pv.live_source {
                    by_id
                        .get(src_id)
                        .and_then(|vals| vals.iter().find(|(name, _)| *name == src_name.as_str()))
                } else {
                    node_vals.and_then(|vals| vals.iter().find(|(name, _)| *name == pv.name))
                };
                let Some(&(_, value)) = found else {
                    continue;
                };
                let Some((value_str, fill)) = numeric_value_fill(pv.kind, value, pv.range) else {
                    continue;
                };
                pv.value = value_str;
                pv.fill = fill;
                pv.current_value = value;
                if let Some(scrub) = pv.scrub.as_mut() {
                    scrub.current_value = value;
                }
                if primary_fill.is_none()
                    && let Some(f) = fill
                {
                    primary_fill = Some(f);
                }
            }
            // Sparkline history is keyed by the node's own `node_id` — a
            // group's is empty, so a group-face row's live update doesn't
            // feed a (meaningless) empty-keyed trace.
            if !node.node_id.is_empty()
                && let Some(f) = primary_fill
            {
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
