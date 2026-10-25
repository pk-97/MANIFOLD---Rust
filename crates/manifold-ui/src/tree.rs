use crate::node::*;
use crate::text::{HeuristicTextMeasure, TextMeasure};

/// Stacking tier for a region root.
/// Paint order is `(tier, region rank, local insertion)`: a lower tier paints
/// first (further back). So a `Chrome` region (the transport/header/footer
/// frame) always wins over a `Base` region (timeline/viewport/inspector)
/// regardless of which one built first.
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

const TIERS: [ZTier; 4] = [ZTier::Base, ZTier::Chrome, ZTier::Overlay, ZTier::Ghost];

/// Region arenas per tree (UI_INVALIDATION_DESIGN.md D7). A region id is the
/// window's region discriminant (`MainRegion` in the main window), and it is
/// also the region's paint rank inside a tier.
pub const MAX_REGIONS: usize = 16;

/// The arena for content no named region owns: trees built whole in tests, and
/// windows that still rebuild their whole tree (the graph editor). Ranks last.
pub const DEFAULT_REGION: u8 = (MAX_REGIONS - 1) as u8;

/// A region opened by [`UITree::begin_region`] or
/// [`UITree::begin_region_in`] — the token its content builds under. `root` is
/// the container node's id; pass `Some(token.root)` as the parent for content
/// built directly under it, or capture `tree.count()` before calling the
/// panel's `None`-rooted build and sweep the result under `token.root` with
/// [`end_region`](UITree::end_region).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RegionToken {
    pub root: NodeId,
    /// The arena parentless nodes minted into before this region opened.
    outer: u8,
}

/// One `begin_region` inside an arena: its tier and its root. The sub-region
/// owns the slots from its root up to the next sub-region's root.
#[derive(Clone, Copy)]
struct SubRegion {
    tier: ZTier,
    root: NodeId,
}

/// One region's nodes: the structure-of-arrays storage, slot-indexed.
///
/// INVARIANTS:
///   - Structural operations (mint) occur only while the region builds;
///     interaction frames use only set_* mutations (O(1), no allocation).
///   - Traversal order: depth-first pre-order matches insertion order.
///   - Every parent link stays inside the arena, so clearing one arena can
///     never leave another holding a dangling link.
#[derive(Default)]
struct RegionArena {
    nodes: Vec<UINode>,
    parent_index: Vec<Option<NodeId>>,
    first_child: Vec<Option<NodeId>>,
    next_sibling: Vec<Option<NodeId>>,
    last_child: Vec<Option<NodeId>>,
    /// Generation stamp per slot, minted from the tree's `gen_counter`, so a
    /// slot reused after a clear carries a fresh generation and ids minted
    /// against the old occupant no longer validate.
    generations: Vec<u32>,
    /// Durable [`WidgetId`] per slot: the parent's id mixed with a stable
    /// sibling salt, so the same logical widget gets the same id on every
    /// rebuild of a structurally stable build.
    widget_ids: Vec<WidgetId>,
    /// Durable component name per slot;
    /// `None` for most nodes.
    names: Vec<Option<Box<str>>>,
    /// Per-slot count of children added so far: the auto sibling salt for
    /// the next child.
    child_counts: Vec<u32>,
    /// Parentless nodes minted so far: the auto salt for the next root. Each
    /// arena counts from zero, seeded by its region id, so a region rebuilt
    /// alone reproduces its own root salts and never collides with another
    /// region's.
    root_count: u32,
    has_dirty: bool,
    sub_regions: Vec<SubRegion>,
}

impl RegionArena {
    fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Drop every node, keeping capacity.
    fn clear(&mut self) {
        self.nodes.clear();
        self.parent_index.clear();
        self.first_child.clear();
        self.next_sibling.clear();
        self.last_child.clear();
        self.generations.clear();
        self.widget_ids.clear();
        self.names.clear();
        self.child_counts.clear();
        self.root_count = 0;
        self.has_dirty = false;
        self.sub_regions.clear();
    }

    /// Slot span `[root, next sub-region root)` of sub-region `i`.
    fn sub_span(&self, i: usize) -> (usize, usize) {
        let start = self.sub_regions[i].root.slot();
        let end = self
            .sub_regions
            .get(i + 1)
            .map_or(self.len(), |s| s.root.slot());
        (start, end.max(start))
    }

    fn recompute_dirty(&mut self) {
        self.has_dirty = self.nodes.iter().any(|n| n.flags.contains(UIFlags::DIRTY));
    }
}

/// Node storage for the bitmap UI system: one arena per region
/// (UI_INVALIDATION_DESIGN.md D7, section 3). Rebuilding a region clears and
/// refills its own arena only, so the other regions' nodes, ids and atlas
/// pixels stay untouched.
///
/// A [`NodeId`] names its region and its slot in that region's arena. Slot
/// numbers, `count()` captures and node ranges are region-local.
///
/// Paint order runs over every sub-region of every arena by (tier, region
/// rank, local insertion); hit order is the reverse. Region rank is the region
/// id, which the main window assigns in its full-build order, so the order
/// equals the old flat tree's.
pub struct UITree {
    arenas: [RegionArena; MAX_REGIONS],
    /// Monotonic source of generation stamps. Starts at 1 — generation 0 is the
    /// reserved "no live node" stamp ([`NodeId::PLACEHOLDER`]), so it is never
    /// minted.
    gen_counter: u32,
    /// Reverse index: durable [`WidgetId`] → its live [`NodeId`], for the
    /// interactive nodes only (the ones press / hover / focus can target).
    /// Widget ids are unique across regions (INV-18), so one map serves all.
    widget_to_node: ahash::AHashMap<WidgetId, NodeId>,
    /// Bumped on every *structural* change (mint, clear, clear_region) —
    /// never on in-place set_* mutations. Consumers that cache per-node-id
    /// data (the intent registry) repopulate only when this changes.
    structure_version: u64,
    /// Text measurement available to the *build* path. Defaults to the
    /// GPU-free [`HeuristicTextMeasure`]; the app installs a CoreText-accurate
    /// measurer via [`set_text_measure`](UITree::set_text_measure).
    text_measure: Box<dyn TextMeasure>,
    /// Depth of open `begin_region` brackets. While open, `mint` permits a
    /// `parent_id: None` node — the "build flat, then sweep under the region"
    /// idiom [`end_region`](UITree::end_region) closes.
    open_regions: u32,
    /// The arena a parentless node mints into: the innermost open region's,
    /// else [`DEFAULT_REGION`]. A node with a parent always mints into its
    /// parent's arena.
    building: u8,
    /// Where an overlay minted at event time (a dropdown opening) goes, so
    /// the overlay region's next rebuild replaces it.
    overlay_region: u8,
    /// The card-drag ghost's arena (D7): no atlas slot.
    ghost_region: u8,
}

const INITIAL_CAPACITY: usize = 512;

impl UITree {
    /// High bit set on an explicit-key salt so it can never collide with an auto
    /// sibling-index salt (sibling indices are small, well under 2^62).
    const EXPLICIT_KEY_FLAG: u64 = 1 << 63;
    /// Mixed into each arena's root seed with its region id (INV-18): two
    /// region roots can never share a `WidgetId`.
    const REGION_SALT_FLAG: u64 = 1 << 62;

    pub fn new() -> Self {
        Self {
            arenas: std::array::from_fn(|_| RegionArena::default()),
            gen_counter: 1,
            widget_to_node: ahash::AHashMap::with_capacity(INITIAL_CAPACITY),
            structure_version: 0,
            text_measure: Box::new(HeuristicTextMeasure),
            open_regions: 0,
            building: DEFAULT_REGION,
            overlay_region: DEFAULT_REGION,
            ghost_region: DEFAULT_REGION,
        }
    }

    /// Slots minted so far in the arena being built ([`DEFAULT_REGION`]
    /// outside any region). Region-local: capture it inside a build to mark a
    /// panel's range in its own arena.
    pub fn count(&self) -> usize {
        self.arenas[self.building as usize].len()
    }

    /// The arena parentless nodes mint into right now.
    pub fn building_region(&self) -> u8 {
        self.building
    }

    /// Nodes in `region`'s arena.
    pub fn region_len(&self, region: u8) -> usize {
        self.arenas[region as usize].len()
    }

    /// The slot range of `region`'s arena, `(0, len)`.
    pub fn region_range(&self, region: u8) -> (usize, usize) {
        (0, self.region_len(region))
    }

    /// Nodes across every arena.
    pub fn total_count(&self) -> usize {
        self.arenas.iter().map(RegionArena::len).sum()
    }

    /// Route event-time overlay mints (`begin_region_in(tree.overlay_region(), …)`)
    /// and the card-drag ghost into the window's overlay and ghost regions.
    pub fn set_region_roles(&mut self, overlay: u8, ghost: u8) {
        self.overlay_region = overlay;
        self.ghost_region = ghost;
    }

    pub fn overlay_region(&self) -> u8 {
        self.overlay_region
    }

    pub fn ghost_region(&self) -> u8 {
        self.ghost_region
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
        self.arenas.iter().any(|a| a.has_dirty)
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
        // (`open_regions > 0`). A `None`-parented node minted with no region
        // open is the per-panel hand-clip bug class that regions exist to kill.
        //
        // `cfg(not(test))`: `manifold-ui`'s own unit tests build panels on a
        // bare `UITree::new()`; the D4 structural test (`all_roots_are_regions`)
        // is the enforcement inside this crate's suite.
        #[cfg(not(test))]
        debug_assert!(
            parent_id.is_some() || self.open_regions > 0,
            "root-parented node minted outside an open UITree::begin_region — \
             Wrap this subtree's build \
             in begin_region(...)/end_region(...) instead of rooting it at the tree."
        );
        // `gen_counter` never yields 0 (reserved for PLACEHOLDER); skip it on
        // the wrap.
        let generation = self.gen_counter;
        self.gen_counter = self.gen_counter.wrapping_add(1);
        if self.gen_counter == 0 {
            self.gen_counter = 1;
        }
        let region = match parent_id {
            Some(p) => p.region(),
            None => self.building,
        };
        let arena = &mut self.arenas[region as usize];
        let slot = arena.len();
        debug_assert!(
            slot < 1 << NodeId::SLOT_BITS,
            "region {region} arena overflow"
        );
        let id = NodeId::from_parts(region, slot as u32, generation);

        // Durable WidgetId: the parent's id mixed with a stable salt. An explicit
        // key (namespaced by EXPLICIT_KEY_FLAG) overrides the sibling index, so the
        // same logical widget resolves to the same WidgetId across rebuilds.
        let parent_widget = match parent_id {
            Some(p) => arena.widget_ids[p.slot()],
            None => WidgetId::ROOT.with(Self::REGION_SALT_FLAG | u64::from(region)),
        };
        let salt = match key {
            Some(k) => Self::EXPLICIT_KEY_FLAG | k,
            None => match parent_id {
                Some(p) => {
                    let s = arena.child_counts[p.slot()] as u64;
                    arena.child_counts[p.slot()] += 1;
                    s
                }
                None => {
                    let s = arena.root_count as u64;
                    arena.root_count += 1;
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
            tooltip: None,
            texture: None,
            uv: [0.0, 0.0, 1.0, 1.0],
            font_family: None,
            draw_order: slot as i32,
        };

        arena.nodes.push(node);
        arena.parent_index.push(parent_id);
        arena.first_child.push(None);
        arena.next_sibling.push(None);
        arena.last_child.push(None);
        arena.generations.push(generation);
        arena.widget_ids.push(widget_id);
        arena.names.push(None);
        arena.child_counts.push(0);
        arena.has_dirty = true;

        // Only interactive nodes are ever the target of press / hover / focus, so
        // only they need a reverse lookup. Two interactive widgets sharing an id
        // is a real bug (a duplicate explicit key), caught here in debug.
        if extra_flags.contains(UIFlags::INTERACTIVE) {
            let prev = self.widget_to_node.insert(widget_id, id);
            debug_assert!(
                prev.is_none(),
                "two interactive nodes share WidgetId {widget_id:?} — duplicate explicit key \
                 among siblings, or a sibling-salt collision"
            );
        }

        self.link_child(id, parent_id);
        self.structure_version = self.structure_version.wrapping_add(1);
        id
    }

    fn link_child(&mut self, child_id: NodeId, parent_id: Option<NodeId>) {
        let Some(parent) = parent_id else {
            return;
        };
        debug_assert_eq!(
            child_id.region(),
            parent.region(),
            "a parent link never crosses regions"
        );
        let arena = &mut self.arenas[parent.region() as usize];
        let pid = parent.slot();
        if arena.first_child[pid].is_none() {
            arena.first_child[pid] = Some(child_id);
        } else {
            let last = arena.last_child[pid]
                .expect("last_child set when first_child set")
                .slot();
            arena.next_sibling[last] = Some(child_id);
        }
        arena.last_child[pid] = Some(child_id);
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
    /// registered-image cache — filling the rect, rounded to `corner_radius`.
    /// Non-interactive by design — an interactive sibling drawn in the same
    /// rect keeps click/hover handling unchanged.
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
        if let Some(node) = self.get_node_mut(id) {
            node.texture = Some(texture);
        }
        id
    }

    /// Add a non-interactive image node that samples a `uv` sub-rect
    /// (`[u0, v0, u1, v1]`) of `texture` instead of the whole texture — one
    /// cell of a shared atlas (the graph canvas's node-preview atlas).
    #[allow(clippy::too_many_arguments)]
    pub fn add_image_uv(
        &mut self,
        parent_id: Option<NodeId>,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        corner_radius: f32,
        texture: TextureHandle,
        uv: [f32; 4],
    ) -> NodeId {
        let id = self.add_image(parent_id, x, y, w, h, corner_radius, texture);
        if let Some(node) = self.get_node_mut(id) {
            node.uv = uv;
        }
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

    /// The live id at `slot` of the arena being built — for build-time
    /// captures (`let start = tree.count(); …; tree.id_at(start)`) and
    /// single-arena trees. Code that walks a stored range after the build
    /// names its region with [`id_in`](Self::id_in).
    #[inline]
    pub fn id_at(&self, slot: usize) -> NodeId {
        self.id_in(self.building, slot)
    }

    /// The live id at `slot` of `region`'s arena, or [`NodeId::PLACEHOLDER`]
    /// for an out-of-range slot, so the result never matches a live node.
    #[inline]
    pub fn id_in(&self, region: u8, slot: usize) -> NodeId {
        match self
            .arenas
            .get(region as usize)
            .and_then(|a| a.generations.get(slot))
        {
            Some(&generation) => NodeId::from_parts(region, slot as u32, generation),
            None => NodeId::PLACEHOLDER,
        }
    }

    /// Whether `id` refers to a live node: its slot is in its arena's range
    /// AND the slot's generation matches. A stale id fails here, which turns
    /// every accessor below into a safe no-op for it.
    #[inline]
    pub fn is_live(&self, id: NodeId) -> bool {
        self.arenas
            .get(id.region() as usize)
            .and_then(|a| a.generations.get(id.slot()))
            .is_some_and(|&g| g == id.generation())
    }

    /// Whether live `id` is one of the `count` nodes a panel built starting at
    /// `first`. The range owns nothing once `first` is stale: after its region
    /// is rebuilt, the same slots belong to whoever built them this time.
    /// Panels that remember their nodes as a slot range ask this, never a bare
    /// slot comparison.
    #[inline]
    pub fn range_owns(&self, first: NodeId, count: usize, id: NodeId) -> bool {
        id.region() == first.region()
            && id.slot() >= first.slot()
            && id.slot() - first.slot() < count
            && self.is_live(first)
            && self.is_live(id)
    }

    /// `None` if `id` is stale or invalid. Callers that cache a `NodeId`
    /// across a rebuild must handle `None`.
    pub fn get_node(&self, id: NodeId) -> Option<&UINode> {
        self.is_live(id)
            .then(|| &self.arenas[id.region() as usize].nodes[id.slot()])
    }

    /// Every node, arena by arena in region order, slot order inside each.
    /// For headless inspection (the snapshot dump, test lookups); the
    /// renderer and hit test use the typed accessors.
    pub fn all_nodes(&self) -> impl DoubleEndedIterator<Item = &UINode> + Clone {
        self.arenas.iter().flat_map(|a| a.nodes.iter())
    }

    /// `region`'s nodes in slot order.
    pub fn region_nodes(&self, region: u8) -> &[UINode] {
        &self.arenas[region as usize].nodes
    }

    /// `None` if `id` is stale or invalid — see [`Self::get_node`].
    pub fn get_node_mut(&mut self, id: NodeId) -> Option<&mut UINode> {
        if !self.is_live(id) {
            return None;
        }
        Some(&mut self.arenas[id.region() as usize].nodes[id.slot()])
    }

    pub fn get_bounds(&self, id: NodeId) -> Rect {
        self.get_node(id).map_or(Rect::ZERO, |n| n.bounds)
    }

    /// The live node behind `id` with its arena, for the in-place setters.
    fn live_mut(&mut self, id: NodeId) -> Option<(&mut RegionArena, usize)> {
        if !self.is_live(id) {
            return None;
        }
        Some((&mut self.arenas[id.region() as usize], id.slot()))
    }

    // ── Batch offset (composite panel build) ────────────────────────

    /// Offset Y of `count` slots of `region`'s arena from `from`.
    pub fn offset_nodes(&mut self, region: u8, from: usize, count: usize, dy: f32) {
        let arena = &mut self.arenas[region as usize];
        let end = (from + count).min(arena.len());
        for n in &mut arena.nodes[from.min(end)..end] {
            n.bounds.y += dy;
        }
    }

    // ── Regions ────────

    /// Open a sub-region of the arena being built ([`DEFAULT_REGION`] outside
    /// any build): a container node at `rect` carrying `CLIPS_CHILDREN` by
    /// construction — unless `extra_flags` carries `ALLOW_OVERFLOW` (D3), the
    /// `Ghost`-tier escape hatch — registered with its tier. A dropdown built
    /// by the panel that owns it is a sub-region of that panel's arena.
    ///
    /// Capture `tree.count()` right after this call, build the panel
    /// (`None`-rooted internally), then `end_region(token, that_start)` sweeps
    /// it under `token.root`.
    pub fn begin_region(
        &mut self,
        rect: Rect,
        tier: ZTier,
        label: &'static str,
        extra_flags: UIFlags,
    ) -> RegionToken {
        self.begin_region_in(self.building, rect, tier, label, extra_flags)
    }

    /// Open a sub-region of `region`'s arena; parentless nodes mint there
    /// until the matching [`end_region`](Self::end_region). A window's
    /// top-level panel build names its region here, after
    /// [`clear_region`](Self::clear_region) when it rebuilds.
    pub fn begin_region_in(
        &mut self,
        region: u8,
        rect: Rect,
        tier: ZTier,
        label: &'static str,
        extra_flags: UIFlags,
    ) -> RegionToken {
        debug_assert!(
            (region as usize) < MAX_REGIONS,
            "region {region} out of range"
        );
        let outer = self.building;
        self.building = region;
        self.open_regions += 1;
        let clips = if extra_flags.contains(UIFlags::ALLOW_OVERFLOW) {
            extra_flags
        } else {
            extra_flags | UIFlags::CLIPS_CHILDREN
        };
        let root = self.mint(
            None,
            rect,
            UINodeType::ClipRegion,
            UIStyle::default(),
            None,
            clips,
            None,
        );
        self.set_name(root, label);
        self.arenas[region as usize]
            .sub_regions
            .push(SubRegion { tier, root });
        RegionToken { root, outer }
    }

    /// Close the bracket `begin_region` opened: sweeps every still-
    /// `None`-parented node in `[content_start, count)` of the region's arena
    /// under `token.root` (a no-op for any node a nested region already
    /// claimed) and returns parentless minting to the outer arena.
    /// `content_start` is `tree.count()` captured right after `begin_region`.
    pub fn end_region(&mut self, token: RegionToken, content_start: usize) {
        let count = self
            .region_len(token.root.region())
            .saturating_sub(content_start);
        self.reparent_root_nodes(content_start, count, token.root);
        self.open_regions = self.open_regions.saturating_sub(1);
        self.building = token.outer;
    }

    /// D4's second enforcement leg: true iff every root-parented node is a
    /// registered region root — no top-level subtree escaped `begin_region`.
    pub fn all_roots_are_regions(&self) -> bool {
        self.arenas.iter().all(|a| {
            (0..a.len()).all(|i| {
                a.parent_index[i].is_some() || a.sub_regions.iter().any(|s| s.root.slot() == i)
            })
        })
    }

    /// Reparent all root-level nodes in slots `[from, from + count)` of
    /// `new_parent`'s arena under `new_parent`.
    pub fn reparent_root_nodes(&mut self, from: usize, count: usize, new_parent: NodeId) {
        let region = new_parent.region();
        let end = (from + count).min(self.region_len(region));
        for i in from..end {
            let arena = &mut self.arenas[region as usize];
            if arena.parent_index[i].is_none() && i != new_parent.slot() {
                arena.parent_index[i] = Some(new_parent);
                let child = self.id_in(region, i);
                self.link_child(child, Some(new_parent));
            }
        }
    }

    /// Offset the Y position of node `id` and all its descendants by `dy`.
    /// Uses the first_child / next_sibling links — no allocation.
    pub fn offset_node_and_children(&mut self, id: NodeId, dy: f32) {
        let Some((arena, slot)) = self.live_mut(id) else {
            return;
        };
        Self::offset_subtree(arena, slot, dy);
    }

    fn offset_subtree(arena: &mut RegionArena, slot: usize, dy: f32) {
        arena.nodes[slot].bounds.y += dy;
        arena.nodes[slot].flags |= UIFlags::DIRTY;
        arena.has_dirty = true;
        let mut child = arena.first_child[slot];
        while let Some(c) = child {
            Self::offset_subtree(arena, c.slot(), dy);
            child = arena.next_sibling[c.slot()];
        }
    }

    // ── Mutation (O(1), marks dirty) ────────────────────────────────

    pub fn set_bounds(&mut self, id: NodeId, bounds: Rect) {
        let Some((arena, slot)) = self.live_mut(id) else {
            return;
        };
        let node = &mut arena.nodes[slot];
        if node.bounds == bounds {
            return;
        }
        node.bounds = bounds;
        node.flags |= UIFlags::DIRTY;
        arena.has_dirty = true;
    }

    pub fn set_style(&mut self, id: NodeId, style: UIStyle) {
        let Some((arena, slot)) = self.live_mut(id) else {
            return;
        };
        let node = &mut arena.nodes[slot];
        if node.style == style {
            return;
        }
        node.style = style;
        node.flags |= UIFlags::DIRTY;
        arena.has_dirty = true;
    }

    pub fn set_text(&mut self, id: NodeId, text: &str) {
        let Some((arena, slot)) = self.live_mut(id) else {
            return;
        };
        let node = &mut arena.nodes[slot];
        if node.text.as_deref() == Some(text) {
            return;
        }
        // In-place updates (a clock, a readout) reuse the node's buffer.
        match &mut node.text {
            Some(t) => {
                t.clear();
                t.push_str(text);
            }
            None => node.text = Some(text.to_owned()),
        }
        node.flags |= UIFlags::DIRTY;
        arena.has_dirty = true;
    }

    /// Draw this node's text in an installed font (see `UINode::font_family`).
    pub fn set_font_family(&mut self, id: NodeId, family: &str) {
        let Some((arena, slot)) = self.live_mut(id) else {
            return;
        };
        let node = &mut arena.nodes[slot];
        if node.font_family.as_deref() == Some(family) {
            return;
        }
        node.font_family = Some(family.to_string());
        node.flags |= UIFlags::DIRTY;
        arena.has_dirty = true;
    }

    pub fn set_flag(&mut self, id: NodeId, flag: UIFlags) {
        let Some((arena, slot)) = self.live_mut(id) else {
            return;
        };
        let node = &mut arena.nodes[slot];
        if node.flags.contains(flag) {
            return;
        }
        node.flags |= flag | UIFlags::DIRTY;
        arena.has_dirty = true;
    }

    pub fn clear_flag(&mut self, id: NodeId, flag: UIFlags) {
        let Some((arena, slot)) = self.live_mut(id) else {
            return;
        };
        let node = &mut arena.nodes[slot];
        if !node.flags.intersects(flag) {
            return;
        }
        node.flags.remove(flag);
        node.flags |= UIFlags::DIRTY;
        arena.has_dirty = true;
    }

    pub fn has_flag(&self, id: NodeId, flag: UIFlags) -> bool {
        self.get_node(id).is_some_and(|n| n.flags.contains(flag))
    }

    pub fn set_visible(&mut self, id: NodeId, visible: bool) {
        if visible {
            self.set_flag(id, UIFlags::VISIBLE);
        } else {
            self.clear_flag(id, UIFlags::VISIBLE);
        }
    }

    // ── Paint order (D7) ─────────────────────────────────────────────

    /// Visit every sub-region in paint order: tier ascending, then region
    /// rank (the region id), then local insertion. Fixed-size loops over a
    /// handful of entries; no sort, no allocation.
    fn for_each_sub_region(&self, mut f: impl FnMut(u8, usize)) {
        for tier in TIERS {
            for (r, arena) in self.arenas.iter().enumerate() {
                for (i, sub) in arena.sub_regions.iter().enumerate() {
                    if sub.tier == tier {
                        f(r as u8, i);
                    }
                }
            }
        }
    }

    // ── Hit testing (O(n × depth)) ─────────────────────────────────

    /// Find the topmost interactive, visible, non-disabled node at `pos`.
    /// Walks sub-regions in reverse paint order (tier descending, then region
    /// rank descending, then local insertion descending) and, inside one,
    /// reverse slot order — so an overlay, ghost or dropdown is hit before the
    /// panel it covers. Slots no sub-region owns (bare roots in a test tree)
    /// come last, arena by arena in reverse.
    pub fn hit_test(&self, pos: Vec2) -> Option<NodeId> {
        for tier in TIERS.into_iter().rev() {
            for arena in self.arenas.iter().rev() {
                for i in (0..arena.sub_regions.len()).rev() {
                    if arena.sub_regions[i].tier != tier {
                        continue;
                    }
                    let (start, end) = arena.sub_span(i);
                    if let Some(hit) = Self::hit_span(arena, start, end, pos) {
                        return Some(hit);
                    }
                }
            }
        }
        for arena in self.arenas.iter().rev() {
            let unowned = arena
                .sub_regions
                .first()
                .map_or(arena.len(), |s| s.root.slot());
            if let Some(hit) = Self::hit_span(arena, 0, unowned, pos) {
                return Some(hit);
            }
        }
        None
    }

    fn hit_span(arena: &RegionArena, start: usize, end: usize, pos: Vec2) -> Option<NodeId> {
        let required = UIFlags::VISIBLE | UIFlags::INTERACTIVE;
        for i in (start..end).rev() {
            let n = &arena.nodes[i];
            if !n.flags.contains(required)
                || n.flags.contains(UIFlags::DISABLED)
                || !n.bounds.contains(pos)
            {
                continue;
            }
            if !Self::is_inside_clip_ancestors(arena, i, pos) {
                continue;
            }
            return Some(n.id);
        }
        None
    }

    /// Walk up the parent chain to verify the point is inside all
    /// ClipsChildren ancestor bounds.
    fn is_inside_clip_ancestors(arena: &RegionArena, slot: usize, pos: Vec2) -> bool {
        let mut pid = arena.parent_index[slot];
        while let Some(p) = pid {
            let p = p.slot();
            if arena.nodes[p].flags.contains(UIFlags::CLIPS_CHILDREN)
                && !arena.nodes[p].bounds.contains(pos)
            {
                return false;
            }
            pid = arena.parent_index[p];
        }
        true
    }

    // ── Dirty tracking ─────────────────────────────────────────────

    pub fn mark_all_dirty(&mut self) {
        for arena in &mut self.arenas {
            for n in &mut arena.nodes {
                n.flags |= UIFlags::DIRTY;
            }
            arena.has_dirty = !arena.nodes.is_empty();
        }
    }

    pub fn clear_dirty(&mut self) {
        for arena in &mut self.arenas {
            for n in &mut arena.nodes {
                n.flags.remove(UIFlags::DIRTY);
            }
            arena.has_dirty = false;
        }
    }

    /// Whether any node in `region`'s arena is dirty.
    pub fn has_dirty_in_region(&self, region: u8) -> bool {
        self.arenas[region as usize].has_dirty
    }

    /// Check if any node in slots `[start, end)` of `region` has the DIRTY flag.
    pub fn has_dirty_in_range(&self, region: u8, start: usize, end: usize) -> bool {
        let nodes = &self.arenas[region as usize].nodes;
        let end = end.min(nodes.len());
        nodes[start.min(end)..end]
            .iter()
            .any(|n| n.flags.contains(UIFlags::DIRTY))
    }

    /// True if any DIRTY node in slots `[start, end)` of `region` lies OUTSIDE
    /// every range in `covered`. The panel cache uses this to spot dirt in a
    /// panel's chrome — nodes (tab strip, cog/Collapse, scrollbar) that sit in
    /// no sub-region and so are invisible to the incremental sub-region
    /// repaint path.
    pub fn has_dirty_outside_ranges(
        &self,
        region: u8,
        start: usize,
        end: usize,
        covered: &[(usize, usize)],
    ) -> bool {
        let nodes = &self.arenas[region as usize].nodes;
        let end = end.min(nodes.len());
        (start.min(end)..end).any(|i| {
            nodes[i].flags.contains(UIFlags::DIRTY)
                && !covered.iter().any(|&(s, e)| i >= s && i < e)
        })
    }

    /// Clear DIRTY on slots `[start, end)` of `region` and recompute the
    /// region's dirty flag.
    pub fn clear_dirty_range(&mut self, region: u8, start: usize, end: usize) {
        let arena = &mut self.arenas[region as usize];
        let end = end.min(arena.len());
        for n in &mut arena.nodes[start.min(end)..end] {
            n.flags.remove(UIFlags::DIRTY);
        }
        arena.recompute_dirty();
    }

    // ── Rendering traversal (recursive DFS) ─────────────────────────

    /// Walk the tree in paint order and call the visitor for each visible
    /// node, with push/pop clip signals: every sub-region of every arena by
    /// (tier, region rank, local insertion).
    pub fn traverse<F>(&self, mut visitor: F)
    where
        F: FnMut(TraversalEvent),
    {
        self.for_each_sub_region(|r, i| {
            let arena = &self.arenas[r as usize];
            Self::traverse_subtree(arena, arena.sub_regions[i].root.slot(), &mut visitor);
        });
    }

    /// Walk only `region`'s sub-regions whose roots sit in slots
    /// `[start, end)`, and their subtrees, in tier order. Used for overlay
    /// and ghost rendering and the editor's base-content scan.
    pub fn traverse_range<F>(&self, region: u8, start: usize, end: usize, mut visitor: F)
    where
        F: FnMut(TraversalEvent),
    {
        let arena = &self.arenas[region as usize];
        for tier in TIERS {
            for sub in arena.sub_regions.iter().filter(|s| s.tier == tier) {
                let slot = sub.root.slot();
                if slot >= start && slot < end {
                    Self::traverse_subtree(arena, slot, &mut visitor);
                }
            }
        }
    }

    /// Flat sequential iteration over slots `[start, end)` of `region` —
    /// visits ALL visible nodes regardless of parentage. Nodes are stored in
    /// DFS pre-order (insertion order), so sequential iteration produces
    /// correct draw order.
    ///
    /// Handles clip regions by tracking which CLIPS_CHILDREN ancestors are
    /// active, and pre-pushes the clips of ancestors outside the range, so a
    /// sub-panel (one effect card) draws inside its scroll container.
    ///
    /// When `dirty_only` is true, only emits `Node` events for dirty nodes,
    /// but always processes clip push/pop for correct clipping of dirty children.
    pub fn traverse_flat_range<F>(
        &self,
        region: u8,
        start: usize,
        end: usize,
        dirty_only: bool,
        mut visitor: F,
    ) where
        F: FnMut(TraversalEvent),
    {
        let arena = &self.arenas[region as usize];
        let end = end.min(arena.len());
        // Stack of (slot, bounds) for active CLIPS_CHILDREN ancestors.
        let mut clip_stack: Vec<(usize, Rect)> = Vec::new();

        if start < arena.len() {
            let mut ancestors: Vec<(usize, Rect)> = Vec::new();
            let mut idx = arena.parent_index[start];
            while let Some(node_id) = idx {
                let node = &arena.nodes[node_id.slot()];
                if node.flags.contains(UIFlags::CLIPS_CHILDREN) {
                    ancestors.push((node_id.slot(), node.bounds));
                }
                idx = arena.parent_index[node_id.slot()];
            }
            // Push outermost first (reverse order of discovery).
            for &(ci, bounds) in ancestors.iter().rev() {
                visitor(TraversalEvent::PushClip(bounds));
                clip_stack.push((ci, bounds));
            }
        }

        for i in start..end {
            let node = &arena.nodes[i];
            if !node.flags.contains(UIFlags::VISIBLE) {
                continue;
            }

            // Pop clip regions for ancestors we've moved past.
            while let Some(&(clip_idx, _)) = clip_stack.last() {
                if Self::is_ancestor_of(arena, clip_idx, i) {
                    break;
                }
                clip_stack.pop();
                visitor(TraversalEvent::PopClip);
            }

            if !dirty_only || node.flags.contains(UIFlags::DIRTY) {
                visitor(TraversalEvent::Node(node));
            }

            if node.flags.contains(UIFlags::CLIPS_CHILDREN) {
                visitor(TraversalEvent::PushClip(node.bounds));
                clip_stack.push((i, node.bounds));
            }
        }

        for _ in &clip_stack {
            visitor(TraversalEvent::PopClip);
        }
    }

    /// Check if `ancestor` is an ancestor of `descendant` (both slots of one
    /// arena) by walking the parent chain. False if they are equal.
    fn is_ancestor_of(arena: &RegionArena, ancestor: usize, descendant: usize) -> bool {
        let mut current = arena.parent_index[descendant];
        while let Some(c) = current {
            if c.slot() == ancestor {
                return true;
            }
            current = arena.parent_index[c.slot()];
        }
        false
    }

    fn traverse_subtree<F>(arena: &RegionArena, slot: usize, visitor: &mut F)
    where
        F: FnMut(TraversalEvent),
    {
        let node = &arena.nodes[slot];
        if !node.flags.contains(UIFlags::VISIBLE) {
            return;
        }

        visitor(TraversalEvent::Node(node));

        let clipping = node.flags.contains(UIFlags::CLIPS_CHILDREN);
        if clipping {
            visitor(TraversalEvent::PushClip(node.bounds));
        }

        let mut child = arena.first_child[slot];
        while let Some(c) = child {
            Self::traverse_subtree(arena, c.slot(), visitor);
            child = arena.next_sibling[c.slot()];
        }

        if clipping {
            visitor(TraversalEvent::PopClip);
        }
    }

    /// Get the parent of a node, or `None` for a root / stale id.
    pub fn parent_of(&self, id: NodeId) -> Option<NodeId> {
        if !self.is_live(id) {
            return None;
        }
        self.arenas[id.region() as usize].parent_index[id.slot()]
    }

    // ── Durable widget identity ─────────────────────────────────────

    /// The durable [`WidgetId`] of `id`, or [`WidgetId::NONE`] for a stale id.
    /// Stable across rebuilds, so the input system can capture it at press and
    /// recognise the same widget at release even after the tree was rebuilt.
    pub fn widget_of(&self, id: NodeId) -> WidgetId {
        if !self.is_live(id) {
            return WidgetId::NONE;
        }
        self.arenas[id.region() as usize].widget_ids[id.slot()]
    }

    /// The live [`NodeId`] that carries `widget` in the *current* build, if any
    /// interactive node does — how a held `WidgetId` (press / hover / focus)
    /// resolves back to a node the input system can act on.
    pub fn node_for_widget(&self, widget: WidgetId) -> Option<NodeId> {
        if widget == WidgetId::NONE {
            return None;
        }
        self.widget_to_node.get(&widget).copied()
    }

    // ── Automation component names (D8, section 3) ──────────────────────────

    /// Register `id`'s durable component name (e.g. `"layer_header.mute"`) —
    /// the automation selector surface's `name` field. Accepts `&'static str`
    /// literals and owned `String`s (per-row identity names); owned names die
    /// with the rebuild. A no-op (debug-asserts) on a stale/invalid id.
    pub fn set_name(&mut self, id: NodeId, name: impl Into<Box<str>>) {
        debug_assert!(
            self.is_live(id),
            "set_name on a stale/invalid NodeId (region {} slot {} gen {})",
            id.region(),
            id.slot(),
            id.generation()
        );
        if let Some((arena, slot)) = self.live_mut(id) {
            arena.names[slot] = Some(name.into());
        }
    }

    /// `id`'s registered component name, or `None` if it was never named (the
    /// common case) or `id` is stale. Read by the automation dump (section 3).
    pub fn name_of(&self, id: NodeId) -> Option<&str> {
        if !self.is_live(id) {
            return None;
        }
        self.arenas[id.region() as usize].names[id.slot()].as_deref()
    }

    // ── Clear ───────────────────────────────────────────────────────

    /// Drop every region's nodes. `gen_counter` keeps climbing, so ids minted
    /// after this never collide with ids from before it.
    pub fn clear(&mut self) {
        for arena in &mut self.arenas {
            arena.clear();
        }
        // Retain the map's capacity — the editor clears+refills it every frame.
        self.widget_to_node.clear();
        // `open_regions` and `building` are NOT reset: a `clear()` mid-build
        // would otherwise silently drop the bracket a caller still holds a
        // token for.
        self.structure_version = self.structure_version.wrapping_add(1);
    }

    /// Drop `region`'s nodes only (D7): every other region keeps its nodes,
    /// ids and widget entries. Its next build reproduces the same slots and
    /// root salts, so its `WidgetId`s are stable across the rebuild.
    pub fn clear_region(&mut self, region: u8) {
        let arena = &mut self.arenas[region as usize];
        for (n, w) in arena.nodes.iter().zip(&arena.widget_ids) {
            if n.flags.contains(UIFlags::INTERACTIVE) {
                self.widget_to_node.remove(w);
            }
        }
        arena.clear();
        self.structure_version = self.structure_version.wrapping_add(1);
    }

    /// Give a region's root clip a new rect, for a region its panel laid out
    /// again in place at a new rect.
    pub fn set_region_rect(&mut self, region: u8, rect: Rect) {
        if let Some(root) = self.arenas[region as usize]
            .sub_regions
            .first()
            .map(|s| s.root)
        {
            self.set_bounds(root, rect);
        }
    }

    /// Move every node of `region` by `(dx, dy)` in place (D7): a region whose
    /// rect moved but kept its size is translated, never rebuilt.
    pub fn translate_region(&mut self, region: u8, dx: f32, dy: f32) {
        let arena = &mut self.arenas[region as usize];
        for n in &mut arena.nodes {
            n.bounds.x += dx;
            n.bounds.y += dy;
            n.flags |= UIFlags::DIRTY;
        }
        arena.has_dirty = !arena.nodes.is_empty();
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
        assert_eq!(root.slot(), 0);
        assert_eq!(root.region(), DEFAULT_REGION);
        assert_eq!(tree.count(), 1);

        let child = tree.add_button(Some(root), 10.0, 10.0, 100.0, 30.0, default_style(), "OK");
        assert_eq!(child.slot(), 1);
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
        tree.translate_region(DEFAULT_REGION, 3.0, 4.0);
        assert_eq!(
            tree.structure_version(),
            v1,
            "set_* mutations and translation must not bump structure_version"
        );

        // clear_region + clear are structural.
        tree.clear_region(DEFAULT_REGION);
        let v2 = tree.structure_version();
        assert!(v2 > v1, "clear_region must bump structure_version");
        tree.clear();
        assert!(
            tree.structure_version() > v2,
            "clear must bump structure_version"
        );
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
        // `traverse()` visits registered regions, not a raw root scan.
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

        let slots: Vec<usize> = order.iter().map(|n| n.slot()).collect();
        assert_eq!(slots, vec![0, 1, 2]); // DFS pre-order: region root, A, B
    }

    /// D4's second enforcement leg (`all_roots_are_regions`), positive case.
    #[test]
    fn all_roots_are_regions_holds_for_a_properly_regioned_tree() {
        let mut tree = UITree::new();

        let chrome = tree.begin_region(
            Rect::new(0.0, 0.0, 800.0, 40.0),
            ZTier::Chrome,
            "chrome",
            UIFlags::empty(),
        );
        let start = tree.count();
        tree.add_panel(None, 0.0, 0.0, 100.0, 20.0, default_style());
        tree.add_panel(None, 0.0, 20.0, 100.0, 20.0, default_style());
        tree.end_region(chrome, start);

        let base = tree.begin_region_in(
            2,
            Rect::new(0.0, 40.0, 800.0, 500.0),
            ZTier::Base,
            "base",
            UIFlags::empty(),
        );
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

    /// D4's second enforcement leg, negative case: a stray root-parented node
    /// created with no `begin_region` bracket open is caught.
    #[test]
    fn all_roots_are_regions_catches_a_stray_root() {
        let mut tree = UITree::new();
        let region = tree.begin_region(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            ZTier::Base,
            "base",
            UIFlags::empty(),
        );
        let start = tree.count();
        tree.add_panel(None, 0.0, 0.0, 50.0, 50.0, default_style());
        tree.end_region(region, start);
        assert!(
            tree.all_roots_are_regions(),
            "sanity: the well-formed part passes"
        );

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

        // Can re-add after clear — slots restart from 0 (generation differs)
        let id = tree.add_panel(None, 0.0, 0.0, 100.0, 100.0, default_style());
        assert_eq!(id.slot(), 0);
    }

    #[test]
    fn offset_nodes() {
        let mut tree = UITree::new();
        tree.add_panel(None, 0.0, 0.0, 100.0, 20.0, default_style());
        tree.add_panel(None, 0.0, 20.0, 100.0, 20.0, default_style());
        tree.add_panel(None, 0.0, 40.0, 100.0, 20.0, default_style());

        tree.offset_nodes(DEFAULT_REGION, 1, 2, 100.0);

        assert_eq!(tree.get_bounds(tree.id_at(0)).y, 0.0);
        assert_eq!(tree.get_bounds(tree.id_at(1)).y, 120.0);
        assert_eq!(tree.get_bounds(tree.id_at(2)).y, 140.0);
    }

    /// Build a region the way a window's panel build does: clear it, open it,
    /// build content flat, sweep it under the root. Returns the button ids.
    fn build_region(tree: &mut UITree, region: u8, tier: ZTier, x: f32) -> Vec<NodeId> {
        tree.clear_region(region);
        let token = tree.begin_region_in(
            region,
            Rect::new(x, 0.0, 100.0, 100.0),
            tier,
            "r",
            UIFlags::empty(),
        );
        let start = tree.count();
        let row = tree.add_panel(None, x, 0.0, 100.0, 20.0, default_style());
        let a = tree.add_button(Some(row), x, 0.0, 50.0, 20.0, default_style(), "a");
        let b = tree.add_button(None, x, 40.0, 50.0, 20.0, default_style(), "b");
        tree.end_region(token, start);
        vec![a, b]
    }

    /// A remembered range stops owning its slots once its region is rebuilt,
    /// even though the new build puts nodes at exactly those slots.
    #[test]
    fn range_owns_rejects_a_range_from_an_earlier_build() {
        let mut tree = UITree::new();
        let old = build_region(&mut tree, 1, ZTier::Base, 0.0);
        let (first, count) = (old[0], old[1].slot() - old[0].slot() + 1);
        assert!(old.iter().all(|&id| tree.range_owns(first, count, id)));

        let new = build_region(&mut tree, 1, ZTier::Base, 0.0);
        assert_eq!(new[0].slot(), first.slot(), "the rebuild reuses the slots");
        assert!(new.iter().all(|&id| !tree.range_owns(first, count, id)));
        assert!(new.iter().all(|&id| tree.range_owns(new[0], count, id)));
        let other = build_region(&mut tree, 2, ZTier::Base, 200.0);
        assert!(
            !tree.range_owns(new[0], count, other[0]),
            "another region's slot is not ours"
        );
    }

    /// INV-14: rebuilding one region touches only its own arena. The other
    /// regions' ids stay live, their nodes keep their state, and their widget
    /// entries still resolve.
    #[test]
    fn clear_region_keeps_other_regions_live() {
        let mut tree = UITree::new();
        let left = build_region(&mut tree, 0, ZTier::Base, 0.0);
        let mid = build_region(&mut tree, 1, ZTier::Base, 200.0);
        let right = build_region(&mut tree, 2, ZTier::Chrome, 400.0);
        tree.set_text(left[0], "kept");
        tree.clear_dirty();
        let v = tree.structure_version();

        let rebuilt = build_region(&mut tree, 1, ZTier::Base, 200.0);

        assert!(tree.structure_version() > v);
        for &id in left.iter().chain(&right) {
            assert!(
                tree.is_live(id),
                "a region rebuild left {id:?} in another region alone"
            );
            assert_eq!(tree.node_for_widget(tree.widget_of(id)), Some(id));
        }
        assert_eq!(
            tree.get_node(left[0]).unwrap().text.as_deref(),
            Some("kept")
        );
        assert!(
            !tree.has_dirty_in_region(0),
            "an untouched region stays clean"
        );
        assert!(!tree.has_dirty_in_region(2));
        for &id in &mid {
            assert!(!tree.is_live(id), "the rebuilt region's old ids are stale");
        }
        for (&old, &new) in mid.iter().zip(&rebuilt) {
            assert_eq!(tree.node_for_widget(tree.widget_of(new)), Some(new));
            assert_eq!(
                old.slot(),
                new.slot(),
                "a region rebuild reproduces its slots"
            );
        }
        assert_eq!(tree.region_len(1), 4, "root, row and two buttons");
        assert!(tree.all_roots_are_regions());
    }

    /// Salts are seeded by region id and counted per arena, so a region
    /// rebuilt alone reproduces every `WidgetId` it had (BUG-bluz6 (per-region
    /// UI tree storage)), including after an earlier root was reparented —
    /// the case that once re-issued a consumed root salt.
    #[test]
    fn region_rebuild_keeps_widget_ids() {
        let mut tree = UITree::new();
        let _chrome = build_region(&mut tree, 0, ZTier::Chrome, 0.0);
        let before = build_region(&mut tree, 3, ZTier::Base, 200.0);
        let _after = build_region(&mut tree, 6, ZTier::Base, 400.0);
        let widgets: Vec<_> = before.iter().map(|&id| tree.widget_of(id)).collect();

        for _ in 0..3 {
            let rebuilt = build_region(&mut tree, 3, ZTier::Base, 200.0);
            let again: Vec<_> = rebuilt.iter().map(|&id| tree.widget_of(id)).collect();
            assert_eq!(
                widgets, again,
                "a region rebuild must reproduce its widget ids"
            );
        }
        // A full rebuild in the same order gives the same ids too.
        tree.clear();
        build_region(&mut tree, 0, ZTier::Chrome, 0.0);
        let full = build_region(&mut tree, 3, ZTier::Base, 200.0);
        assert_eq!(
            widgets,
            full.iter()
                .map(|&id| tree.widget_of(id))
                .collect::<Vec<_>>()
        );
    }

    /// INV-18: identical builds in different regions never share a `WidgetId`,
    /// before or after one region rebuilds.
    #[test]
    fn widget_ids_unique_across_regions() {
        let mut tree = UITree::new();
        for region in 0..MAX_REGIONS as u8 {
            build_region(&mut tree, region, ZTier::Base, 0.0);
        }
        let unique = |tree: &UITree| {
            let mut ids: Vec<_> = tree.all_nodes().map(|n| tree.widget_of(n.id)).collect();
            let total = ids.len();
            ids.sort();
            ids.dedup();
            ids.len() == total
        };
        assert!(
            unique(&tree),
            "two live nodes share a WidgetId across regions"
        );
        build_region(&mut tree, 7, ZTier::Base, 0.0);
        assert!(
            unique(&tree),
            "a region rebuild re-issued another region's WidgetId"
        );
    }

    /// The hit-order rule: reverse paint order — tier descending, then region
    /// rank descending, then local insertion descending — whatever order the
    /// regions were built or rebuilt in.
    #[test]
    fn hit_test_follows_region_paint_order() {
        let mut tree = UITree::new();
        // Built out of rank order: rank 5 Base, rank 2 Chrome, rank 1 Base.
        let base_hi = build_region(&mut tree, 5, ZTier::Base, 0.0);
        let chrome = build_region(&mut tree, 2, ZTier::Chrome, 0.0);
        let base_lo = build_region(&mut tree, 1, ZTier::Base, 0.0);
        let at = Vec2::new(10.0, 10.0);
        assert_eq!(
            tree.hit_test(at),
            Some(chrome[0]),
            "Chrome tier beats Base regardless of rank"
        );

        tree.clear_region(2);
        assert_eq!(
            tree.hit_test(at),
            Some(base_hi[0]),
            "higher rank wins inside a tier"
        );

        tree.clear_region(5);
        assert_eq!(tree.hit_test(at), Some(base_lo[0]));

        // Two sub-regions in one arena: the later insertion is on top.
        let token = tree.begin_region_in(
            1,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            ZTier::Base,
            "late",
            UIFlags::empty(),
        );
        let start = tree.count();
        let late = tree.add_button(None, 0.0, 0.0, 50.0, 20.0, default_style(), "late");
        tree.end_region(token, start);
        assert_eq!(tree.hit_test(at), Some(late));

        // Paint order matches: the walk visits rank 1 sub-regions in insertion order.
        let mut texts = Vec::new();
        tree.traverse(|ev| {
            if let TraversalEvent::Node(n) = ev
                && let Some(t) = n.text.as_deref()
            {
                texts.push(t.to_string());
            }
        });
        assert_eq!(texts, ["a", "b", "late"]);
    }

    #[test]
    fn translate_region_moves_only_that_region() {
        let mut tree = UITree::new();
        let a = build_region(&mut tree, 0, ZTier::Base, 0.0);
        let b = build_region(&mut tree, 1, ZTier::Base, 200.0);
        tree.clear_dirty();
        tree.translate_region(1, 5.0, -3.0);
        assert_eq!(tree.get_bounds(a[0]).x, 0.0);
        assert_eq!(tree.get_bounds(b[0]).x, 205.0);
        assert_eq!(tree.get_bounds(b[0]).y, -3.0);
        assert!(tree.has_dirty_in_region(1) && !tree.has_dirty_in_region(0));
        assert!(tree.is_live(b[0]), "translation keeps ids");
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
        // Even with a node at slot 0, the placeholder (generation 0) doesn't match.
        assert!(!tree.is_live(NodeId::PLACEHOLDER));
    }

    #[test]
    fn ids_at_same_slot_differ_by_generation_across_clear() {
        let mut tree = UITree::new();
        let before = tree.add_panel(None, 0.0, 0.0, 10.0, 10.0, default_style());
        tree.clear();
        let after = tree.add_panel(None, 0.0, 0.0, 10.0, 10.0, default_style());
        assert_eq!(before.slot(), after.slot(), "both reuse slot 0");
        assert_ne!(before, after, "but the generation must differ");
        assert!(!tree.is_live(before), "the pre-clear id is stale");
        assert!(tree.is_live(after));
    }

    #[test]
    fn stale_id_after_clear_region_is_inert() {
        let mut tree = UITree::new();
        let keep = build_region(&mut tree, 0, ZTier::Base, 0.0)[0];
        let doomed = build_region(&mut tree, 1, ZTier::Base, 0.0)[0];
        // The slot is reused by a different node.
        let reused = build_region(&mut tree, 1, ZTier::Base, 0.0)[0];
        assert_eq!(doomed.slot(), reused.slot(), "slot reused");
        tree.set_text(reused, "new");

        // Reads through the stale id are inert — never the new occupant.
        assert!(!tree.is_live(doomed));
        assert_eq!(tree.get_bounds(doomed), Rect::ZERO);
        assert!(!tree.has_flag(doomed, UIFlags::VISIBLE));

        // Writes through the stale id are no-ops — the new occupant is untouched.
        tree.set_text(doomed, "hijacked");
        assert_eq!(tree.get_node(reused).unwrap().text.as_deref(), Some("new"));
        tree.set_bounds(doomed, Rect::new(99.0, 99.0, 1.0, 1.0));
        assert_eq!(tree.get_bounds(reused).y, 0.0);

        // The other region's node still validates.
        assert!(tree.is_live(keep));
    }

    /// `get_node`/`get_node_mut` are the accessors a cached `NodeId` field
    /// goes through. They must give the same `None`/no-op inertness as every
    /// other accessor here.
    #[test]
    fn get_node_on_stale_id_is_none_not_a_panic() {
        let mut tree = UITree::new();
        let doomed = tree.add_label(None, 0.0, 0.0, 10.0, 10.0, "old", default_style());
        tree.clear_region(DEFAULT_REGION);

        assert!(tree.get_node(doomed).is_none());
        assert!(tree.get_node_mut(doomed).is_none());
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
        assert_eq!(tree.id_in(DEFAULT_REGION, 1), b);
        // Out of range → placeholder (never live).
        assert_eq!(tree.id_at(99), NodeId::PLACEHOLDER);
        assert_eq!(tree.id_in(3, 0), NodeId::PLACEHOLDER);
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
        let _inserted = tree.add_button(Some(root), 0.0, 0.0, 10.0, 10.0, default_style(), "new");
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
        assert_eq!(
            tree.widget_of(keyed_b),
            keyed_w_a,
            "explicit key is reorder-stable"
        );
        assert_ne!(
            tree.widget_of(auto_b),
            auto_w_a,
            "auto sibling-index identity shifts when an earlier sibling is inserted"
        );
    }
}
