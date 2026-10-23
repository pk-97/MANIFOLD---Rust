//! What the stage shows about a layer whose graph failed or whose GPU frames
//! ran away (SCENE_REFINEMENT_DESIGN.md section 3 (Phases and lanes), P1b):
//! each node error as one toast naming the layer, and Draw Again on the World
//! card of a stopped layer. `ContentState` carries the whole list, so a newer
//! state replaces an older one and the UI loses nothing it has not shown.

use std::sync::Arc;

use manifold_compositor::generator_renderer::{GeneratorRenderer, LayerNews};
use manifold_core::LayerId;
use manifold_core::layer::Layer;
use manifold_gpu::{RUNAWAY_LAYER_FRAME_SECONDS, RUNAWAY_LAYER_FRAMES};

/// One layer's newest notice. The UI toasts each sequence once.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerNotice {
    pub sequence: u64,
    pub layer_id: LayerId,
    pub message: Arc<str>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayerNotices {
    /// The newest notice per layer.
    pub notices: Vec<LayerNotice>,
    /// Layers stopped by runaway GPU frames: their World card offers Draw Again.
    pub stopped: Vec<LayerId>,
    /// Moves with every change; the frame planner keys on it.
    pub version: u64,
}

impl LayerNotices {
    /// This list with `news` folded in, oldest first. Each message leads with
    /// its layer's name from `name_of`.
    pub fn with_news<'a>(
        &self,
        news: impl IntoIterator<Item = LayerNews>,
        name_of: impl Fn(&LayerId) -> Option<&'a str>,
    ) -> Self {
        let mut next = self.clone();
        for news in news {
            next.version += 1;
            let (layer_id, message) = match news {
                LayerNews::NodeError(layer_id, message) => (layer_id, message),
                LayerNews::Stopped(layer_id) => {
                    if !next.stopped.contains(&layer_id) {
                        next.stopped.push(layer_id.clone());
                    }
                    let message = format!(
                        "Stopped drawing: its GPU frame ran over {RUNAWAY_LAYER_FRAME_SECONDS} s {RUNAWAY_LAYER_FRAMES} \
                         frames in a row, which freezes the Mac. Draw Again on the World card, or any edit, brings it back."
                    );
                    (layer_id, Arc::from(message))
                }
                LayerNews::Restored(layer_id) => {
                    next.stopped.retain(|stopped| stopped != &layer_id);
                    continue;
                }
            };
            let name = name_of(&layer_id).filter(|name| !name.is_empty()).unwrap_or("Layer");
            let message = Arc::from(format!("{name}: {message}"));
            let notice = LayerNotice { sequence: next.version, layer_id, message };
            match next.notices.iter_mut().find(|old| old.layer_id == notice.layer_id) {
                Some(old) => *old = notice,
                None => next.notices.push(notice),
            }
        }
        next
    }

    /// Notices the UI has not shown yet.
    pub fn after(&self, shown: u64) -> impl Iterator<Item = &LayerNotice> {
        self.notices.iter().filter(move |notice| notice.sequence > shown)
    }

    pub fn is_stopped(&self, layer_id: &LayerId) -> bool {
        self.stopped.contains(layer_id)
    }
}

/// The content thread's side: the published list, and the renderer's news
/// waiting to fold into it.
#[derive(Default)]
pub struct LayerNoticeFeed {
    notices: Arc<LayerNotices>,
    news: Vec<LayerNews>,
}

impl LayerNoticeFeed {
    pub fn notices(&self) -> &Arc<LayerNotices> {
        &self.notices
    }

    /// Fold the renderer's news from this frame. The list is replaced only
    /// when there is news, so a quiet frame allocates nothing.
    pub fn take(&mut self, renderer: &mut GeneratorRenderer, layers: &[Layer]) {
        let news = &mut self.news;
        renderer.drain_layer_news(|item| news.push(item));
        if !self.news.is_empty() {
            let name_of = |id: &LayerId| layers.iter().find(|layer| &layer.layer_id == id).map(|layer| layer.name.as_str());
            self.notices = Arc::new(self.notices.with_news(self.news.drain(..), name_of));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(id: &str) -> LayerId {
        LayerId::new(id)
    }

    fn name(layer: &LayerId) -> Option<&'static str> {
        (layer.as_str() == "a").then_some("Cliff")
    }

    #[test]
    fn notices_fold_newest_per_layer_and_stops_come_and_go() {
        let a = layer("a");
        let b = layer("b");
        let notices = LayerNotices::default().with_news([
            LayerNews::NodeError(a.clone(), Arc::from("first")),
            LayerNews::NodeError(b.clone(), Arc::from("other layer")),
            LayerNews::NodeError(a.clone(), Arc::from("second")),
        ], name);
        assert_eq!(notices.notices.len(), 2);
        let shown: Vec<&str> = notices.after(0).map(|notice| &*notice.message).collect();
        assert_eq!(shown, ["Cliff: second", "Layer: other layer"], "each layer keeps its newest notice");
        assert_eq!(notices.after(notices.version).count(), 0);

        let stopped = notices.with_news([LayerNews::Stopped(a.clone())], name);
        assert!(stopped.is_stopped(&a) && !stopped.is_stopped(&b));
        let fresh: Vec<&LayerNotice> = stopped.after(notices.version).collect();
        assert_eq!(fresh.len(), 1);
        assert!(fresh[0].message.contains("Draw Again"));
        let restored = stopped.with_news([LayerNews::Restored(a.clone())], name);
        assert!(!restored.is_stopped(&a));
        assert!(restored.version > stopped.version);
        assert_eq!(restored.after(stopped.version).count(), 0, "drawing again is no toast");
    }
}
