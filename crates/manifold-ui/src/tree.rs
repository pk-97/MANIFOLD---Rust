use crate::node::*;

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
///   parent_index[] — parent's array index (-1 for roots)
///   first_child[]  — first child's index (-1 if leaf)
///   next_sibling[] — next sibling's index (-1 if last)
///   last_child[]   — last child's index (-1 if leaf), for O(1) appending
pub struct UITree {
    nodes: Vec<UINode>,
    parent_index: Vec<i32>,
    first_child: Vec<i32>,
    next_sibling: Vec<i32>,
    last_child: Vec<i32>,
    count: usize,
    has_dirty: bool,
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
        }
    }

    pub fn count(&self) -> usize {
        self.count
    }

    pub fn has_dirty(&self) -> bool {
        self.has_dirty
    }

    // ── Node creation ───────────────────────────────────────────────

    pub fn add_node(
        &mut self,
        parent_id: i32,
        bounds: Rect,
        node_type: UINodeType,
        style: UIStyle,
        text: Option<&str>,
        extra_flags: UIFlags,
    ) -> u32 {
        let id = self.count as u32;

        let node = UINode {
            id,
            parent_id,
            bounds,
            node_type,
            flags: UIFlags::VISIBLE | UIFlags::DIRTY | extra_flags,
            style,
            text: text.map(String::from),
            draw_order: self.count as i32,
        };

        self.nodes.push(node);
        self.parent_index.push(parent_id);
        self.first_child.push(-1);
        self.next_sibling.push(-1);
        self.last_child.push(-1);

        self.link_child(id as i32, parent_id);
        self.count += 1;
        self.has_dirty = true;
        id
    }

    fn link_child(&mut self, child_id: i32, parent_id: i32) {
        if parent_id < 0 {
            return;
        }
        let pid = parent_id as usize;
        if self.first_child[pid] == -1 {
            self.first_child[pid] = child_id;
        } else {
            let last = self.last_child[pid] as usize;
            self.next_sibling[last] = child_id;
        }
        self.last_child[pid] = child_id;
    }

    /// Add a panel node (non-interactive rect).
    pub fn add_panel(
        &mut self,
        parent_id: i32,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        style: UIStyle,
    ) -> u32 {
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
        parent_id: i32,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        style: UIStyle,
        text: &str,
    ) -> u32 {
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
        parent_id: i32,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        text: &str,
        style: UIStyle,
    ) -> u32 {
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
        parent_id: i32,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        style: UIStyle,
    ) -> u32 {
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

    pub fn get_node(&self, id: u32) -> &UINode {
        &self.nodes[id as usize]
    }

    pub fn get_node_mut(&mut self, id: u32) -> &mut UINode {
        &mut self.nodes[id as usize]
    }

    pub fn get_bounds(&self, id: u32) -> Rect {
        if (id as usize) >= self.count {
            return Rect::ZERO;
        }
        self.nodes[id as usize].bounds
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

    // ── Mutation (O(1), marks dirty) ────────────────────────────────

    pub fn set_bounds(&mut self, id: u32, bounds: Rect) {
        let idx = id as usize;
        if idx >= self.count {
            return;
        }
        self.nodes[idx].bounds = bounds;
        self.nodes[idx].flags |= UIFlags::DIRTY;
        self.has_dirty = true;
    }

    pub fn set_style(&mut self, id: u32, style: UIStyle) {
        let idx = id as usize;
        if idx >= self.count {
            return;
        }
        self.nodes[idx].style = style;
        self.nodes[idx].flags |= UIFlags::DIRTY;
        self.has_dirty = true;
    }

    pub fn set_text(&mut self, id: u32, text: &str) {
        let idx = id as usize;
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

    pub fn set_flag(&mut self, id: u32, flag: UIFlags) {
        let idx = id as usize;
        if idx >= self.count {
            return;
        }
        if self.nodes[idx].flags.contains(flag) {
            return;
        }
        self.nodes[idx].flags |= flag | UIFlags::DIRTY;
        self.has_dirty = true;
    }

    pub fn clear_flag(&mut self, id: u32, flag: UIFlags) {
        let idx = id as usize;
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

    pub fn has_flag(&self, id: u32, flag: UIFlags) -> bool {
        let idx = id as usize;
        if idx >= self.count {
            return false;
        }
        self.nodes[idx].flags.contains(flag)
    }

    pub fn set_visible(&mut self, id: u32, visible: bool) {
        if visible {
            self.set_flag(id, UIFlags::VISIBLE);
        } else {
            self.clear_flag(id, UIFlags::VISIBLE);
        }
    }

    // ── Hit testing (O(n × depth)) ─────────────────────────────────

    /// Find the topmost interactive, visible, non-disabled node at `pos`.
    /// Returns -1 if nothing hit. Walk reverse insertion order.
    pub fn hit_test(&self, pos: Vec2) -> i32 {
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
            return n.id as i32;
        }
        -1
    }

    /// Walk up the parent chain to verify the point is inside all
    /// ClipsChildren ancestor bounds.
    fn is_inside_clip_ancestors(&self, index: usize, pos: Vec2) -> bool {
        let mut pid = self.parent_index[index];
        while pid >= 0 {
            let p = pid as usize;
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

    // ── Rendering traversal (recursive DFS) ─────────────────────────

    /// Walk the tree in DFS order and call the visitor for each visible node.
    /// The visitor receives: (node, push_clip, pop_clip) signals.
    pub fn traverse<F>(&self, mut visitor: F)
    where
        F: FnMut(TraversalEvent),
    {
        for i in 0..self.count {
            if self.parent_index[i] == -1 {
                self.traverse_subtree(i, &mut visitor);
            }
        }
    }

    fn traverse_subtree<F>(&self, index: usize, visitor: &mut F)
    where
        F: FnMut(TraversalEvent),
    {
        let node = &self.nodes[index];
        let visible = node.flags.contains(UIFlags::VISIBLE);
        let clipping = visible && node.flags.contains(UIFlags::CLIPS_CHILDREN);

        if visible {
            visitor(TraversalEvent::Node(node));
        }

        if clipping {
            visitor(TraversalEvent::PushClip(node.bounds));
        }

        let mut child = self.first_child[index];
        while child >= 0 {
            self.traverse_subtree(child as usize, visitor);
            child = self.next_sibling[child as usize];
        }

        if clipping {
            visitor(TraversalEvent::PopClip);
        }
    }

    /// Get the parent id of a node.
    pub fn parent_of(&self, id: u32) -> i32 {
        let idx = id as usize;
        if idx >= self.count {
            return -1;
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

        let root = tree.add_panel(-1, 0.0, 0.0, 800.0, 600.0, default_style());
        assert_eq!(root, 0);
        assert_eq!(tree.count(), 1);

        let child = tree.add_button(root as i32, 10.0, 10.0, 100.0, 30.0, default_style(), "OK");
        assert_eq!(child, 1);
        assert_eq!(tree.count(), 2);
    }

    #[test]
    fn parent_child_relationships() {
        let mut tree = UITree::new();
        let root = tree.add_panel(-1, 0.0, 0.0, 800.0, 600.0, default_style());
        let a = tree.add_panel(root as i32, 0.0, 0.0, 400.0, 300.0, default_style());
        let b = tree.add_panel(root as i32, 400.0, 0.0, 400.0, 300.0, default_style());

        assert_eq!(tree.parent_of(root), -1);
        assert_eq!(tree.parent_of(a), root as i32);
        assert_eq!(tree.parent_of(b), root as i32);
    }

    #[test]
    fn hit_test_topmost() {
        let mut tree = UITree::new();
        let _root = tree.add_panel(-1, 0.0, 0.0, 800.0, 600.0, default_style());
        // Two overlapping buttons — second (id=2) is on top
        let _btn1 = tree.add_button(0, 50.0, 50.0, 100.0, 30.0, default_style(), "A");
        let btn2 = tree.add_button(0, 50.0, 50.0, 100.0, 30.0, default_style(), "B");

        let hit = tree.hit_test(Vec2::new(60.0, 60.0));
        assert_eq!(hit, btn2 as i32);
    }

    #[test]
    fn hit_test_miss() {
        let mut tree = UITree::new();
        let _root = tree.add_panel(-1, 0.0, 0.0, 800.0, 600.0, default_style());
        let _btn = tree.add_button(0, 50.0, 50.0, 100.0, 30.0, default_style(), "A");

        let hit = tree.hit_test(Vec2::new(200.0, 200.0));
        assert_eq!(hit, -1);
    }

    #[test]
    fn hit_test_respects_disabled() {
        let mut tree = UITree::new();
        let _root = tree.add_panel(-1, 0.0, 0.0, 800.0, 600.0, default_style());
        let btn = tree.add_button(0, 50.0, 50.0, 100.0, 30.0, default_style(), "A");
        tree.set_flag(btn, UIFlags::DISABLED);

        let hit = tree.hit_test(Vec2::new(60.0, 60.0));
        assert_eq!(hit, -1);
    }

    #[test]
    fn hit_test_respects_clip_ancestors() {
        let mut tree = UITree::new();
        // Clip region that only covers (0,0)-(50,50)
        let clip = tree.add_node(
            -1,
            Rect::new(0.0, 0.0, 50.0, 50.0),
            UINodeType::ClipRegion,
            default_style(),
            None,
            UIFlags::CLIPS_CHILDREN,
        );
        // Button extends past clip
        let _btn = tree.add_button(clip as i32, 0.0, 0.0, 200.0, 200.0, default_style(), "X");

        // Inside clip → hit
        assert!(tree.hit_test(Vec2::new(25.0, 25.0)) >= 0);
        // Outside clip → miss
        assert_eq!(tree.hit_test(Vec2::new(100.0, 100.0)), -1);
    }

    #[test]
    fn set_text_dedup() {
        let mut tree = UITree::new();
        let id = tree.add_label(-1, 0.0, 0.0, 100.0, 20.0, "Hello", default_style());
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
        let root = tree.add_panel(-1, 0.0, 0.0, 800.0, 600.0, default_style());
        let _a = tree.add_label(root as i32, 0.0, 0.0, 100.0, 20.0, "A", default_style());
        let _b = tree.add_label(root as i32, 0.0, 20.0, 100.0, 20.0, "B", default_style());

        let mut order = Vec::new();
        tree.traverse(|event| {
            if let TraversalEvent::Node(node) = event {
                order.push(node.id);
            }
        });

        assert_eq!(order, vec![0, 1, 2]); // DFS pre-order
    }

    #[test]
    fn clear_resets() {
        let mut tree = UITree::new();
        tree.add_panel(-1, 0.0, 0.0, 100.0, 100.0, default_style());
        tree.add_panel(-1, 0.0, 0.0, 100.0, 100.0, default_style());
        assert_eq!(tree.count(), 2);

        tree.clear();
        assert_eq!(tree.count(), 0);
        assert!(!tree.has_dirty());

        // Can re-add after clear
        let id = tree.add_panel(-1, 0.0, 0.0, 100.0, 100.0, default_style());
        assert_eq!(id, 0); // IDs restart from 0
    }

    #[test]
    fn offset_nodes() {
        let mut tree = UITree::new();
        tree.add_panel(-1, 0.0, 0.0, 100.0, 20.0, default_style());
        tree.add_panel(-1, 0.0, 20.0, 100.0, 20.0, default_style());
        tree.add_panel(-1, 0.0, 40.0, 100.0, 20.0, default_style());

        tree.offset_nodes(1, 2, 100.0);

        assert_eq!(tree.get_bounds(0).y, 0.0);
        assert_eq!(tree.get_bounds(1).y, 120.0);
        assert_eq!(tree.get_bounds(2).y, 140.0);
    }
}
