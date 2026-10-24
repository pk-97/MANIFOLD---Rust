//! Clicking a picture picks the item drawn there (SCENE_DOCUMENT_DESIGN.md
//! section 7.14). The UI sends where the click landed; the content thread
//! has the scene draw its item ids there on the next frame
//! (`render_scene/item_pick.rs`), reads the render slot back a frame later,
//! and turns it into an `ItemId` through the compiler's slot table
//! (`scene_compile::drawn_items`). The answer reaches the Scene panel's one
//! selection store as a selection receipt (`edit_selection.rs`), so tree,
//! preview and viewport stay one selection, with gizmos shown or hidden.

use manifold_compositor::generator_renderer::GeneratorRenderer;
use manifold_core::LayerId;
use manifold_core::project::Project;
use manifold_core::scene_doc::ItemId;
use manifold_node_engine::scene::pick::PickRequest;

/// Which picture of a scene layer was clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickSurface {
    /// The layer's output render, on the main preview.
    Output,
    /// The layer's scene viewport: the graph editor's, or the preview while
    /// it navigates.
    Viewport,
}

/// One click on a scene picture.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ScenePick {
    pub layer: LayerId,
    pub surface: PickSurface,
    /// Where the click landed, as a fraction of the picture from its top left.
    pub uv: [f32; 2],
    /// Shift: add the picked item to the selection, or take it out.
    pub extend: bool,
}

/// Where `point` (render pixels of `view`) sits in the picture, as a
/// fraction from its top left.
pub(crate) fn uv_of(view: &crate::scene_gizmo::GizmoView, point: (f32, f32)) -> [f32; 2] {
    [
        point.0 / view.width.max(1) as f32,
        point.1 / view.height.max(1) as f32,
    ]
}

/// What a pick found: the item drawn at the point, or `None` for empty space.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PickedItem {
    pub pick: ScenePick,
    pub item: Option<ItemId>,
}

/// A pick waits this many frames for its answer before it is given up (the
/// layer stopped rendering, or its graph was rebuilt mid-pick).
const GIVE_UP_FRAMES: u32 = 30;

/// The content thread's picks: sent to the scene, waiting for an answer, and
/// answered.
#[derive(Default)]
pub(crate) struct ScenePicks {
    next_id: u64,
    unsent: Vec<ScenePick>,
    /// Sent picks: the pick, its request id and the frames it has waited.
    pending: Vec<(ScenePick, u64, u32)>,
    answered: Vec<(ScenePick, Option<u32>)>,
}

impl ScenePicks {
    pub fn push(&mut self, pick: ScenePick) {
        self.unsent.push(pick);
    }

    /// Before the generators render: ask each new pick's scene for it.
    pub fn send(&mut self, generators: &mut GeneratorRenderer) {
        for pick in self.unsent.drain(..) {
            // A picture answers one pick at a time: the newer click wins.
            self.pending
                .retain(|(older, ..)| older.layer != pick.layer || older.surface != pick.surface);
            self.next_id += 1;
            let request = PickRequest {
                id: self.next_id,
                uv: pick.uv,
            };
            match pick.surface {
                PickSurface::Output => generators.request_pick(&pick.layer, request),
                PickSurface::Viewport => {
                    generators.request_scene_viewport_pick(&pick.layer, request)
                }
            }
            self.pending.push((pick, self.next_id, 0));
        }
    }

    /// After the generators render: collect the answers read back this frame
    /// and give up on picks that waited too long.
    pub fn collect(&mut self, generators: &mut GeneratorRenderer) {
        if self.pending.is_empty() {
            return;
        }
        let answered = &mut self.answered;
        self.pending.retain_mut(|(pick, id, waited)| {
            let hit = match pick.surface {
                PickSurface::Output => generators.take_pick(&pick.layer),
                PickSurface::Viewport => generators.take_scene_viewport_pick(&pick.layer),
            };
            match hit {
                Some(hit) if hit.id == *id => {
                    answered.push((pick.clone(), hit.slot));
                    false
                }
                _ => {
                    *waited += 1;
                    *waited < GIVE_UP_FRAMES
                }
            }
        });
    }

    /// The answered picks, with each render slot turned into the item the
    /// compiler drew in it.
    pub fn take_answered(&mut self, project: Option<&Project>) -> Vec<PickedItem> {
        if self.answered.is_empty() {
            return Vec::new();
        }
        self.answered
            .drain(..)
            .map(|(pick, slot)| {
                let item = slot
                    .zip(project.and_then(|project| project.scene_of(&pick.layer)))
                    .and_then(|(slot, doc)| {
                        manifold_nodes_scene::node_graph::scene_compile::drawn_items(doc)
                            .into_iter()
                            .nth(slot as usize)
                    });
                PickedItem { pick, item }
            })
            .collect()
    }
}
