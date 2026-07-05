//! Layered ("Sugiyama"-style) auto-layout: column assignment by
//! dependency depth, crossing minimisation, and vertical coordinate
//! assignment. Pure geometry — owns `LayeredLayout` and the
//! `GraphCanvas` relayout entry points.

use super::*;

/// Median of a slice of values (mutates it by sorting). Returns `0.0` for an
/// empty slice. Used by both layout passes: the ordering pass takes the
/// median neighbour *position*, the coordinate pass the median target *y*.
pub(crate) fn layout_median(vals: &mut [f32]) -> f32 {
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
    let len = vals.len();
    if len == 0 {
        0.0
    } else if len % 2 == 1 {
        vals[len / 2]
    } else {
        0.5 * (vals[len / 2 - 1] + vals[len / 2])
    }
}

/// Push apart the `desired` y-positions of one column so adjacent vertices
/// keep `gap` of clearance and never reorder, then rigid-shift the whole
/// column back so its mean matches the mean of the requested positions. The
/// shift keeps the column centred where alignment wanted it instead of
/// drifting downward each pass. `desired[i]` pairs with `col[i]`.
pub(crate) fn layout_resolve_overlaps(col: &[usize], height: &[f32], desired: &mut [f32], gap: f32) {
    let len = col.len();
    if len == 0 {
        return;
    }
    let mean_before: f32 = desired.iter().sum::<f32>() / len as f32;
    for i in 1..len {
        let min_y = desired[i - 1] + height[col[i - 1]] + gap;
        if desired[i] < min_y {
            desired[i] = min_y;
        }
    }
    let mean_after: f32 = desired.iter().sum::<f32>() / len as f32;
    let shift = mean_before - mean_after;
    for d in desired.iter_mut() {
        *d += shift;
    }
}

/// A layered ("Sugiyama"-style) auto-layout. Nodes are assigned to
/// left-to-right columns by dependency depth (done by the caller), ordered
/// within each column to minimise wire crossings, then nudged vertically so
/// connected ports line up and wires run straight.
///
/// Vertices are *layout vertices*, addressed by `lvid`. The first `n` are the
/// real graph nodes (lvid == index into `GraphCanvas::nodes`); the rest are
/// virtual routing waypoints inserted for wires that span more than one
/// column, so a long wire participates in ordering and alignment instead of
/// slicing diagonally across the graph. Waypoints are discarded once the real
/// nodes' positions are read back.
pub(crate) struct LayeredLayout {
    pub(crate) num_cols: usize,
    /// Column index per layout vertex.
    pub(crate) column: Vec<usize>,
    /// Layout height per vertex (real node height, or `LAYOUT_DUMMY_H`).
    pub(crate) height: Vec<f32>,
    /// Vertices in each column, top to bottom. Mutated by the ordering pass.
    pub(crate) order: Vec<Vec<usize>>,
    /// Per vertex `v`, the segments arriving from the previous column:
    /// `(u, up_off, down_off)` where `u` sits one column left, `up_off` is the
    /// y-offset of the wire's exit port on `u`, and `down_off` its entry port
    /// on `v`. Alignment lines those two ports up, not the boxes.
    pub(crate) up_edges: Vec<Vec<(usize, f32, f32)>>,
    /// Mirror of `up_edges` pointing forward: segments leaving `v` toward the
    /// next column. `(w, up_off, down_off)` with `up_off` the exit port on `v`.
    pub(crate) down_edges: Vec<Vec<(usize, f32, f32)>>,
}

impl LayeredLayout {
    /// Position index (0 = top) of every vertex within its column.
    pub(crate) fn positions(&self) -> Vec<usize> {
        let mut pos = vec![0usize; self.column.len()];
        for col in &self.order {
            for (i, &v) in col.iter().enumerate() {
                pos[v] = i;
            }
        }
        pos
    }

    /// Total wire crossings across all adjacent column pairs, counted as
    /// inversions between the two endpoints' position indices. O(edges²) per
    /// column boundary — fine for graphs this size.
    pub(crate) fn count_crossings(&self) -> usize {
        let pos = self.positions();
        let mut total = 0;
        for c in 0..self.num_cols.saturating_sub(1) {
            let mut edges: Vec<(usize, usize)> = Vec::new();
            for &v in &self.order[c] {
                for &(w, _, _) in &self.down_edges[v] {
                    edges.push((pos[v], pos[w]));
                }
            }
            for i in 0..edges.len() {
                for j in (i + 1)..edges.len() {
                    let (a, b) = (edges[i], edges[j]);
                    if (a.0 < b.0 && a.1 > b.1) || (a.0 > b.0 && a.1 < b.1) {
                        total += 1;
                    }
                }
            }
        }
        total
    }

    /// Reorder one column by the median position of each vertex's neighbours
    /// in the adjacent column (`use_up` → look left, else look right).
    /// Vertices with no neighbour on that side keep their current slot, so
    /// they drift with — rather than against — their surroundings.
    pub(crate) fn order_column_by(&mut self, col: usize, use_up: bool) {
        let pos = self.positions();
        let mut keyed: Vec<(f32, usize, usize)> = Vec::with_capacity(self.order[col].len());
        for (idx, &v) in self.order[col].iter().enumerate() {
            let edges = if use_up {
                &self.up_edges[v]
            } else {
                &self.down_edges[v]
            };
            let mut np: Vec<f32> = edges.iter().map(|&(u, _, _)| pos[u] as f32).collect();
            let key = if np.is_empty() {
                idx as f32
            } else {
                layout_median(&mut np)
            };
            keyed.push((key, v, idx));
        }
        keyed.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(core::cmp::Ordering::Equal)
                .then(a.2.cmp(&b.2))
        });
        self.order[col] = keyed.into_iter().map(|(_, v, _)| v).collect();
    }

    /// Alternating up/down median sweeps; keep the best-scoring ordering seen.
    pub(crate) fn minimise_crossings(&mut self) {
        let mut best = self.order.clone();
        let mut best_cross = self.count_crossings();
        for it in 0..LAYOUT_ORDER_ITERS {
            if it % 2 == 0 {
                for c in 1..self.num_cols {
                    self.order_column_by(c, true);
                }
            } else {
                for c in (0..self.num_cols.saturating_sub(1)).rev() {
                    self.order_column_by(c, false);
                }
            }
            let cross = self.count_crossings();
            if cross < best_cross {
                best_cross = cross;
                best = self.order.clone();
                if cross == 0 {
                    break;
                }
            }
        }
        self.order = best;
    }

    /// Assign a top-edge y to every vertex. Starts by stacking each column,
    /// then runs alternating passes that pull each vertex toward the median
    /// height of the ports it connects to (resolving overlaps after each), so
    /// wires straighten. Returns y per lvid in an un-shifted frame.
    pub(crate) fn assign_y(&self) -> Vec<f32> {
        let mut y = vec![0.0f32; self.column.len()];
        for col in &self.order {
            let mut yy = 0.0;
            for &v in col {
                y[v] = yy;
                yy += self.height[v] + LAYOUT_VGAP;
            }
        }
        for pass in 0..LAYOUT_COORD_ITERS {
            let forward = pass % 2 == 0;
            let cols: Vec<usize> = if forward {
                (1..self.num_cols).collect()
            } else {
                (0..self.num_cols.saturating_sub(1)).rev().collect()
            };
            for c in cols {
                let col = &self.order[c];
                let mut desired: Vec<f32> = Vec::with_capacity(col.len());
                for &v in col {
                    let edges = if forward {
                        &self.up_edges[v]
                    } else {
                        &self.down_edges[v]
                    };
                    if edges.is_empty() {
                        desired.push(y[v]);
                    } else {
                        // Top-of-`v` that lines its port up with the neighbour's
                        // port. Forward: neighbour `u` is left, its exit port at
                        // y[u]+up_off, v's entry port at top+down_off. Backward:
                        // neighbour is right, entry at y[u]+down_off, v's exit at
                        // top+up_off.
                        let mut targets: Vec<f32> = edges
                            .iter()
                            .map(|&(u, up_off, down_off)| {
                                if forward {
                                    y[u] + up_off - down_off
                                } else {
                                    y[u] + down_off - up_off
                                }
                            })
                            .collect();
                        desired.push(layout_median(&mut targets));
                    }
                }
                layout_resolve_overlaps(col, &self.height, &mut desired, LAYOUT_VGAP);
                for (i, &v) in col.iter().enumerate() {
                    y[v] = desired[i];
                }
            }
        }
        y
    }
}

impl GraphCanvas {
    /// Re-run the layered auto-layout over the current level and emit a single
    /// undoable `RelayoutGraph` action carrying every node's new position.
    /// Wired to Cmd+L. Writes positions optimistically so the canvas updates
    /// immediately; the command persists them to `editor_pos`. No-op on an
    /// empty level.
    pub fn request_relayout(&mut self) {
        if self.nodes.is_empty() {
            return;
        }
        self.auto_layout();
        let positions: Vec<(u32, (f32, f32))> =
            self.nodes.iter().map(|n| (n.id, n.pos_graph)).collect();
        self.pending_actions.push(GraphEditCommand::RelayoutGraph {
            scope_path: self.scope.clone(),
            positions,
        });
    }

    /// Lay the graph out as left-to-right layers (the Sugiyama framework):
    /// assign every node a column by dependency depth, order each column to
    /// minimise wire crossings, then nudge nodes vertically so connected
    /// ports line up and wires run straight. See [`LayeredLayout`].
    pub(crate) fn auto_layout(&mut self) {
        let n = self.nodes.len();
        if n == 0 {
            return;
        }
        // Map node id → index in self.nodes for adjacency walks.
        let id_to_idx: ahash::AHashMap<u32, usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, nv)| (nv.id, i))
            .collect();

        // Forward edges only. A wire terminating on a cycle-breaking node
        // (e.g. `node.feedback`) closes a per-frame feedback loop — `connect`
        // permits it and `topological_sort` ignores it, so layout must too,
        // else depth accumulates around the loop and consumers get pushed
        // thousands of pixels off-screen. Each surviving edge carries the
        // y-offset of its source output port and target input port so the
        // coordinate pass can line the two up rather than the boxes.
        struct FwdEdge {
            from: usize,
            to: usize,
            from_off: f32,
            to_off: f32,
        }
        let mut fwd: Vec<FwdEdge> = Vec::with_capacity(self.wires.len());
        for w in &self.wires {
            let (Some(&from), Some(&to)) =
                (id_to_idx.get(&w.from_node), id_to_idx.get(&w.to_node))
            else {
                continue;
            };
            if self.nodes[to].breaks_dependency_cycle {
                continue;
            }
            fwd.push(FwdEdge {
                from,
                to,
                from_off: self.nodes[from].output_port_offset(&w.from_port),
                to_off: self.nodes[to].input_port_offset(&w.to_port),
            });
        }

        // Phase 1 — layer assignment by longest path. With back-edges removed
        // the graph is a DAG, so this converges in ≤ n passes; cap at n+1 as a
        // safety net.
        let mut depth = vec![0i32; n];
        for _ in 0..=n {
            let mut changed = false;
            for e in &fwd {
                let candidate = depth[e.from] + 1;
                if candidate > depth[e.to] {
                    depth[e.to] = candidate;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let num_cols = (depth.iter().copied().max().unwrap_or(0) as usize) + 1;

        // Phase 2 — build layout vertices. Real nodes carry their column and
        // height; each edge spanning more than one column gets a chain of
        // virtual waypoints so it participates in ordering and alignment.
        let mut column: Vec<usize> = (0..n).map(|i| depth[i] as usize).collect();
        let mut height: Vec<f32> = self.nodes.iter().map(|nv| nv.height()).collect();
        let mut up_edges: Vec<Vec<(usize, f32, f32)>> = vec![Vec::new(); n];
        let mut down_edges: Vec<Vec<(usize, f32, f32)>> = vec![Vec::new(); n];
        for e in &fwd {
            let (c0, c1) = (column[e.from], column[e.to]);
            // c1 >= c0 + 1 is guaranteed by longest-path layering.
            if c1 == c0 + 1 {
                down_edges[e.from].push((e.to, e.from_off, e.to_off));
                up_edges[e.to].push((e.from, e.from_off, e.to_off));
                continue;
            }
            let mut prev = e.from;
            let mut prev_off = e.from_off;
            for c in (c0 + 1)..c1 {
                let d = column.len();
                column.push(c);
                height.push(LAYOUT_DUMMY_H);
                up_edges.push(Vec::new());
                down_edges.push(Vec::new());
                let mid = LAYOUT_DUMMY_H * 0.5;
                down_edges[prev].push((d, prev_off, mid));
                up_edges[d].push((prev, prev_off, mid));
                prev = d;
                prev_off = mid;
            }
            down_edges[prev].push((e.to, prev_off, e.to_off));
            up_edges[e.to].push((prev, prev_off, e.to_off));
        }

        // Initial column ordering: real nodes by id (deterministic, no
        // twitch on rebuild), waypoints after them — the sweep fixes both.
        let mut order: Vec<Vec<usize>> = vec![Vec::new(); num_cols];
        for (lvid, &c) in column.iter().enumerate() {
            order[c].push(lvid);
        }
        for col in &mut order {
            col.sort_by_key(|&lvid| {
                if lvid < n {
                    (0u8, self.nodes[lvid].id)
                } else {
                    (1u8, (lvid - n) as u32)
                }
            });
        }

        let mut layout = LayeredLayout {
            num_cols,
            column,
            height,
            order,
            up_edges,
            down_edges,
        };
        layout.minimise_crossings();
        let mut y = layout.assign_y();

        // Re-seat fully-disconnected real nodes against their column's connected
        // block. A node with no wires at this level has no port to align, so the
        // coordinate passes leave it wherever its column's initial stack put it —
        // and once the connected nodes have drifted to line up with a high-fan-in
        // node's tightly-packed port stack (e.g. `render_scene`'s 9+ inputs), the
        // dangling node is stranded thousands of px away, ballooning the bounding
        // box so zoom-to-fit can't frame the graph on editor open. The common
        // trigger is a synthesis generator's unwired `generator_input`. Stacking
        // each disconnected node just above its column's connected block keeps the
        // whole graph compact; a node with no alignment role loses nothing by it.
        for c in 0..num_cols {
            let is_disconnected =
                |v: usize| layout.up_edges[v].is_empty() && layout.down_edges[v].is_empty();
            let conn_top = (0..n)
                .filter(|&v| layout.column[v] == c && !is_disconnected(v))
                .map(|v| y[v])
                .fold(f32::INFINITY, f32::min);
            if !conn_top.is_finite() {
                // No connected node in this column (all-disconnected or empty) —
                // its initial stack is already compact; leave it.
                continue;
            }
            let mut disc: Vec<usize> =
                (0..n).filter(|&v| layout.column[v] == c && is_disconnected(v)).collect();
            // Keep their relative order (topmost stays topmost), then stack the
            // block upward from just above the connected nodes.
            disc.sort_by(|&a, &b| y[a].partial_cmp(&y[b]).unwrap_or(core::cmp::Ordering::Equal));
            let mut cursor = conn_top;
            for &v in disc.iter().rev() {
                cursor -= LAYOUT_VGAP + layout.height[v];
                y[v] = cursor;
            }
        }

        // Shift so the topmost real node sits at the layout origin, then
        // write back. Waypoints are dropped — only real nodes have a position.
        let min_y = y.iter().take(n).copied().fold(f32::INFINITY, f32::min);
        let y_shift = if min_y.is_finite() {
            LAYOUT_ORIGIN.1 - min_y
        } else {
            0.0
        };
        for (i, node) in self.nodes.iter_mut().enumerate() {
            let x = LAYOUT_ORIGIN.0 + layout.column[i] as f32 * COL_SPACING;
            node.pos_graph = (x, y[i] + y_shift);
        }
    }
}
