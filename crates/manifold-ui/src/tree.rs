use crate::node::*;
use crate::text::{HeuristicTextMeasure, TextMeasure};

/// Stacking tier for a top-level [`UITree::begin_region`] root —
/// `UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` D2. Render order is `(tier,
/// insertion)`: regions of a lower tier paint first (further back); within a
/// tier, insertion order breaks ties, matching today's build-order behavior.
/// So a `Chrome` region (the transport/header/footer frame) always wins over
/// a `Base` region (timeline/viewport/inspector) regardless of which one
/// built first this frame — "the footer can never lose to the inspector
/// again regardless of who builds first" (D2).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum ZTier {
    /// Timeline, viewport, inspector — the main content surfaces.
    Base = 0,
    /// Transport, header, footer — the always-visible frame.
    Chrome = 1,
    /// Popups/modals (the existing `OverlayId::Z_ORDER` registry becomes
    /// ordering *within* this tier).
    Overlay = 2,
    /// Drag ghosts, tooltips, toasts — topmost, and the only tier where
    /// `ALLOW_OVERFLOW` is expected to be legitimate (D3).
    Ghost = 3,
}

/// A region minted by [`UITree::begin_region`] — the token its content builds
/// under. `root` is the container node's id; pass `Some(token.root)` as the
/// parent for content built directly under it, or capture `tree.count()`
/// before calling the panel's existing (unchanged) `None`-rooted build and
/// sweep the result under `token.root` via
/// [`end_region`](UITree::end_region)/[`reparent_root_nodes`](UITree::reparent_root_nodes) —
/// the same idiom the inspector's own `ClipRegion` already uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RegionToken {
    pub root: NodeId,
}

/// Flat indexed node storage for the bitmap UI system.
///
/// INVARIANTS:
///   - id == index: node IDs are sequential from 0, equal to their array index.
///   - Structural operations (add_node) occur only during panel build phase.
///     Interaction frames use only set_* mutations (O(1), no allocation).
///   - Traversal order: depth-first pre-order matches insertion order.
///   - Clipping: hierarchical push/pop following the tree structure.
///
/// STORAGE LAYOUT (SoA):
///   nodes[]        — node data (style, bounds, flags, text)
///   parent_index[] — parent node (`None` for roots)
///   first_child[]  — first child (`None` if leaf)
///   next_sibling[] — next sibling (`None` if last)
///   last_child[]   — last child (`None` if leaf), for O(1) appending
pub struct UITree {
    nodes: Vec<UINode>,
    parent_index: Vec<Option<NodeId>>,
    first_child: Vec<Option<NodeId>>,
    next_sibling: Vec<Option<NodeId>>,
    last_child: Vec<Option<NodeId>>,
    /// Generation stamp per slot, parallel to `nodes`. Bumped from `gen_counter`
    /// each time a slot is minted, so a slot reused after truncate/clear+rebuild
    /// carries a fresh generation and ids minted against the old occupant no
    /// longer validate. See [`NodeId`].
    generations: Vec<u32>,
    /// Monotonic source of generation stamps. Starts at 1 — generation 0 is the
    /// reserved "no live node" stamp ([`NodeId::PLACEHOLDER`]), so it is never
    /// minted.
    gen_counter: u32,
    /// Durable [`WidgetId`] per slot, parallel to `nodes`. Unlike the generational
    /// `NodeId`, a node's `WidgetId` is the *same on every rebuild* as long as the
    /// build is structurally stable, so it survives the editor's per-frame
    /// clear+rebuild. Derived from the parent's `WidgetId` mixed with a stable
    /// sibling salt (the sibling index, or an explicit key). Used for hierarchy
    /// (a child salts off its parent's id) and for [`widget_of`](UITree::widget_of).
    widget_ids: Vec<WidgetId>,
    /// Durable component name per slot, parallel to `nodes`/`widget_ids`
    /// (`UI_AUTOMATION_DESIGN.md` D8/§3). `&'static str` only — panels register a
    /// literal like `"layer_header.mute"` via [`set_name`](UITree::set_name)
    /// right after building the node; *which row* it's on comes from the
    /// selector's structural query (`under_text`), never a per-row `String`
    /// allocation (the editor rebuilds its tree every frame, so that would be a
    /// per-frame alloc on the UI thread). `None` for the overwhelming majority
    /// of nodes — the automation dump (§3) reaches unnamed nodes via
    /// text/type/structure instead.
    names: Vec<Option<Box<str>>>,
    /// Per-slot count of children added so far, parallel to `nodes`. The auto
    /// sibling salt for the next child of a node — so siblings get 0, 1, 2, … in
    /// build order, deterministically reproduced on rebuild.
    child_counts: Vec<u32>,
    /// Count of root-level (parentless) nodes added so far — the auto salt source
    /// for roots, mirroring `child_counts` for a virtual root parent.
    root_count: u32,
    /// Per-slot flag: was this node *minted* as a root (parent_id `None`)?
    /// Parallel to `nodes`. Set once at mint and NEVER changed by
    /// [`reparent_root_nodes`](UITree::reparent_root_nodes) — so it records the
    /// root salt a node actually consumed, which [`truncate_from`](UITree::truncate_from)
    /// needs to continue the root-salt sequence correctly. Counting current
    /// `parent_index.is_none()` instead undercounts once a root has been
    /// reparented (the inspector wraps built subpanels under a ClipRegion),
    /// which made a partial rebuild re-issue an already-consumed root salt →
    /// two nodes deriving the same `WidgetId` (a silent widget→node map
    /// corruption in release; the double-click / mis-target that surfaced).
    root_minted: Vec<bool>,
    /// Reverse index: durable [`WidgetId`] → its live [`NodeId`] in the *current*
    /// build, for the interactive nodes only (the ones the input system can target
    /// for press / hover / focus). Rebuilt alongside the tree, so a lookup always
    /// returns a live id. This is how the input system resolves an identity it has
    /// held across frames back to a node it can act on.
    widget_to_node: ahash::AHashMap<WidgetId, NodeId>,
    count: usize,
    has_dirty: bool,
    /// Monotonic counter bumped on every *structural* change (add_node, clear,
    /// truncate_from) — never on in-place set_* mutations. Consumers that cache
    /// per-node-id data (e.g. the intent registry) repopulate only when this
    /// changes, so they stay correct across partial rebuilds without paying a
    /// per-frame cost. Cannot go stale: it lives at the mutation layer, so no
    /// structural change can bypass it.
    structure_version: u64,
    /// Text measurement available to the *build* path, so a panel can size a
    /// cell to its text while building (not just guess a fixed width). Defaults
    /// to the GPU-free [`HeuristicTextMeasure`] so the tree can always answer;
    /// the app installs a CoreText-accurate measurer via [`set_text_measure`].
    ///
    /// [`set_text_measure`]: UITree::set_text_measure
    text_measure: Box<dyn TextMeasure>,
    /// Every region minted by [`begin_region`](UITree::begin_region), in the
    /// order it was minted — `(tier, root)`. The render walk
    /// ([`traverse`](UITree::traverse), [`traverse_range`](UITree::traverse_range))
    /// visits these instead of scanning for root-parented nodes directly, in
    /// `(tier, insertion)` order — `UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` D1/D2.
    /// Pruned in [`truncate_from`](UITree::truncate_from) and
    /// [`clear`](UITree::clear) alongside the other per-node Vecs.
    regions: Vec<(ZTier, NodeId)>,
    /// Depth counter: >0 while a [`begin_region`](UITree::begin_region) call's
    /// subtree is still being built (before its matching
    /// [`end_region`](UITree::end_region) reparents it). While open, `mint`
    /// permits a `parent_id: None` node — the sanctioned "build flat, then
    /// sweep under the region" idiom already used by the inspector's own
    /// `ClipRegion` (`reparent_root_nodes`'s doc comment) — generalized here
    /// instead of hand-rolled per panel. A counter, not a bool, because two
    /// regions can be open sequentially without interleaving inside one build
    /// pass; nesting (a region opened while another is still open) is safe too.
    open_regions: u32,
}

const INITIAL_CAPACITY: usize = 512;

impl UITree {
    pub fn new() -> Self {
        Self {
            nodes: Vec::with_capacity(INITIAL_CAPACITY),
            parent_index: Vec::with_capacity(INITIAL_CAPACITY),
            first_child: Vec::with_capacity(INITIAL_CAPACITY),
            next_sibling: Vec::with_capacity(INITIAL_CAPACITY),
            last_child: Vec::with_capacity(INITIAL_CAPACITY),
            generations: Vec::with_capacity(INITIAL_CAPACITY),
            gen_counter: 1,
            widget_ids: Vec::with_capacity(INITIAL_CAPACITY),
            names: Vec::with_capacity(INITIAL_CAPACITY),
            child_counts: Vec::with_capacity(INITIAL_CAPACITY),
            root_count: 0,
            root_minted: Vec::with_capacity(INITIAL_CAPACITY),
            widget_to_node: ahash::AHashMap::with_capacity(INITIAL_CAPACITY),
            count: 0,
            has_dirty: false,
            structure_version: 0,
            text_measure: Box::new(HeuristicTextMeasure),
            regions: Vec::with_capacity(16),
            open_regions: 0,
        }
    }

    pub fn count(&self) -> usize {
        self.count
    }

    // ── Text measurement (build path) ───────────────────────────────

    /// Install the measurer the build path uses for size-to-content. The app
    /// calls this once with a CoreText-accurate measurer; until then (and in
    /// tests) the tree falls back to the always-on [`HeuristicTextMeasure`].
    pub fn set_text_measure(&mut self, measure: Box<dyn TextMeasure>) {
        self.text_measure = measure;
    }

    /// Measure `text` at build time. Available wherever a `&UITree` is, so a
    /// panel's `build()` can size a cell to its content.
    pub fn measure_text(&self, text: &str, font_size: u16, weight: FontWeight) -> Vec2 {
        self.text_measure.measure_text(text, font_size, weight)
    }

    /// Measured pixel width of `text` — the common case of [`measure_text`].
    ///
    /// [`measure_text`]: UITree::measure_text
    pub fn text_width(&self, text: &str, font_size: u16, weight: FontWeight) -> f32 {
        self.text_measure.measure_text(text, font_size, weight).x
    }

    /// The installed measurer, for callers that drive a layout pass before
    /// mutating the tree (the Chrome API solves with this, then applies the
    /// result through `&mut self`). Borrows end before any mutation begins.
    pub fn measurer(&self) -> &dyn TextMeasure {
        &*self.text_measure
    }

    pub fn has_dirty(&self) -> bool {
        self.has_dirty
    }

    /// Current structural generation — see [`UITree::structure_version`].
    pub fn structure_version(&self) -> u64 {
        self.structure_version
    }

    // ── Node creation ───────────────────────────────────────────────

    pub fn add_node(
        &mut self,
        parent_id: Option<NodeId>,
        bounds: Rect,
        node_type: UINodeType,
        style: UIStyle,
        text: Option<&str>,
        extra_flags: UIFlags,
    ) -> NodeId {
        self.mint(parent_id, bounds, node_type, style, text, extra_flags, None)
    }

    /// Like [`add_node`](UITree::add_node) but pins this node's durable
    /// [`WidgetId`] to an explicit `key` instead of its sibling index. Use it for
    /// identity-bearing interactive nodes whose position among siblings can shift
    /// between rebuilds (e.g. a row control when an earlier row grows a drawer) —
    /// the key keeps the node's identity stable regardless of order. Keys only
    /// need to be unique among siblings of the same parent.
    pub fn add_node_keyed(
        &mut self,
        parent_id: Option<NodeId>,
        bounds: Rect,
        node_type: UINodeType,
        style: UIStyle,
        text: Option<&str>,
        extra_flags: UIFlags,
        key: u64,
    ) -> NodeId {
        self.mint(
            parent_id,
            bounds,
            node_type,
            style,
            text,
            extra_flags,
            Some(key),
        )
    }

    /// High bit set on an explicit-key salt so it can never collide with an auto
    /// sibling-index salt (sibling indices are small, well under 2^63).
    const EXPLICIT_KEY_FLAG: u64 = 1 << 63;

    #[allow(clippy::too_many_arguments)]
    fn mint(
        &mut self,
        parent_id: Option<NodeId>,
        bounds: Rect,
        node_type: UINodeType,
        style: UIStyle,
        text: Option<&str>,
        extra_flags: UIFlags,
        key: Option<u64>,
    ) -> NodeId {
        // D4 — the only sanctioned way to root a top-level (parent: None)
        // subtree is inside an open `begin_region`/`end_region` bracket
        // (`open_regions > 0`): either `begin_region`'s own mint of the
        // region container, or a panel building its (still `None`-rooted)
        // content in between, later swept under the region by `end_region`.
        // A `None`-parented node minted with no region open is exactly the
        // per-panel hand-clip bug class `UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md`
        // D1 exists to kill — see that doc before adding one back.
        //
        // `cfg(not(test))`: gated out of `manifold-ui`'s OWN unit tests, which
        // legitimately build a panel on a bare `UITree::new()` in isolation
        // (never touching `begin_region` — that wrapping is `ui_root.rs`'s
        // job, one layer up). `cfg(test)` is per-compilation-unit, so this
        // still fires for `manifold-app` — the real app (debug build) and
        // its own test/gate-scene binaries, where every panel DOES build
        // through the now-fully-migrated `UIRoot::build()`. The D4
        // structural unit test (`manifold-ui`, walking a *built* tree after
        // the fact) is the enforcement that still applies inside
        // `manifold-ui`'s own suite.
        #[cfg(not(test))]
        debug_assert!(
            parent_id.is_some() || self.open_regions > 0,
            "root-parented node minted outside an open UITree::begin_region — \
             UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md D1/D4. Wrap this subtree's build \
             in begin_region(...)/end_region(...) instead of rooting it at the tree."
        );
        // Mint a fresh generation for this slot. `gen_counter` never yields 0
        // (reserved for PLACEHOLDER); skip it on the wrap.
        let generation = self.gen_counter;
        self.gen_counter = self.gen_counter.wrapping_add(1);
        if self.gen_counter == 0 {
            self.gen_counter = 1;
        }
        let id = NodeId::from_parts(self.count as u32, generation);

        // Durable WidgetId: the parent's id mixed with a stable salt. An explicit
        // key (namespaced by EXPLICIT_KEY_FLAG) overrides the sibling index, so the
        // same logical widget resolves to the same WidgetId across rebuilds.
        let parent_widget = match parent_id {
            Some(p) => self.widget_ids[p.index()],
            None => WidgetId::ROOT,
        };
        let salt = match key {
            Some(k) => Self::EXPLICIT_KEY_FLAG | k,
            None => match parent_id {
                Some(p) => {
                    let pi = p.index();
                    let s = self.child_counts[pi] as u64;
                    self.child_counts[pi] += 1;
                    s
                }
                None => {
                    let s = self.root_count as u64;
                    self.root_count += 1;
                    s
                }
            },
        };
        let widget_id = parent_widget.with(salt);

        let node = UINode {
            id,
            parent_id,
            bounds,
            node_type,
            flags: UIFlags::VISIBLE | UIFlags::DIRTY | extra_flags,
            style,
            text: text.map(String::from),
            texture: None,
            draw_order: self.count as i32,
        };

        self.nodes.push(node);
        self.parent_index.push(parent_id);
        self.first_child.push(None);
        self.next_sibling.push(None);
        self.last_child.push(None);
        self.generations.push(generation);
        self.widget_ids.push(widget_id);
        self.names.push(None);
        self.child_counts.push(0);
        self.root_minted.push(parent_id.is_none());

        // Only interactive nodes are ever the target of press / hover / focus, so
        // only they need a reverse lookup. Keeping the map interactive-only also
        // makes a collision meaningful — two interactive widgets sharing an id is a
        // real bug (a duplicate explicit key), caught here in debug.
        if extra_flags.contains(UIFlags::INTERACTIVE) {
            let prev = self.widget_to_node.insert(widget_id, id);
            debug_assert!(
                prev.is_none(),
                "two interactive nodes share WidgetId {widget_id:?} — duplicate explicit key \
                 among siblings, or a sibling-salt collision"
            );
        }

        self.link_child(id, parent_id);
        self.count += 1;
        self.has_dirty = true;
        self.structure_version = self.structure_version.wrapping_add(1);
        id
    }

    fn link_child(&mut self, child_id: NodeId, parent_id: Option<NodeId>) {
        let Some(parent) = parent_id else {
            return;
        };
        let pid = parent.index();
        if self.first_child[pid].is_none() {
            self.first_child[pid] = Some(child_id);
        } else {
            let last = self.last_child[pid].expect("last_child set when first_child set").index();
            self.next_sibling[last] = Some(child_id);
        }
        self.last_child[pid] = Some(child_id);
    }

    /// Add a panel node (non-interactive rect).
    pub fn add_panel(
        &mut self,
        parent_id: Option<NodeId>,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        style: UIStyle,
    ) -> NodeId {
        self.add_node(
            parent_id,
            Rect::new(x, y, w, h),
            UINodeType::Panel,
            style,
            None,
            UIFlags::empty(),
        )
    }

    /// Add an interactive button node.
    #[allow(clippy::too_many_arguments)]
    pub fn add_button(
        &mut self,
        parent_id: Option<NodeId>,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        style: UIStyle,
        text: &str,
    ) -> NodeId {
        self.add_node(
            parent_id,
            Rect::new(x, y, w, h),
            UINodeType::Button,
            style,
            Some(text),
            UIFlags::INTERACTIVE,
        )
    }

    /// Add an interactive button whose durable [`WidgetId`] is pinned to an
    /// explicit `key` (see [`add_node_keyed`](UITree::add_node_keyed)) — for
    /// controls whose sibling position can shift between rebuilds.
    #[allow(clippy::too_many_arguments)]
    pub fn add_button_keyed(
        &mut self,
        parent_id: Option<NodeId>,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        style: UIStyle,
        text: &str,
        key: u64,
    ) -> NodeId {
        self.add_node_keyed(
            parent_id,
            Rect::new(x, y, w, h),
            UINodeType::Button,
            style,
            Some(text),
            UIFlags::INTERACTIVE,
            key,
        )
    }

    /// Add a non-interactive image node (PRESET_LIBRARY_DESIGN P6, D7):
    /// draws `texture` — a handle the renderer resolves against its
    /// registered-image cache (populated once, off the per-frame path, by
    /// decoding a saved thumbnail PNG) — filling the rect, rounded to
    /// `corner_radius`. `UINodeType::Image` and `UINode.texture` existed as
    /// dead Unity-port scaffolding (zero consumers) until this; see
    /// `ui_renderer.rs`'s tree walk for the drawing side. Non-interactive by
    /// design — an interactive sibling (e.g. the browser cell's own button)
    /// drawn in the same rect keeps click/hover handling unchanged.
    pub fn add_image(
        &mut self,
        parent_id: Option<NodeId>,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        corner_radius: f32,
        texture: TextureHandle,
    ) -> NodeId {
        let id = self.add_node(
            parent_id,
            Rect::new(x, y, w, h),
            UINodeType::Image,
            UIStyle {
                corner_radius,
                ..UIStyle::default()
            },
            None,
            UIFlags::empty(),
        );
        self.nodes[id.index()].texture = Some(texture);
        id
    }

    /// Add a text label. Takes explicit width (fixes Unity's width=0 bug).
    #[allow(clippy::too_many_arguments)]
    pub fn add_label(
        &mut self,
        parent_id: Option<NodeId>,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        text: &str,
        style: UIStyle,
    ) -> NodeId {
        self.add_node(
            parent_id,
            Rect::new(x, y, w, h),
            UINodeType::Label,
            style,
            Some(text),
            UIFlags::empty(),
        )
    }

    /// Add an interactive slider node.
    pub fn add_slider(
        &mut self,
        parent_id: Option<NodeId>,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        style: UIStyle,
    ) -> NodeId {
        self.add_node(
            parent_id,
            Rect::new(x, y, w, h),
            UINodeType::Slider,
            style,
            None,
            UIFlags::INTERACTIVE,
        )
    }

    // ── Node access (O(1)) ──────────────────────────────────────────

    /// The current id for a slot index — index plus the slot's live generation.
    /// The way external code (and internal index-based passes) turn a raw index
    /// into a validatable id. Returns [`NodeId::PLACEHOLDER`] for an out-of-range
    /// index, so the result never matches a live node.
    #[inline]
    pub fn id_at(&self, index: usize) -> NodeId {
        match self.generations.get(index) {
            Some(&generation) => NodeId::from_parts(index as u32, generation),
            None => NodeId::PLACEHOLDER,
        }
    }

    /// Whether `id` refers to a live node: its index is in range AND the slot's
    /// generation matches. A stale id (slot reused since the id was minted) fails
    /// here, which is what turns every accessor below into a safe no-op for it.
    #[inline]
    pub fn is_live(&self, id: NodeId) -> bool {
        let idx = id.index();
        idx < self.count && self.generations[idx] == id.generation()
    }

    /// `None` if `id` is stale or invalid — the same inertness every other
    /// accessor here gives a stale id (`get_bounds` → `ZERO`, `set_style` →
    /// no-op). Callers that cache a `NodeId` across a rebuild must handle
    /// `None`; a stale id must never be able to reach `&self.nodes[..]`.
    pub fn get_node(&self, id: NodeId) -> Option<&UINode> {
        self.is_live(id).then(|| &self.nodes[id.index()])
    }

    /// Read-only view of every node in insertion order (`NodeId` == index).
    /// For headless inspection (the UI snapshot harness' tree dump); the
    /// renderer and hit-test use the typed accessors above.
    pub fn nodes(&self) -> &[UINode] {
        &self.nodes
    }

    /// `None` if `id` is stale or invalid — see [`Self::get_node`].
    pub fn get_node_mut(&mut self, id: NodeId) -> Option<&mut UINode> {
        self.is_live(id).then(|| &mut self.nodes[id.index()])
    }

    pub fn get_bounds(&self, id: NodeId) -> Rect {
        if !self.is_live(id) {
            return Rect::ZERO;
        }
        self.nodes[id.index()].bounds
    }

    // ── Batch offset (composite panel build) ────────────────────────

    /// Offset Y position of a range of nodes by array index.
    /// Called during initial build to position sub-panels.
    pub fn offset_nodes(&mut self, from_index: usize, count: usize, dy: f32) {
        let end = (from_index + count).min(self.count);
        for i in from_index..end {
            self.nodes[i].bounds.y += dy;
        }
    }

    // ── Regions (UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md D1–D4) ────────────

    /// The ONLY sanctioned way to root a top-level subtree. Mints a container
    /// node at `rect` carrying `CLIPS_CHILDREN` by construction — unless
    /// `extra_flags` carries `ALLOW_OVERFLOW` (D3), the `Ghost`-tier
    /// drag-ghost/toast escape hatch — and registers `(tier, root)` in the
    /// tree's region list, which the render walk
    /// ([`traverse`](Self::traverse), [`traverse_range`](Self::traverse_range))
    /// visits instead of scanning for root-parented nodes directly.
    ///
    /// `label` names the region for debugging/automation (`set_name`) — not
    /// currently read by any consumer, kept `&'static str` (not `String`) so
    /// a future one costs nothing on the per-frame rebuild path.
    ///
    /// `extra_flags` is normally `UIFlags::empty()`; pass `ALLOW_OVERFLOW`
    /// for the `Ghost`-tier escape hatch (D3) — the design doc's committed
    /// signature (§3) omits this parameter but its own doc comment on
    /// `begin_region` requires it ("unless ALLOW_OVERFLOW is passed in
    /// extra_flags"), and D3's opt-out cannot exist without it. Resolved
    /// here in the 4-param direction the doc's own prose specifies; see the
    /// P1 report.
    ///
    /// Opens the `open_regions` bracket so the panel content built between
    /// this call and the matching [`end_region`](Self::end_region) may itself
    /// mint `None`-parented nodes without tripping `mint`'s D4 debug
    /// assertion — capture `tree.count()` right after this call, build the
    /// panel exactly as before (still `None`-rooted internally), then call
    /// `end_region(token, that_start)` to sweep it under `token.root`. This
    /// generalizes the inspector's own pre-existing `ClipRegion` +
    /// `reparent_root_nodes` idiom into the one mechanism every top-level
    /// panel uses.
    pub fn begin_region(
        &mut self,
        rect: Rect,
        tier: ZTier,
        label: &'static str,
        extra_flags: UIFlags,
    ) -> RegionToken {
        self.open_regions += 1;
        let clips = if extra_flags.contains(UIFlags::ALLOW_OVERFLOW) {
            extra_flags
        } else {
            extra_flags | UIFlags::CLIPS_CHILDREN
        };
        let root = self.mint(None, rect, UINodeType::ClipRegion, UIStyle::default(), None, clips, None);
        self.set_name(root, label);
        self.regions.push((tier, root));
        RegionToken { root }
    }

    /// Close the bracket [`begin_region`](Self::begin_region) opened: sweeps
    /// every still-`None`-parented node in `[content_start, tree.count())`
    /// under `token.root` (via [`reparent_root_nodes`](Self::reparent_root_nodes) —
    /// a no-op for any node a nested region already claimed) and closes the
    /// `open_regions` bracket. `content_start` is `tree.count()` captured
    /// right after the matching `begin_region` call, so the region's own
    /// container node (minted before that capture) is never re-swept.
    pub fn end_region(&mut self, token: RegionToken, content_start: usize) {
        let count = self.count.saturating_sub(content_start);
        self.reparent_root_nodes(content_start, count, token.root);
        self.open_regions = self.open_regions.saturating_sub(1);
    }

    /// D4's second enforcement leg: true iff every root-parented node
    /// (`parent_index[i].is_none()`) is a registered region — i.e. no
    /// top-level subtree escaped `begin_region`. `mint`'s debug assertion
    /// (the first leg) catches this at the moment a stray root is minted,
    /// but only in non-test builds of a `manifold-ui` dependency
    /// (`UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` D4); this walks a tree AFTER
    /// the fact, unconditionally, so both `manifold-ui`'s own suite and
    /// `manifold-app`'s can assert it directly against a real, fully-built
    /// tree.
    pub fn all_roots_are_regions(&self) -> bool {
        (0..self.count).all(|i| {
            self.parent_index[i].is_some() || self.regions.iter().any(|&(_, root)| root.index() == i)
        })
    }

    /// Reparent all root-level nodes (parent_id == -1) in the index range
    /// [from_index..from_index+count) under the given parent.
    /// Used by the inspector to wrap built sub-panel nodes under a ClipRegion.
    pub fn reparent_root_nodes(&mut self, from_index: usize, count: usize, new_parent: NodeId) {
        let end = (from_index + count).min(self.count);
        for i in from_index..end {
            if self.parent_index[i].is_none() {
                self.parent_index[i] = Some(new_parent);
                self.link_child(self.id_at(i), Some(new_parent));
            }
        }
    }

    /// Offset the Y position of node `id` and all its descendants by `dy`.
    /// Uses the existing first_child / next_sibling linked list — no allocation.
    /// Nesting depth is bounded by tree depth (max ~2 in practice).
    pub fn offset_node_and_children(&mut self, id: NodeId, dy: f32) {
        if !self.is_live(id) {
            return;
        }
        let idx = id.index();
        self.nodes[idx].bounds.y += dy;
        self.nodes[idx].flags |= UIFlags::DIRTY;
        self.has_dirty = true;

        let mut child = self.first_child[idx];
        while let Some(c) = child {
            self.offset_node_and_children(c, dy);
            child = self.next_sibling[c.index()];
        }
    }

    // ── Mutation (O(1), marks dirty) ────────────────────────────────

    pub fn set_bounds(&mut self, id: NodeId, bounds: Rect) {
        if !self.is_live(id) {
            return;
        }
        let idx = id.index();
        if self.nodes[idx].bounds == bounds {
            return;
        }
        self.nodes[idx].bounds = bounds;
        self.nodes[idx].flags |= UIFlags::DIRTY;
        self.has_dirty = true;
    }

    pub fn set_style(&mut self, id: NodeId, style: UIStyle) {
        if !self.is_live(id) {
            return;
        }
        let idx = id.index();
        if self.nodes[idx].style == style {
            return;
        }
        self.nodes[idx].style = style;
        self.nodes[idx].flags |= UIFlags::DIRTY;
        self.has_dirty = true;
    }

    pub fn set_text(&mut self, id: NodeId, text: &str) {
        if !self.is_live(id) {
            return;
        }
        let idx = id.index();
        if self.nodes[idx].text.as_deref() == Some(text) {
            return;
        }
        self.nodes[idx].text = Some(text.to_string());
        self.nodes[idx].flags |= UIFlags::DIRTY;
        self.has_dirty = true;
    }

    pub fn set_flag(&mut self, id: NodeId, flag: UIFlags) {
        if !self.is_live(id) {
            return;
        }
        let idx = id.index();
        if self.nodes[idx].flags.contains(flag) {
            return;
        }
        self.nodes[idx].flags |= flag | UIFlags::DIRTY;
        self.has_dirty = true;
    }

    pub fn clear_flag(&mut self, id: NodeId, flag: UIFlags) {
        if !self.is_live(id) {
            return;
        }
        let idx = id.index();
        if !self.nodes[idx].flags.intersects(flag) {
            return;
        }
        self.nodes[idx].flags.remove(flag);
        self.nodes[idx].flags |= UIFlags::DIRTY;
        self.has_dirty = true;
    }

    pub fn has_flag(&self, id: NodeId, flag: UIFlags) -> bool {
        if !self.is_live(id) {
            return false;
        }
        self.nodes[id.index()].flags.contains(flag)
    }

    pub fn set_visible(&mut self, id: NodeId, visible: bool) {
        if visible {
            self.set_flag(id, UIFlags::VISIBLE);
        } else {
            self.clear_flag(id, UIFlags::VISIBLE);
        }
    }

    // ── Hit testing (O(n × depth)) ─────────────────────────────────

    /// Find the topmost interactive, visible, non-disabled node at `pos`.
    /// Returns `None` if nothing hit. Walk reverse insertion order.
    pub fn hit_test(&self, pos: Vec2) -> Option<NodeId> {
        for i in (0..self.count).rev() {
            let n = &self.nodes[i];
            let required = UIFlags::VISIBLE | UIFlags::INTERACTIVE;
            if !n.flags.contains(required) {
                continue;
            }
            if n.flags.contains(UIFlags::DISABLED) {
                continue;
            }
            if !n.bounds.contains(pos) {
                continue;
            }
            if !self.is_inside_clip_ancestors(i, pos) {
                continue;
            }
            return Some(n.id);
        }
        None
    }

    /// Walk up the parent chain to verify the point is inside all
    /// ClipsChildren ancestor bounds.
    fn is_inside_clip_ancestors(&self, index: usize, pos: Vec2) -> bool {
        let mut pid = self.parent_index[index];
        while let Some(p) = pid {
            let p = p.index();
            if self.nodes[p].flags.contains(UIFlags::CLIPS_CHILDREN)
                && !self.nodes[p].bounds.contains(pos)
            {
                return false;
            }
            pid = self.parent_index[p];
        }
        true
    }

    // ── Dirty tracking ─────────────────────────────────────────────

    pub fn mark_all_dirty(&mut self) {
        for i in 0..self.count {
            self.nodes[i].flags |= UIFlags::DIRTY;
        }
        self.has_dirty = true;
    }

    pub fn clear_dirty(&mut self) {
        for i in 0..self.count {
            self.nodes[i].flags.remove(UIFlags::DIRTY);
        }
        self.has_dirty = false;
    }

    /// Check if any node in [start, end) has the DIRTY flag.
    pub fn has_dirty_in_range(&self, start: usize, end: usize) -> bool {
        let end = end.min(self.count);
        for i in start..end {
            if self.nodes[i].flags.contains(UIFlags::DIRTY) {
                return true;
            }
        }
        false
    }

    /// True if any DIRTY node in [start, end) lies OUTSIDE every range in
    /// `covered`. The panel cache uses this to spot dirt in a panel's chrome —
    /// nodes (tab strip, cog/Collapse, scrollbar) that sit in no sub-region and
    /// so are invisible to the incremental sub-region repaint path. `covered`
    /// is small (one range per card/chrome sub-panel) and the scan short-circuits
    /// on the first uncovered dirty node, so this is the same order as the
    /// per-frame `has_dirty_in_range` scans it sits beside.
    pub fn has_dirty_outside_ranges(
        &self,
        start: usize,
        end: usize,
        covered: &[(usize, usize)],
    ) -> bool {
        let end = end.min(self.count);
        for i in start..end {
            if self.nodes[i].flags.contains(UIFlags::DIRTY)
                && !covered.iter().any(|&(s, e)| i >= s && i < e)
            {
                return true;
            }
        }
        false
    }

    /// Clear DIRTY flag on nodes in [start, end) and recompute global has_dirty.
    pub fn clear_dirty_range(&mut self, start: usize, end: usize) {
        let end = end.min(self.count);
        for i in start..end {
            self.nodes[i].flags.remove(UIFlags::DIRTY);
        }
        self.has_dirty = (0..self.count).any(|i| self.nodes[i].flags.contains(UIFlags::DIRTY));
    }

    // ── Rendering traversal (recursive DFS) ─────────────────────────

    /// Walk the tree in DFS order and call the visitor for each visible node.
    /// The visitor receives: (node, push_clip, pop_clip) signals.
    ///
    /// Root-level order is `(tier, insertion)` over the registered region
    /// list (`UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` D1/D2), not raw array
    /// index — a `Chrome` region always paints after (on top of) a `Base`
    /// region regardless of which one built first this frame. Post-D4
    /// migration every root IS a region, so this and the old "scan for
    /// `parent_index[i].is_none()`" behavior agree on WHICH nodes are
    /// visited; they differ only in order, and only across tiers.
    pub fn traverse<F>(&self, mut visitor: F)
    where
        F: FnMut(TraversalEvent),
    {
        self.traverse_regions(0, usize::MAX, &mut visitor);
    }

    /// Walk only region roots in `[start, end)` and their subtrees, in
    /// `(tier, insertion)` order. Used for overlay rendering and per-panel
    /// cache re-render — avoids traversing the entire tree when only a small
    /// range of nodes needs to be drawn. See [`traverse`](Self::traverse)'s
    /// doc for the ordering rationale.
    pub fn traverse_range<F>(&self, start: usize, end: usize, mut visitor: F)
    where
        F: FnMut(TraversalEvent),
    {
        self.traverse_regions(start, end, &mut visitor);
    }

    /// Shared implementation of [`traverse`](Self::traverse) and
    /// [`traverse_range`](Self::traverse_range): visit every registered
    /// region whose root index falls in `[start, end)`, tier-ascending
    /// (`Base` first / furthest back, `Ghost` last / topmost), insertion
    /// order breaking ties within a tier. Four linear passes over the
    /// (small, ~10-entry) region list — no sort, no allocation, per the
    /// hot-path discipline the design commits to (§3).
    fn traverse_regions<F>(&self, start: usize, end: usize, visitor: &mut F)
    where
        F: FnMut(TraversalEvent),
    {
        let end = end.min(self.count);
        for tier in [ZTier::Base, ZTier::Chrome, ZTier::Overlay, ZTier::Ghost] {
            for &(t, root) in &self.regions {
                if t != tier {
                    continue;
                }
                let idx = root.index();
                if idx >= start && idx < end && self.is_live(root) {
                    self.traverse_subtree(idx, visitor);
                }
            }
        }
    }

    /// Flat sequential iteration over `[start, end)` — visits ALL visible nodes
    /// regardless of parent_index. Nodes are stored in DFS pre-order (insertion
    /// order), so sequential iteration produces correct draw order.
    ///
    /// Handles clip regions by tracking which CLIPS_CHILDREN ancestors are active.
    /// Used for sub-region rendering where nodes have been reparented and
    /// `traverse_range` (which requires parent == -1) would skip them.
    ///
    /// When `dirty_only` is true, only emits `Node` events for dirty nodes,
    /// but always processes clip push/pop for correct clipping of dirty children.
    pub fn traverse_flat_range<F>(&self, start: usize, end: usize, dirty_only: bool, mut visitor: F)
    where
        F: FnMut(TraversalEvent),
    {
        let end = end.min(self.count);
        // Stack of (node_index, bounds) for active CLIPS_CHILDREN ancestors.
        let mut clip_stack: Vec<(usize, Rect)> = Vec::new();

        // Pre-push ancestor clip regions that are outside this range.
        // Without this, sub-region rendering (e.g. a single effect card)
        // misses the parent ClipRegion node and draws without clip context,
        // causing content to render outside its scroll container.
        if start < self.count {
            let mut ancestors: Vec<(usize, Rect)> = Vec::new();
            let mut idx = self.parent_index[start];
            while let Some(node_id) = idx {
                let node = &self.nodes[node_id.index()];
                if node.flags.contains(UIFlags::CLIPS_CHILDREN) {
                    ancestors.push((node_id.index(), node.bounds));
                }
                idx = self.parent_index[node_id.index()];
            }
            // Push outermost first (reverse order of discovery).
            for &(ci, bounds) in ancestors.iter().rev() {
                visitor(TraversalEvent::PushClip(bounds));
                clip_stack.push((ci, bounds));
            }
        }

        for i in start..end {
            let node = &self.nodes[i];
            if !node.flags.contains(UIFlags::VISIBLE) {
                continue;
            }

            // Pop clip regions for ancestors we've moved past.
            // A clip owner at index `c` is still active if `i` is a descendant.
            // Check by walking the parent chain of `i` — if no parent leads to
            // `c`, we've left `c`'s subtree.
            while let Some(&(clip_idx, _)) = clip_stack.last() {
                if self.is_ancestor_of(clip_idx, i) {
                    break;
                }
                clip_stack.pop();
                visitor(TraversalEvent::PopClip);
            }

            // Emit the node (skip non-dirty in dirty_only mode).
            if !dirty_only || node.flags.contains(UIFlags::DIRTY) {
                visitor(TraversalEvent::Node(node));
            }

            // Push clip if this node clips children.
            if node.flags.contains(UIFlags::CLIPS_CHILDREN) {
                visitor(TraversalEvent::PushClip(node.bounds));
                clip_stack.push((i, node.bounds));
            }
        }

        // Pop remaining clip regions.
        for _ in &clip_stack {
            visitor(TraversalEvent::PopClip);
        }
    }

    /// Check if `ancestor` is an ancestor of `descendant` by walking the parent chain.
    /// Returns false if `ancestor == descendant`.
    fn is_ancestor_of(&self, ancestor: usize, descendant: usize) -> bool {
        let mut current = self.parent_index[descendant];
        while let Some(c) = current {
            if c.index() == ancestor {
                return true;
            }
            current = self.parent_index[c.index()];
        }
        false
    }

    fn traverse_subtree<F>(&self, index: usize, visitor: &mut F)
    where
        F: FnMut(TraversalEvent),
    {
        let node = &self.nodes[index];
        let visible = node.flags.contains(UIFlags::VISIBLE);

        // If this node is invisible, skip it AND all its children.
        if !visible {
            return;
        }

        visitor(TraversalEvent::Node(node));

        let clipping = node.flags.contains(UIFlags::CLIPS_CHILDREN);
        if clipping {
            visitor(TraversalEvent::PushClip(node.bounds));
        }

        let mut child = self.first_child[index];
        while let Some(c) = child {
            self.traverse_subtree(c.index(), visitor);
            child = self.next_sibling[c.index()];
        }

        if clipping {
            visitor(TraversalEvent::PopClip);
        }
    }

    /// Get the parent of a node, or `None` for a root / stale / out-of-range id.
    pub fn parent_of(&self, id: NodeId) -> Option<NodeId> {
        if !self.is_live(id) {
            return None;
        }
        self.parent_index[id.index()]
    }

    // ── Durable widget identity ─────────────────────────────────────

    /// The durable [`WidgetId`] of `id`, or [`WidgetId::NONE`] for a stale id.
    /// Stable across rebuilds, so the input system can capture it at press and
    /// recognise the same widget at release even after the tree was rebuilt.
    pub fn widget_of(&self, id: NodeId) -> WidgetId {
        if !self.is_live(id) {
            return WidgetId::NONE;
        }
        self.widget_ids[id.index()]
    }

    /// The live [`NodeId`] that carries `widget` in the *current* build, if any
    /// interactive node does. This is how a held `WidgetId` (press / hover /
    /// focus, captured in an earlier frame) resolves back to a node the input
    /// system can emit against or mutate — always a fresh, live id.
    pub fn node_for_widget(&self, widget: WidgetId) -> Option<NodeId> {
        if widget == WidgetId::NONE {
            return None;
        }
        self.widget_to_node.get(&widget).copied()
    }

    // ── Automation component names (D8, §3) ──────────────────────────

    /// Register `id`'s durable component name (e.g. `"layer_header.mute"`) —
    /// the automation selector surface's `name` field. Call once, right after
    /// the builder that minted `id` returns, at a high-value interaction point
    /// (§3's naming-pass scope) — most nodes stay unnamed and stay reachable via
    /// text/type/structure. A no-op (debug-asserts) on a stale/invalid id.
    ///
    /// Accepts both `&'static str` literals (the mute/solo-chip style: one
    /// static name shared across every instance, disambiguated by `under_text`)
    /// and owned `String`s (the per-row identity style: param-id-derived names
    /// on converged card rows, `WIDGET_TREE_DESIGN.md` §5 — a row's own name IS
    /// its selector because `under_text` can't reach a control past the value
    /// cell in a flat row). Owned names live in the tree's `names` vec and die
    /// with the rebuild — no leak, no global interner.
    pub fn set_name(&mut self, id: NodeId, name: impl Into<Box<str>>) {
        debug_assert!(
            self.is_live(id),
            "set_name on a stale/invalid NodeId (index {} gen {})",
            id.index(),
            id.generation()
        );
        if self.is_live(id) {
            self.names[id.index()] = Some(name.into());
        }
    }

    /// `id`'s registered component name, or `None` if it was never named (the
    /// common case) or `id` is stale. Read by the automation dump (§3).
    pub fn name_of(&self, id: NodeId) -> Option<&str> {
        if !self.is_live(id) {
            return None;
        }
        self.names[id.index()].as_deref()
    }

    // ── Clear ───────────────────────────────────────────────────────

    pub fn clear(&mut self) {
        self.nodes.clear();
        self.parent_index.clear();
        self.first_child.clear();
        self.next_sibling.clear();
        self.last_child.clear();
        // Drop the slot generations but keep `gen_counter` climbing, so ids minted
        // after this clear never collide with ids from before it (the slots are
        // reused from index 0, but with fresh, higher generations).
        self.generations.clear();
        self.widget_ids.clear();
        self.names.clear();
        self.child_counts.clear();
        self.root_minted.clear();
        self.root_count = 0;
        // Retain the map's capacity — the editor clears+refills it every frame.
        self.widget_to_node.clear();
        self.regions.clear();
        // `open_regions` is NOT reset here: a `clear()` mid-build (none exist
        // today) would otherwise silently drop the bracket a caller is still
        // holding a token for. Normal usage always pairs begin/end within one
        // build pass, so this is zero-cost in practice and fails loud (the D4
        // assertion) instead of silent if that assumption is ever broken.
        self.count = 0;
        self.has_dirty = false;
        self.structure_version = self.structure_version.wrapping_add(1);
    }

    /// Truncate the tree to `from_index` nodes, removing everything at and
    /// after that index. Used for partial rebuilds: panels before `from_index`
    /// keep their nodes intact; panels at/after are rebuilt from scratch.
    ///
    /// SAFETY: `from_index` must be a panel boundary — no remaining node
    /// may have children, siblings, or parents at index >= from_index.
    /// This is guaranteed when panels are self-contained subtrees built
    /// sequentially (each panel's nodes are contiguous).
    pub fn truncate_from(&mut self, from_index: usize) {
        if from_index >= self.count {
            return;
        }
        // Drop the reverse-lookup entries for the interactive nodes being removed,
        // before their rows go. Surviving nodes (< from_index) keep their entries
        // and their still-live ids untouched.
        for i in from_index..self.count {
            if self.nodes[i].flags.contains(UIFlags::INTERACTIVE) {
                self.widget_to_node.remove(&self.widget_ids[i]);
            }
        }
        self.nodes.truncate(from_index);
        self.parent_index.truncate(from_index);
        self.first_child.truncate(from_index);
        self.next_sibling.truncate(from_index);
        self.last_child.truncate(from_index);
        // Drop the truncated slots' generations; `gen_counter` keeps climbing, so
        // the rebuilt tail gets fresh generations and ids minted against the old
        // tail (e.g. a panel that didn't re-capture after a partial rebuild) no
        // longer validate.
        self.generations.truncate(from_index);
        self.widget_ids.truncate(from_index);
        self.names.truncate(from_index);
        self.child_counts.truncate(from_index);
        // Surviving parents keep their child_counts (a partial rebuild's safety
        // invariant guarantees their children all survived), so the rebuilt tail
        // re-salts correctly. Only `root_count` needs recomputing — the rebuilt
        // tail's roots must continue the surviving roots' salt sequence.
        //
        // Count roots by `root_minted` (mint-time parentage), NOT current
        // `parent_index.is_none()`: a survivor minted as a root but later
        // reparented (the inspector wraps subpanels under a ClipRegion) still
        // consumed its root salt, and undercounting it here re-issued that salt
        // to a rebuilt root → a `WidgetId` collision (see `root_minted`).
        self.root_count = self.root_minted[..from_index]
            .iter()
            .filter(|&&r| r)
            .count() as u32;
        self.root_minted.truncate(from_index);
        // Drop region entries whose root node was truncated away — a partial
        // rebuild that re-enters the same range mints a fresh region via a
        // fresh `begin_region` call, so a stale entry here would leave a dead
        // NodeId in the tier-sorted render walk (harmless — `is_live` filters
        // it — but keeps the region list bounded and honest).
        self.regions.retain(|&(_, root)| root.index() < from_index);
        self.count = from_index;
        self.has_dirty = true;
        self.structure_version = self.structure_version.wrapping_add(1);
    }
}

impl Default for UITree {
    fn default() -> Self {
        Self::new()
    }
}

/// Events emitted during tree traversal.
pub enum TraversalEvent<'a> {
    Node(&'a UINode),
    PushClip(Rect),
    PopClip,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_style() -> UIStyle {
        UIStyle::default()
    }

    #[test]
    fn add_nodes_and_count() {
        let mut tree = UITree::new();
        assert_eq!(tree.count(), 0);

        let root = tree.add_panel(None, 0.0, 0.0, 800.0, 600.0, default_style());
        assert_eq!(root.index(), 0);
        assert_eq!(tree.count(), 1);

        let child = tree.add_button(Some(root), 10.0, 10.0, 100.0, 30.0, default_style(), "OK");
        assert_eq!(child.index(), 1);
        assert_eq!(tree.count(), 2);
    }

    #[test]
    fn parent_child_relationships() {
        let mut tree = UITree::new();
        let root = tree.add_panel(None, 0.0, 0.0, 800.0, 600.0, default_style());
        let a = tree.add_panel(Some(root), 0.0, 0.0, 400.0, 300.0, default_style());
        let b = tree.add_panel(Some(root), 400.0, 0.0, 400.0, 300.0, default_style());

        assert_eq!(tree.parent_of(root), None);
        assert_eq!(tree.parent_of(a), Some(root));
        assert_eq!(tree.parent_of(b), Some(root));
    }

    #[test]
    fn hit_test_topmost() {
        let mut tree = UITree::new();
        let root = tree.add_panel(None, 0.0, 0.0, 800.0, 600.0, default_style());
        // Two overlapping buttons — second is on top
        let _btn1 = tree.add_button(Some(root), 50.0, 50.0, 100.0, 30.0, default_style(), "A");
        let btn2 = tree.add_button(Some(root), 50.0, 50.0, 100.0, 30.0, default_style(), "B");

        let hit = tree.hit_test(Vec2::new(60.0, 60.0));
        assert_eq!(hit, Some(btn2));
    }

    #[test]
    fn structure_version_bumps_on_structural_ops_only() {
        let mut tree = UITree::new();
        let v0 = tree.structure_version();
        let btn = tree.add_button(None, 0.0, 0.0, 100.0, 30.0, default_style(), "A");
        let v1 = tree.structure_version();
        assert!(v1 > v0, "add_node must bump structure_version");

        // In-place set_* mutations must NOT bump it (intent ids stay valid).
        tree.set_flag(btn, UIFlags::PRESSED);
        tree.set_text(btn, "B");
        assert_eq!(
            tree.structure_version(),
            v1,
            "set_* mutations must not bump structure_version"
        );

        // truncate + clear are structural.
        tree.truncate_from(0);
        let v2 = tree.structure_version();
        assert!(v2 > v1, "truncate_from must bump structure_version");
        tree.clear();
        assert!(tree.structure_version() > v2, "clear must bump structure_version");
    }

    #[test]
    fn hit_test_miss() {
        let mut tree = UITree::new();
        let root = tree.add_panel(None, 0.0, 0.0, 800.0, 600.0, default_style());
        let _btn = tree.add_button(Some(root), 50.0, 50.0, 100.0, 30.0, default_style(), "A");

        let hit = tree.hit_test(Vec2::new(200.0, 200.0));
        assert_eq!(hit, None);
    }

    #[test]
    fn hit_test_respects_disabled() {
        let mut tree = UITree::new();
        let root = tree.add_panel(None, 0.0, 0.0, 800.0, 600.0, default_style());
        let btn = tree.add_button(Some(root), 50.0, 50.0, 100.0, 30.0, default_style(), "A");
        tree.set_flag(btn, UIFlags::DISABLED);

        let hit = tree.hit_test(Vec2::new(60.0, 60.0));
        assert_eq!(hit, None);
    }

    #[test]
    fn hit_test_respects_clip_ancestors() {
        let mut tree = UITree::new();
        // Clip region that only covers (0,0)-(50,50)
        let clip = tree.add_node(
            None,
            Rect::new(0.0, 0.0, 50.0, 50.0),
            UINodeType::ClipRegion,
            default_style(),
            None,
            UIFlags::CLIPS_CHILDREN,
        );
        // Button extends past clip
        let _btn = tree.add_button(Some(clip), 0.0, 0.0, 200.0, 200.0, default_style(), "X");

        // Inside clip → hit
        assert!(tree.hit_test(Vec2::new(25.0, 25.0)).is_some());
        // Outside clip → miss
        assert_eq!(tree.hit_test(Vec2::new(100.0, 100.0)), None);
    }

    #[test]
    fn set_text_dedup() {
        let mut tree = UITree::new();
        let id = tree.add_label(None, 0.0, 0.0, 100.0, 20.0, "Hello", default_style());
        tree.clear_dirty();

        // Same text → no dirty
        tree.set_text(id, "Hello");
        assert!(!tree.has_dirty());

        // Different text → dirty
        tree.set_text(id, "World");
        assert!(tree.has_dirty());
        assert_eq!(tree.get_node(id).unwrap().text.as_deref(), Some("World"));
    }

    #[test]
    fn traversal_order() {
        // Post-D1/D4: `traverse()` visits registered regions, not a raw
        // root scan — build this fixture the way `ui_root.rs` now does
        // (`begin_region`, build content, `end_region`) instead of a bare
        // `add_panel(None, ...)`.
        let mut tree = UITree::new();
        let region = tree.begin_region(
            Rect::new(0.0, 0.0, 800.0, 600.0),
            ZTier::Base,
            "test",
            UIFlags::empty(),
        );
        let start = tree.count();
        let _a = tree.add_label(None, 0.0, 0.0, 100.0, 20.0, "A", default_style());
        let _b = tree.add_label(None, 0.0, 20.0, 100.0, 20.0, "B", default_style());
        tree.end_region(region, start);

        let mut order = Vec::new();
        tree.traverse(|event| {
            if let TraversalEvent::Node(node) = event {
                order.push(node.id);
            }
        });

        let indices: Vec<usize> = order.iter().map(|n| n.index()).collect();
        assert_eq!(indices, vec![0, 1, 2]); // DFS pre-order: region root, A, B
    }

    /// D4's second enforcement leg (`all_roots_are_regions`), positive case:
    /// a tree built entirely through `begin_region`/`end_region` — several
    /// regions, several tiers, some with multi-node content swept under them
    /// — has every root accounted for.
    #[test]
    fn all_roots_are_regions_holds_for_a_properly_regioned_tree() {
        let mut tree = UITree::new();

        let chrome = tree.begin_region(Rect::new(0.0, 0.0, 800.0, 40.0), ZTier::Chrome, "chrome", UIFlags::empty());
        let start = tree.count();
        tree.add_panel(None, 0.0, 0.0, 100.0, 20.0, default_style());
        tree.add_panel(None, 0.0, 20.0, 100.0, 20.0, default_style());
        tree.end_region(chrome, start);

        let base = tree.begin_region(Rect::new(0.0, 40.0, 800.0, 500.0), ZTier::Base, "base", UIFlags::empty());
        let start = tree.count();
        tree.add_panel(None, 0.0, 40.0, 200.0, 200.0, default_style());
        tree.end_region(base, start);

        let ghost = tree.begin_region(
            Rect::new(0.0, 0.0, 800.0, 600.0),
            ZTier::Ghost,
            "ghost",
            UIFlags::ALLOW_OVERFLOW,
        );
        let start = tree.count();
        tree.add_label(None, 400.0, 300.0, 80.0, 20.0, "ghost", default_style());
        tree.end_region(ghost, start);

        assert!(tree.all_roots_are_regions());
    }

    /// D4's second enforcement leg, negative case: a stray root-parented
    /// node created with no `begin_region` bracket open at all (the exact
    /// per-panel hand-clip shape D1 forbids) is caught by
    /// `all_roots_are_regions` even though `mint`'s debug assertion is
    /// `cfg(not(test))`-gated and stays silent inside this crate's own
    /// suite (`tree.rs`'s `mint` doc comment).
    #[test]
    fn all_roots_are_regions_catches_a_stray_root() {
        let mut tree = UITree::new();
        let region = tree.begin_region(Rect::new(0.0, 0.0, 100.0, 100.0), ZTier::Base, "base", UIFlags::empty());
        let start = tree.count();
        tree.add_panel(None, 0.0, 0.0, 50.0, 50.0, default_style());
        tree.end_region(region, start);
        assert!(tree.all_roots_are_regions(), "sanity: the well-formed part passes");

        // A panel that forgot to wrap itself — no begin_region, no end_region.
        tree.add_panel(None, 0.0, 0.0, 10.0, 10.0, default_style());
        assert!(
            !tree.all_roots_are_regions(),
            "a root-parented node outside any region must fail the D4 check"
        );
    }

    #[test]
    fn clear_resets() {
        let mut tree = UITree::new();
        tree.add_panel(None, 0.0, 0.0, 100.0, 100.0, default_style());
        tree.add_panel(None, 0.0, 0.0, 100.0, 100.0, default_style());
        assert_eq!(tree.count(), 2);

        tree.clear();
        assert_eq!(tree.count(), 0);
        assert!(!tree.has_dirty());

        // Can re-add after clear — indices restart from 0 (generation differs)
        let id = tree.add_panel(None, 0.0, 0.0, 100.0, 100.0, default_style());
        assert_eq!(id.index(), 0);
    }

    #[test]
    fn offset_nodes() {
        let mut tree = UITree::new();
        tree.add_panel(None, 0.0, 0.0, 100.0, 20.0, default_style());
        tree.add_panel(None, 0.0, 20.0, 100.0, 20.0, default_style());
        tree.add_panel(None, 0.0, 40.0, 100.0, 20.0, default_style());

        tree.offset_nodes(1, 2, 100.0);

        assert_eq!(tree.get_bounds(tree.id_at(0)).y, 0.0);
        assert_eq!(tree.get_bounds(tree.id_at(1)).y, 120.0);
        assert_eq!(tree.get_bounds(tree.id_at(2)).y, 140.0);
    }

    #[test]
    fn truncate_from_preserves_earlier_nodes() {
        let mut tree = UITree::new();
        // Panel A: root + child
        let a_root = tree.add_panel(None, 0.0, 0.0, 100.0, 50.0, default_style());
        let _a_child = tree.add_label(Some(a_root), 10.0, 10.0, 80.0, 20.0, "A", default_style());
        let boundary = tree.count(); // = 2

        // Panel B: root + child
        let b_root = tree.add_panel(None, 0.0, 50.0, 100.0, 50.0, default_style());
        let _b_child = tree.add_label(Some(b_root), 10.0, 60.0, 80.0, 20.0, "B", default_style());
        assert_eq!(tree.count(), 4);

        // Truncate at panel B boundary
        tree.truncate_from(boundary);
        assert_eq!(tree.count(), 2);
        assert!(tree.has_dirty());

        // Panel A nodes intact
        assert_eq!(tree.get_bounds(tree.id_at(0)).y, 0.0);
        assert_eq!(tree.get_node(tree.id_at(1)).unwrap().text.as_deref(), Some("A"));

        // Can re-add after truncation — indices continue from the truncation point
        let new_id = tree.add_panel(None, 0.0, 100.0, 100.0, 50.0, default_style());
        assert_eq!(new_id.index(), 2);
        assert_eq!(tree.count(), 3);
    }

    #[test]
    fn truncate_from_beyond_count_is_noop() {
        let mut tree = UITree::new();
        tree.add_panel(None, 0.0, 0.0, 100.0, 100.0, default_style());
        tree.clear_dirty();

        tree.truncate_from(5);
        assert_eq!(tree.count(), 1);
        assert!(!tree.has_dirty());
    }

    /// Regression (the Audio Setup double-click root cause): a partial rebuild
    /// must re-issue the SAME root salt a node had before the rebuild, even when
    /// an earlier root was REPARENTED (the inspector wraps its subpanels under a
    /// ClipRegion). Pre-fix, `truncate_from` recomputed `root_count` from the
    /// current root-parented survivors — undercounting the reparented roots that
    /// had already consumed a salt — so the rebuilt root got a lower salt: its
    /// `WidgetId` churned AND collided with the surviving reparented root (a
    /// press+release then resolved to different widgets → the dead first click).
    #[test]
    fn truncate_from_reproduces_root_salt_after_reparent() {
        let mut tree = UITree::new();

        // A "static region" that mimics the inspector: a container plus three
        // interactive roots that get wrapped under it. They consume root salts
        // 1, 2, 3 at mint, then stop being roots.
        let container = tree.add_panel(None, 0.0, 0.0, 100.0, 100.0, default_style());
        let start = tree.count();
        tree.add_button(None, 0.0, 0.0, 10.0, 10.0, default_style(), "a");
        tree.add_button(None, 0.0, 10.0, 10.0, 10.0, default_style(), "b");
        tree.add_button(None, 0.0, 20.0, 10.0, 10.0, default_style(), "c");
        tree.reparent_root_nodes(start, 3, container);

        // A "scroll region" interactive root, built after the static boundary —
        // its identity must survive a partial rebuild (the overlay-chrome case).
        let boundary = tree.count();
        let scroll_btn = tree.add_button(None, 0.0, 40.0, 10.0, 10.0, default_style(), "scroll");
        let w_before = tree.widget_of(scroll_btn);

        // Partial rebuild from the boundary (mid-click overlay rebuild).
        tree.truncate_from(boundary);
        let scroll_btn2 = tree.add_button(None, 0.0, 40.0, 10.0, 10.0, default_style(), "scroll");
        let w_after = tree.widget_of(scroll_btn2);

        assert_eq!(
            w_before, w_after,
            "a rebuilt root must reproduce its salt across a partial rebuild — \
             reparented roots still consumed their salt, so the count must include them"
        );
        // The reparented roots keep distinct WidgetIds from the rebuilt one (no
        // collision — the debug_assert in `mint` would also catch this).
        assert_ne!(w_after, tree.widget_of(container));
    }

    #[test]
    fn offset_node_and_children_shifts_subtree() {
        let mut tree = UITree::new();
        let parent = tree.add_panel(None, 10.0, 20.0, 100.0, 30.0, default_style());
        let c1 = tree.add_panel(Some(parent), 15.0, 25.0, 10.0, 2.0, default_style());
        let c2 = tree.add_panel(Some(parent), 15.0, 29.0, 10.0, 2.0, default_style());
        let c3 = tree.add_panel(Some(parent), 15.0, 33.0, 10.0, 2.0, default_style());
        tree.clear_dirty();

        tree.offset_node_and_children(parent, 50.0);

        assert!((tree.get_bounds(parent).y - 70.0).abs() < 0.001);
        assert!((tree.get_bounds(c1).y - 75.0).abs() < 0.001);
        assert!((tree.get_bounds(c2).y - 79.0).abs() < 0.001);
        assert!((tree.get_bounds(c3).y - 83.0).abs() < 0.001);
        assert!(tree.has_dirty());
    }

    #[test]
    fn offset_node_and_children_leaf_only() {
        let mut tree = UITree::new();
        let leaf = tree.add_panel(None, 0.0, 10.0, 50.0, 20.0, default_style());
        tree.clear_dirty();

        tree.offset_node_and_children(leaf, -5.0);

        assert!((tree.get_bounds(leaf).y - 5.0).abs() < 0.001);
        assert!(tree.has_dirty());
    }

    #[test]
    fn offset_node_and_children_does_not_affect_siblings() {
        let mut tree = UITree::new();
        let a = tree.add_panel(None, 0.0, 10.0, 50.0, 20.0, default_style());
        let b = tree.add_panel(None, 0.0, 40.0, 50.0, 20.0, default_style());

        tree.offset_node_and_children(a, 100.0);

        assert!((tree.get_bounds(a).y - 110.0).abs() < 0.001);
        assert!((tree.get_bounds(b).y - 40.0).abs() < 0.001); // unchanged
    }

    // ── Generational NodeId ──────────────────────────────────────────

    #[test]
    fn placeholder_is_never_live() {
        let mut tree = UITree::new();
        assert!(!tree.is_live(NodeId::PLACEHOLDER));
        tree.add_panel(None, 0.0, 0.0, 10.0, 10.0, default_style());
        // Even with a node at index 0, the placeholder (generation 0) doesn't match.
        assert!(!tree.is_live(NodeId::PLACEHOLDER));
    }

    #[test]
    fn ids_at_same_index_differ_by_generation_across_clear() {
        let mut tree = UITree::new();
        let before = tree.add_panel(None, 0.0, 0.0, 10.0, 10.0, default_style());
        tree.clear();
        let after = tree.add_panel(None, 0.0, 0.0, 10.0, 10.0, default_style());
        assert_eq!(before.index(), after.index(), "both reuse slot 0");
        assert_ne!(before, after, "but the generation must differ");
        assert!(!tree.is_live(before), "the pre-clear id is stale");
        assert!(tree.is_live(after));
    }

    #[test]
    fn stale_id_after_truncate_is_inert() {
        let mut tree = UITree::new();
        let keep = tree.add_panel(None, 0.0, 0.0, 10.0, 10.0, default_style());
        let boundary = tree.count();
        let doomed = tree.add_label(None, 0.0, 20.0, 10.0, 10.0, "old", default_style());

        tree.truncate_from(boundary);
        // The slot is reused by a different node.
        let reused = tree.add_label(None, 0.0, 20.0, 10.0, 10.0, "new", default_style());
        assert_eq!(doomed.index(), reused.index(), "slot reused");

        // Reads through the stale id are inert — never the new occupant.
        assert!(!tree.is_live(doomed));
        assert_eq!(tree.get_bounds(doomed), Rect::ZERO);
        assert!(!tree.has_flag(doomed, UIFlags::VISIBLE));

        // Writes through the stale id are no-ops — the new occupant is untouched.
        tree.set_text(doomed, "hijacked");
        assert_eq!(tree.get_node(reused).unwrap().text.as_deref(), Some("new"));
        tree.set_bounds(doomed, Rect::new(99.0, 99.0, 1.0, 1.0));
        assert_eq!(tree.get_bounds(reused).y, 20.0);

        // The untouched earlier node still validates.
        assert!(tree.is_live(keep));
    }

    /// `get_node`/`get_node_mut` are the accessors a cached `NodeId` field
    /// goes through (e.g. `ToastPanel::bg_id`/`text_id` repainted every frame
    /// while animating). They must
    /// give the same `None`/no-op inertness as every other accessor here.
    #[test]
    fn get_node_on_stale_id_is_none_not_a_panic() {
        let mut tree = UITree::new();
        let boundary = 0;
        let doomed = tree.add_label(None, 0.0, 0.0, 10.0, 10.0, "old", default_style());
        tree.truncate_from(boundary);

        assert!(tree.get_node(doomed).is_none());
        assert!(tree.get_node_mut(doomed).is_none());
        // Also out-of-range entirely (index never minted) — same inertness.
        assert!(tree.get_node(NodeId::PLACEHOLDER).is_none());

        let live = tree.add_label(None, 0.0, 0.0, 10.0, 10.0, "new", default_style());
        assert!(tree.get_node(live).is_some());
    }

    #[test]
    fn id_at_returns_the_live_id() {
        let mut tree = UITree::new();
        let a = tree.add_panel(None, 0.0, 0.0, 10.0, 10.0, default_style());
        let b = tree.add_panel(None, 0.0, 10.0, 10.0, 10.0, default_style());
        assert_eq!(tree.id_at(0), a);
        assert_eq!(tree.id_at(1), b);
        // Out of range → placeholder (never live).
        assert_eq!(tree.id_at(99), NodeId::PLACEHOLDER);
    }

    // ── WidgetId (durable identity) ──────────────────────────────────

    /// The same build produces the same WidgetIds — the property the editor's
    /// per-frame clear+rebuild relies on. NodeIds differ (fresh generations);
    /// WidgetIds match.
    fn build_sample(tree: &mut UITree) -> (NodeId, NodeId) {
        let root = tree.add_panel(None, 0.0, 0.0, 100.0, 100.0, default_style());
        let btn = tree.add_button(Some(root), 10.0, 10.0, 20.0, 20.0, default_style(), "x");
        (root, btn)
    }

    #[test]
    fn widget_id_is_stable_across_clear_and_rebuild() {
        let mut tree = UITree::new();
        let (_, btn1) = build_sample(&mut tree);
        let w1 = tree.widget_of(btn1);

        tree.clear();
        let (_, btn2) = build_sample(&mut tree);
        let w2 = tree.widget_of(btn2);

        assert_ne!(btn1, btn2, "NodeId gets a fresh generation after rebuild");
        assert_eq!(w1, w2, "WidgetId is the same logical widget across rebuild");
        assert_ne!(w1, WidgetId::NONE);
    }

    #[test]
    fn node_for_widget_resolves_to_the_live_id() {
        let mut tree = UITree::new();
        let (_, btn) = build_sample(&mut tree);
        let w = tree.widget_of(btn);
        assert_eq!(tree.node_for_widget(w), Some(btn));

        // After a rebuild the widget resolves to the NEW live id, not the stale one.
        tree.clear();
        let (_, btn2) = build_sample(&mut tree);
        assert_eq!(tree.node_for_widget(w), Some(btn2));
        assert!(!tree.is_live(btn), "the pre-rebuild id is stale");
    }

    #[test]
    fn widget_of_stale_id_is_none() {
        let mut tree = UITree::new();
        let (_, btn) = build_sample(&mut tree);
        tree.clear();
        assert_eq!(tree.widget_of(btn), WidgetId::NONE);
    }

    #[test]
    fn named_node_round_trips_its_static_str() {
        let mut tree = UITree::new();
        let (root, btn) = build_sample(&mut tree);
        assert_eq!(tree.name_of(btn), None, "unnamed until registered");
        tree.set_name(btn, "layer_header.mute");
        assert_eq!(tree.name_of(btn), Some("layer_header.mute"));
        // A sibling built alongside it stays unnamed — naming is per-node, opt-in.
        assert_eq!(tree.name_of(root), None);
    }

    #[test]
    fn unnamed_node_is_none() {
        let mut tree = UITree::new();
        let (_, btn) = build_sample(&mut tree);
        assert_eq!(tree.name_of(btn), None);
    }

    #[test]
    fn only_interactive_nodes_are_resolvable() {
        let mut tree = UITree::new();
        let panel = tree.add_panel(None, 0.0, 0.0, 100.0, 100.0, default_style());
        // A non-interactive panel has a WidgetId (for hierarchy) but is not in the
        // reverse map — input never targets it.
        let w = tree.widget_of(panel);
        assert_ne!(w, WidgetId::NONE);
        assert_eq!(tree.node_for_widget(w), None);
    }

    #[test]
    fn explicit_key_survives_sibling_reordering() {
        // Build A: one keyed button, then an auto button after it.
        let mut tree = UITree::new();
        let root = tree.add_panel(None, 0.0, 0.0, 100.0, 100.0, default_style());
        let keyed_a = tree.add_node_keyed(
            Some(root),
            Rect::new(0.0, 0.0, 10.0, 10.0),
            UINodeType::Button,
            default_style(),
            Some("k"),
            UIFlags::INTERACTIVE,
            42,
        );
        let auto_a = tree.add_button(Some(root), 0.0, 20.0, 10.0, 10.0, default_style(), "a");
        let keyed_w_a = tree.widget_of(keyed_a);
        let auto_w_a = tree.widget_of(auto_a);

        // Build B: insert a NEW sibling before both, shifting sibling indices.
        tree.clear();
        let root = tree.add_panel(None, 0.0, 0.0, 100.0, 100.0, default_style());
        let _inserted =
            tree.add_button(Some(root), 0.0, 0.0, 10.0, 10.0, default_style(), "new");
        let keyed_b = tree.add_node_keyed(
            Some(root),
            Rect::new(0.0, 0.0, 10.0, 10.0),
            UINodeType::Button,
            default_style(),
            Some("k"),
            UIFlags::INTERACTIVE,
            42,
        );
        let auto_b = tree.add_button(Some(root), 0.0, 20.0, 10.0, 10.0, default_style(), "a");

        // The keyed widget keeps its identity; the auto one (now a later sibling)
        // does not. This is exactly why identity-bearing controls take a key.
        assert_eq!(tree.widget_of(keyed_b), keyed_w_a, "explicit key is reorder-stable");
        assert_ne!(
            tree.widget_of(auto_b),
            auto_w_a,
            "auto sibling-index identity shifts when an earlier sibling is inserted"
        );
    }
}
