//! `GraphEditCommand` — the node-graph editor surface's own command
//! vocabulary, lifted out of the 281-variant `PanelAction` god-enum
//! (UI Architecture Overhaul Phase 4.3).
//!
//! The graph-editor canvas and its right-sidebar inspector emit these instead
//! of `PanelAction`; the app translates each into the matching
//! `manifold_editing::commands::graph::*`, resolving the watched effect/
//! generator target + catalog default + scope at the boundary (exactly as the
//! old `PanelAction` arms did — the action stays intentionally context-free).
//!
//! Payloads carry the shared `NodeId` (from `manifold-foundation`) and UI-local
//! mirrors (`crate::types::ParamConvert` / `SerializedParamValue`) — the app
//! translates them to the engine types at the boundary. The layering win is
//! moving them off the shared god-enum onto the graph surface's *own* focused
//! type. Part of Phase 5's layering inversion: a UI-local command the app maps
//! to an engine command.
//!
//! Deliberately NOT here (see `docs/CANVAS_API_DESIGN.md` §3): the
//! `EffectMapping*` binding-edit family (a different command family —
//! `EditUserParamBindingCommand`) and the `Open*` window-open intents (emitted
//! from the main-window card, not the canvas). Those stay on `PanelAction`.

/// One edit to the watched node graph (or a UI flow that produces one).
#[derive(Debug, Clone, PartialEq)]
pub enum GraphEditCommand {
    /// Add a new node of `type_id` to the watched graph at the canvas
    /// center. Emitted by clicking an entry in the palette.
    AddGraphNode { type_id: String },
    /// Open the node picker over the canvas, anchored at `screen_pos`, to
    /// spawn the chosen node at `graph_pos`. Emitted by a double-click on
    /// empty canvas space. The app resolves the spawn into an
    /// `AddGraphNodeAt` once a node is picked.
    OpenNodePicker {
        screen_pos: (f32, f32),
        graph_pos: (f32, f32),
    },
    /// Add a new node of `type_id` at a specific `graph_pos`. Emitted after
    /// a node is chosen in the picker (the positioned sibling of
    /// `AddGraphNode`, which drops at a fixed canvas spot).
    AddGraphNodeAt {
        type_id: String,
        graph_pos: (f32, f32),
    },
    /// Connect an output port to an input port. Emitted by the
    /// wire-drag completion path on the canvas.
    ConnectPorts {
        from_node: u32,
        from_port: String,
        to_node: u32,
        to_port: String,
    },
    /// Remove a node from the watched graph plus every wire that
    /// touches it. Emitted by the canvas's delete-key handler.
    RemoveGraphNode { node_id: u32 },
    /// Disconnect the wire feeding `(to_node, to_port)`. The input
    /// side uniquely identifies the wire because each input port has
    /// at most one incoming wire. Emitted by clicking on an already-
    /// connected input port on the canvas.
    DisconnectPorts { to_node: u32, to_port: String },
    /// Revert the watched effect's graph to the bundled preset
    /// (`instance.graph = None`). Emitted by the "Reset to Default"
    /// button in the graph editor header when the card is diverged.
    RevertEffectGraph,
    /// Save to Library (PRESET_LIBRARY_DESIGN D4, P3): publish the watched
    /// graph's current effective definition as a new user-library entry.
    /// Opens the shared name-prompt text-input session anchored at the
    /// header button; the write happens on commit. Emitted by the "Save to
    /// Library" header button.
    SaveGraphToLibrary { anchor: (f32, f32, f32, f32) },
    /// Save to Project (PRESET_LIBRARY_DESIGN D4, P3): publish the watched
    /// graph's current effective definition as a new `origin: Saved`
    /// project-embedded preset, without retargeting the watched instance.
    /// Emitted by the "Save to Project" header button.
    SaveGraphToProject { anchor: (f32, f32, f32, f32) },
    /// Push to Library (PRESET_LIBRARY_DESIGN D3, P4): overwrite the
    /// watched instance's tracked user-library file with its current
    /// (diverged) definition — every OTHER instance still tracking that id
    /// picks it up via the existing hot-reload watcher. A factory/stock id
    /// has no user file to overwrite; the app-side handler falls back to
    /// opening the Save to Library (as new) prompt at `anchor` instead.
    /// Emitted by the "Push to Library" header button, shown only while
    /// diverged.
    PushGraphToLibrary { anchor: (f32, f32, f32, f32) },
    /// Update a node's editor position. Emitted by the canvas's
    /// node-drag completion path.
    MoveGraphNode { node_id: u32, new_pos: (f32, f32) },
    /// Re-position every node at `scope_path` in one undoable step. Emitted
    /// by the canvas's Tidy command (Cmd+L). Routed to `LayoutGraphNodesCommand`.
    RelayoutGraph {
        scope_path: Vec<u32>,
        positions: Vec<(u32, (f32, f32))>,
    },
    /// Set an inner-node parameter to a new value. Emitted by the
    /// right-sidebar inspector (Bool toggle, Enum cycle, Float/Int scrub) and
    /// by the canvas's on-face param scrub. `new_value` is already coerced to
    /// the inner param's kind, so the handler hands it straight to
    /// `SetGraphNodeParamCommand`.
    SetGraphNodeParam {
        node_id: u32,
        param_name: String,
        new_value: crate::types::SerializedParamValue,
    },
    /// A node-face numeric scrub session just ended (mouse-up). Emitted
    /// unconditionally by the canvas's `ParamScrub` release path — harmless
    /// for an ordinary (unbound) row, where the app has nothing tracked for
    /// it. Exists so the dispatch layer can close out a **card-bound**
    /// param's write-back gesture (`PARAM_TWO_WAY_BINDING_DESIGN.md` D1)
    /// with exactly one undo-worthy commit for the whole drag, instead of
    /// one per pointer-move — every `SetGraphNodeParam` during the drag
    /// already wrote the live (card) value, so there is nothing left to
    /// apply here, only to close the undo entry.
    EndGraphNodeParamScrub { node_id: u32, param_name: String },
    /// Set an **outer performance-card param** to a new value. Emitted by a
    /// scrub/click on a group box's face row (D6,
    /// `docs/SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md` §2): that row is a live
    /// mirror of an already-exposed card param, not an inner node's own row,
    /// so it's addressed the same way the card's own slider is — by the
    /// outer binding's stable `ParamId` string — rather than by
    /// `(node_id, param_name)`. Deliberately a *different* command from
    /// `SetGraphNodeParam`, staying context-free like every other
    /// `GraphEditCommand`: the app resolves the watched effect/generator
    /// target and re-dispatches this through the identical
    /// `PanelAction::ParamChanged` handler the card's own slider uses (the
    /// parity invariant — one value, three surfaces: card, group face, inner
    /// node face), rather than inventing a second write path.
    SetOuterParam {
        outer_param_id: String,
        new_value: f32,
    },
    /// Open a native folder picker for a path-like String param and set it to
    /// the chosen path. Emitted by the inspector's Browse button.
    BrowseGraphNodePath { node_id: u32, param_name: String },
    /// Open the inline text editor over a free-text String param's value cell.
    /// `anchor` is `(x, y, w, h)` in logical px; commit routes back through
    /// `SetGraphNodeParamCommand` with a String value.
    EditGraphNodeStringParam {
        node_id: u32,
        param_name: String,
        current: String,
        anchor: (f32, f32, f32, f32),
    },
    /// Open the multiline WGSL code editor over the selected `wgsl_compute`
    /// node's kernel source. Commit routes to `SetWgslSourceCommand`.
    EditGraphNodeWgsl {
        node_id: u32,
        current: String,
        anchor: (f32, f32, f32, f32),
    },
    /// Open the inline numeric type-in for a ranged param on the node face
    /// (UI_WIDGET_UNIFICATION P5d — the contract's `(ValueCell, DoubleClick)
    /// -> EditValue` row's last dead stop). `outer_param_id` mirrors the
    /// scrub-commit parity rule (D4/D6): `Some` means this is a group-face
    /// mirror row, so commit must write through `SetOuterParam`, never
    /// `SetGraphNodeParam`, on the SAME node the scrub would have.
    EditGraphNodeNumericParam {
        node_id: u32,
        param_name: String,
        current: f32,
        min: f32,
        max: f32,
        whole_numbers: bool,
        outer_param_id: Option<String>,
        anchor: (f32, f32, f32, f32),
    },
    /// Open the inline numeric editor over one cell of a `Table` param's grid.
    /// `rows` + `(row, col)` are stashed so commit rebuilds the one edited cell
    /// into a full `Table` value through `SetGraphNodeParamCommand`.
    EditGraphNodeTableCell {
        node_id: u32,
        param_name: String,
        row: usize,
        col: usize,
        current: f32,
        rows: Vec<Vec<f32>>,
        anchor: (f32, f32, f32, f32),
    },
    /// Collapse a set of nodes at `scope_path` into a single group node.
    /// Emitted by Ctrl+G on a canvas selection. Routed to `GroupNodesCommand`.
    GroupSelection {
        scope_path: Vec<u32>,
        node_ids: Vec<u32>,
        handle: String,
        centroid: (f32, f32),
    },
    /// Dissolve a group node at `scope_path` back into its level. Emitted by
    /// Ctrl+Shift+G on a selected group. Routed to `UngroupNodeCommand`.
    Ungroup { scope_path: Vec<u32>, group_id: u32 },
    /// Set (or clear) the accent colour of a group node at `scope_path`.
    /// Cosmetic only — `None` restores the default tint.
    SetGroupTint {
        scope_path: Vec<u32>,
        group_id: u32,
        tint: Option<[f32; 4]>,
    },
    /// Toggle whether an inner-graph param is exposed on the outer card.
    /// One variant for both Effect- and Generator-hosted graphs; the app
    /// resolves the watched `GraphTarget` and routes to
    /// `ToggleNodeParamExposeCommand`. `label`/`min`/`max`/`default_value`/
    /// `convert`/`value_labels` are the inner-node ParamDef metadata captured
    /// at panel-build time (kept off the renderer registry on the click path).
    ToggleNodeParamExpose {
        node_id: manifold_foundation::NodeId,
        /// Runtime (doc) id of the inner node at the current view depth — the
        /// SAME addressing key every other graph command uses (`n.id`). Always
        /// populated, unlike `node_id`, whose stable `NodeId` is empty for
        /// bundled-preset nodes. The app pairs this with the canvas scope so
        /// `ToggleNodeParamExposeCommand` can `descend_level` + match by id and
        /// reach a node nested inside a group. `node_id` stays for the mirror
        /// side's binding identity only.
        node_u32_id: u32,
        node_handle: String,
        inner_param: String,
        expose: bool,
        label: String,
        min: f32,
        max: f32,
        default_value: f32,
        convert: crate::types::ParamConvert,
        is_angle: bool,
        value_labels: Vec<String>,
    },
    /// Flip auto-gain/normalization on the editor's node-output preview pane.
    /// Routed to `ContentCommand::SetNodePreviewNormalize`. Node preview only —
    /// no undo, no model mutation.
    SetNodePreviewNormalize(bool),
    /// The "+ Object" one-click gesture (D7,
    /// `docs/SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md` §2): build a placeholder
    /// cube+material+transform group and wire it into `render_scene`'s next
    /// object slot. Emitted by the `NodeRow::Action(AddSceneObject)` button on
    /// the `render_scene` node face. Routed to `AddSceneObjectCommand`.
    /// `next_index` is the live `objects` count read straight off the node
    /// face at click time (the render_scene primitive's own defaults are
    /// private to `manifold-renderer`, unreachable from the command crate —
    /// see the command's doc comment).
    AddSceneObject {
        scope_path: Vec<u32>,
        render_scene_node_id: u32,
        next_index: u32,
        centroid: (f32, f32),
    },
    /// The "+ Light" one-click gesture (D7a): spawn a bare `node.light` with
    /// the D7a defaults (Sun, white, intensity 1.0, ~45° elevation,
    /// `cast_shadows` ON) and wire it into `render_scene`'s next light slot.
    /// Emitted by the `NodeRow::Action(AddSceneLight)` button. Routed to
    /// `AddSceneLightCommand`.
    AddSceneLight {
        scope_path: Vec<u32>,
        render_scene_node_id: u32,
        next_index: u32,
        pos: (f32, f32),
    },
}
