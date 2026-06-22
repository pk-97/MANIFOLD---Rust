use crate::node::*;
use crate::text::{HeuristicTextMeasure, TextMeasure};

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
            count: 0,
            has_dirty: false,
            structure_version: 0,
            text_measure: Box::new(HeuristicTextMeasure),
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
        let id = NodeId(self.count as u32);

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

    pub fn get_node(&self, id: NodeId) -> &UINode {
        &self.nodes[id.index()]
    }

    pub fn get_node_mut(&mut self, id: NodeId) -> &mut UINode {
        &mut self.nodes[id.index()]
    }

    pub fn get_bounds(&self, id: NodeId) -> Rect {
        if id.index() >= self.count {
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

    /// Reparent all root-level nodes (parent_id == -1) in the index range
    /// [from_index..from_index+count) under the given parent.
    /// Used by the inspector to wrap built sub-panel nodes under a ClipRegion.
    pub fn reparent_root_nodes(&mut self, from_index: usize, count: usize, new_parent: NodeId) {
        let end = (from_index + count).min(self.count);
        for i in from_index..end {
            if self.parent_index[i].is_none() {
                self.parent_index[i] = Some(new_parent);
                self.link_child(NodeId(i as u32), Some(new_parent));
            }
        }
    }

    /// Offset the Y position of node `id` and all its descendants by `dy`.
    /// Uses the existing first_child / next_sibling linked list — no allocation.
    /// Nesting depth is bounded by tree depth (max ~2 in practice).
    pub fn offset_node_and_children(&mut self, id: NodeId, dy: f32) {
        let idx = id.index();
        if idx >= self.count {
            return;
        }
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
        let idx = id.index();
        if idx >= self.count {
            return;
        }
        if self.nodes[idx].bounds == bounds {
            return;
        }
        self.nodes[idx].bounds = bounds;
        self.nodes[idx].flags |= UIFlags::DIRTY;
        self.has_dirty = true;
    }

    pub fn set_style(&mut self, id: NodeId, style: UIStyle) {
        let idx = id.index();
        if idx >= self.count {
            return;
        }
        if self.nodes[idx].style == style {
            return;
        }
        self.nodes[idx].style = style;
        self.nodes[idx].flags |= UIFlags::DIRTY;
        self.has_dirty = true;
    }

    pub fn set_text(&mut self, id: NodeId, text: &str) {
        let idx = id.index();
        if idx >= self.count {
            return;
        }
        if self.nodes[idx].text.as_deref() == Some(text) {
            return;
        }
        self.nodes[idx].text = Some(text.to_string());
        self.nodes[idx].flags |= UIFlags::DIRTY;
        self.has_dirty = true;
    }

    pub fn set_flag(&mut self, id: NodeId, flag: UIFlags) {
        let idx = id.index();
        if idx >= self.count {
            return;
        }
        if self.nodes[idx].flags.contains(flag) {
            return;
        }
        self.nodes[idx].flags |= flag | UIFlags::DIRTY;
        self.has_dirty = true;
    }

    pub fn clear_flag(&mut self, id: NodeId, flag: UIFlags) {
        let idx = id.index();
        if idx >= self.count {
            return;
        }
        if !self.nodes[idx].flags.intersects(flag) {
            return;
        }
        self.nodes[idx].flags.remove(flag);
        self.nodes[idx].flags |= UIFlags::DIRTY;
        self.has_dirty = true;
    }

    pub fn has_flag(&self, id: NodeId, flag: UIFlags) -> bool {
        let idx = id.index();
        if idx >= self.count {
            return false;
        }
        self.nodes[idx].flags.contains(flag)
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
    pub fn traverse<F>(&self, mut visitor: F)
    where
        F: FnMut(TraversalEvent),
    {
        for i in 0..self.count {
            if self.parent_index[i].is_none() {
                self.traverse_subtree(i, &mut visitor);
            }
        }
    }

    /// Walk only root nodes in `[start, end)` and their subtrees.
    /// Used for overlay rendering — avoids traversing the entire tree
    /// when only a small range of nodes needs to be drawn.
    pub fn traverse_range<F>(&self, start: usize, end: usize, mut visitor: F)
    where
        F: FnMut(TraversalEvent),
    {
        let end = end.min(self.count);
        for i in start..end {
            if self.parent_index[i].is_none() {
                self.traverse_subtree(i, &mut visitor);
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

    /// Get the parent of a node, or `None` for a root / out-of-range id.
    pub fn parent_of(&self, id: NodeId) -> Option<NodeId> {
        let idx = id.index();
        if idx >= self.count {
            return None;
        }
        self.parent_index[idx]
    }

    // ── Clear ───────────────────────────────────────────────────────

    pub fn clear(&mut self) {
        self.nodes.clear();
        self.parent_index.clear();
        self.first_child.clear();
        self.next_sibling.clear();
        self.last_child.clear();
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
        self.nodes.truncate(from_index);
        self.parent_index.truncate(from_index);
        self.first_child.truncate(from_index);
        self.next_sibling.truncate(from_index);
        self.last_child.truncate(from_index);
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
        assert_eq!(root, NodeId(0));
        assert_eq!(tree.count(), 1);

        let child = tree.add_button(Some(root), 10.0, 10.0, 100.0, 30.0, default_style(), "OK");
        assert_eq!(child, NodeId(1));
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
        let _root = tree.add_panel(None, 0.0, 0.0, 800.0, 600.0, default_style());
        // Two overlapping buttons — second (id=2) is on top
        let _btn1 = tree.add_button(Some(NodeId(0)), 50.0, 50.0, 100.0, 30.0, default_style(), "A");
        let btn2 = tree.add_button(Some(NodeId(0)), 50.0, 50.0, 100.0, 30.0, default_style(), "B");

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
        let _root = tree.add_panel(None, 0.0, 0.0, 800.0, 600.0, default_style());
        let _btn = tree.add_button(Some(NodeId(0)), 50.0, 50.0, 100.0, 30.0, default_style(), "A");

        let hit = tree.hit_test(Vec2::new(200.0, 200.0));
        assert_eq!(hit, None);
    }

    #[test]
    fn hit_test_respects_disabled() {
        let mut tree = UITree::new();
        let _root = tree.add_panel(None, 0.0, 0.0, 800.0, 600.0, default_style());
        let btn = tree.add_button(Some(NodeId(0)), 50.0, 50.0, 100.0, 30.0, default_style(), "A");
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
        assert_eq!(tree.get_node(id).text.as_deref(), Some("World"));
    }

    #[test]
    fn traversal_order() {
        let mut tree = UITree::new();
        let root = tree.add_panel(None, 0.0, 0.0, 800.0, 600.0, default_style());
        let _a = tree.add_label(Some(root), 0.0, 0.0, 100.0, 20.0, "A", default_style());
        let _b = tree.add_label(Some(root), 0.0, 20.0, 100.0, 20.0, "B", default_style());

        let mut order = Vec::new();
        tree.traverse(|event| {
            if let TraversalEvent::Node(node) = event {
                order.push(node.id);
            }
        });

        assert_eq!(order, vec![NodeId(0), NodeId(1), NodeId(2)]); // DFS pre-order
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

        // Can re-add after clear
        let id = tree.add_panel(None, 0.0, 0.0, 100.0, 100.0, default_style());
        assert_eq!(id, NodeId(0)); // IDs restart from 0
    }

    #[test]
    fn offset_nodes() {
        let mut tree = UITree::new();
        tree.add_panel(None, 0.0, 0.0, 100.0, 20.0, default_style());
        tree.add_panel(None, 0.0, 20.0, 100.0, 20.0, default_style());
        tree.add_panel(None, 0.0, 40.0, 100.0, 20.0, default_style());

        tree.offset_nodes(1, 2, 100.0);

        assert_eq!(tree.get_bounds(NodeId(0)).y, 0.0);
        assert_eq!(tree.get_bounds(NodeId(1)).y, 120.0);
        assert_eq!(tree.get_bounds(NodeId(2)).y, 140.0);
    }

    #[test]
    fn truncate_from_preserves_earlier_nodes() {
        let mut tree = UITree::new();
        // Panel A: root + child
        let a_root = tree.add_panel(None, 0.0, 0.0, 100.0, 50.0, default_style());
        let _a_child = tree.add_label(Some(a_root), 10.0, 10.0, 80.0, 20.0, "A", default_style());
        let boundary = tree.count(); // = 2

        // Panel B: root + child
        let _b_root = tree.add_panel(None, 0.0, 50.0, 100.0, 50.0, default_style());
        let _b_child = tree.add_label(Some(NodeId(2)), 10.0, 60.0, 80.0, 20.0, "B", default_style());
        assert_eq!(tree.count(), 4);

        // Truncate at panel B boundary
        tree.truncate_from(boundary);
        assert_eq!(tree.count(), 2);
        assert!(tree.has_dirty());

        // Panel A nodes intact
        assert_eq!(tree.get_bounds(NodeId(0)).y, 0.0);
        assert_eq!(tree.get_node(NodeId(1)).text.as_deref(), Some("A"));

        // Can re-add after truncation — IDs continue from truncation point
        let new_id = tree.add_panel(None, 0.0, 100.0, 100.0, 50.0, default_style());
        assert_eq!(new_id, NodeId(2));
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
}
