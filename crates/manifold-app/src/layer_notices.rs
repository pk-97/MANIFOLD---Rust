//! What the stage shows about a layer whose graph failed or whose GPU frames
//! ran away (SCENE_REFINEMENT_DESIGN.md section 3 (Phases and lanes), P1b):
//! one plain toast per failing node, naming the layer and then the item, and
//! Draw Again on the World card of a stopped layer. The technical text goes
//! to the log only. `ContentState` carries the whole list, so a newer state
//! replaces an older one and the UI loses nothing it has not shown.

use std::sync::Arc;

use manifold_compositor::generator_renderer::{GeneratorRenderer, LayerNews};
use manifold_core::LayerId;
use manifold_core::layer::Layer;
use manifold_core::scene_doc::{ItemId, SceneDoc};
use manifold_node_engine::exec::node_errors::NodeErrorNews;

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
    /// This list with `news` folded in, oldest first; `toast` words each.
    pub fn with_news(&self, news: impl IntoIterator<Item = LayerNews>, toast: impl Fn(&LayerNews) -> String) -> Self {
        let mut next = self.clone();
        for news in news {
            next.version += 1;
            let layer_id = match &news {
                LayerNews::NodeError(layer_id, _) => layer_id,
                LayerNews::Stopped(layer_id) => {
                    if !next.stopped.contains(layer_id) {
                        next.stopped.push(layer_id.clone());
                    }
                    layer_id
                }
                LayerNews::Restored(layer_id) => {
                    next.stopped.retain(|stopped| stopped != layer_id);
                    continue;
                }
            };
            let notice = LayerNotice { sequence: next.version, layer_id: layer_id.clone(), message: Arc::from(toast(&news)) };
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

    #[allow(dead_code)]
    pub fn is_stopped(&self, layer_id: &LayerId) -> bool {
        self.stopped.contains(layer_id)
    }
}

/// The toast for `news`: one short sentence, the layer first, then the item
/// the performer sees.
fn toast(layer: &str, item: Option<&str>, news: &LayerNews) -> String {
    match (news, item) {
        (LayerNews::Stopped(_), _) => {
            format!("{layer} paused after heavy GPU frames; adjust the scene to resume drawing.")
        }
        (_, Some(item)) => format!("{layer}: {item} can't be drawn."),
        _ => format!("{layer} can't be drawn."),
    }
}

/// The scene item a failing node belongs to: the item drawn in the render
/// object it names, else the item its node id is scoped to (`<item id>:<role>`).
fn failing_item<'a>(doc: &'a SceneDoc, news: &NodeErrorNews) -> Option<&'a str> {
    let id = match news.object {
        Some(object) => {
            manifold_nodes_scene::node_graph::scene_compile::drawn_items(doc).into_iter().nth(object as usize)?
        }
        None => ItemId::new(news.node.as_str().rsplit_once(':')?.0),
    };
    doc.find(&id).map(|item| item.name.as_str()).filter(|name| !name.is_empty())
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
        if self.news.is_empty() {
            return;
        }
        let words = |news: &LayerNews| {
            let (LayerNews::NodeError(id, _) | LayerNews::Stopped(id) | LayerNews::Restored(id)) = news;
            let layer = layers.iter().find(|layer| &layer.layer_id == id);
            let name = layer.map(|layer| layer.name.as_str()).filter(|name| !name.is_empty()).unwrap_or("Layer");
            let item = match news {
                LayerNews::NodeError(_, failing) => layer
                    .and_then(|layer| layer.gen_params()?.scene.as_ref())
                    .and_then(|doc| failing_item(doc, failing)),
                _ => None,
            };
            toast(name, item, news)
        };
        self.notices = Arc::new(self.notices.with_news(self.news.drain(..), words));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::NodeId;

    fn layer(id: &str) -> LayerId {
        LayerId::new(id)
    }

    fn failing(node: &str) -> NodeErrorNews {
        NodeErrorNews { node: NodeId::new(node), object: None }
    }

    fn words(news: &LayerNews) -> String {
        let item = matches!(news, LayerNews::NodeError(_, failing) if failing.node.as_str() == "rock:object").then_some("Rock");
        toast("Cliff", item, news)
    }

    #[test]
    fn notices_fold_newest_per_layer_and_stops_come_and_go() {
        let a = layer("a");
        let b = layer("b");
        let notices = LayerNotices::default().with_news([
            LayerNews::NodeError(a.clone(), failing("scene:render")),
            LayerNews::NodeError(b.clone(), failing("scene:render")),
            LayerNews::NodeError(a.clone(), failing("rock:object")),
        ], words);
        assert_eq!(notices.notices.len(), 2);
        let shown: Vec<&str> = notices.after(0).map(|notice| &*notice.message).collect();
        assert_eq!(shown, ["Cliff: Rock can't be drawn.", "Cliff can't be drawn."], "each layer keeps its newest notice");
        assert_eq!(notices.after(notices.version).count(), 0);

        let stopped = notices.with_news([LayerNews::Stopped(a.clone())], words);
        assert!(stopped.is_stopped(&a) && !stopped.is_stopped(&b));
        let fresh: Vec<&LayerNotice> = stopped.after(notices.version).collect();
        assert_eq!(fresh.len(), 1);
        assert_eq!(
            &*fresh[0].message,
            "Cliff paused after heavy GPU frames; adjust the scene to resume drawing."
        );
        let restored = stopped.with_news([LayerNews::Restored(a.clone())], words);
        assert!(!restored.is_stopped(&a));
        assert!(restored.version > stopped.version);
        assert_eq!(restored.after(stopped.version).count(), 0, "drawing again is no toast");
    }
}
