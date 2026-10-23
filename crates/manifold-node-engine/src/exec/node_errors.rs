//! Which nodes are failing. A node that keeps failing reaches the log and the
//! stage once, whatever its message says and however it flickers: the news
//! is keyed on the node and hook, never on the text, and a node is forgotten
//! only after it has run clean for [`FORGET_AFTER`].

use std::time::{Duration, Instant};

use manifold_core::NodeId;

use super::effect_node::NodeInstanceId;

/// How long a failing node must run clean before failing again is news.
pub const FORGET_AFTER: Duration = Duration::from_secs(1);

/// A node that started failing: what the stage names. The text goes to the
/// log only.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeErrorNews {
    pub node: NodeId,
    /// The render object the error is about, when it names one.
    pub object: Option<u32>,
}

#[derive(Default)]
pub struct NodeErrors {
    /// Each failing node and hook (`true` for `late_capture`) and when it
    /// last failed, so one hook running clean never forgets the other.
    failing: Vec<((NodeInstanceId, bool), Instant)>,
    news: Vec<NodeErrorNews>,
}

impl NodeErrors {
    /// Take the errors one hook of `node` reported this frame, empty when it
    /// ran clean. Returns the first, the cause of an abort, when the node
    /// just started failing; `now` is read only while a node is failing.
    pub(crate) fn record(
        &mut self,
        node: NodeInstanceId,
        late: bool,
        news: impl FnOnce() -> NodeErrorNews,
        errors: &mut Vec<String>,
        now: impl FnOnce() -> Instant,
    ) -> Option<String> {
        if errors.is_empty() && self.failing.is_empty() {
            return None;
        }
        let key = (node, late);
        let known = self.failing.iter().position(|(failing, _)| *failing == key);
        if errors.is_empty() {
            if let Some(index) = known
                && now().saturating_duration_since(self.failing[index].1) >= FORGET_AFTER
            {
                self.failing.swap_remove(index);
            }
            return None;
        }
        let message = errors.swap_remove(0);
        errors.clear();
        let now = now();
        match known {
            Some(index) => {
                self.failing[index].1 = now;
                None
            }
            None => {
                self.failing.push((key, now));
                self.news.push(news());
                Some(message)
            }
        }
    }

    /// Hand `each` the nodes that started failing since the last call.
    pub fn take_news(&mut self, each: impl FnMut(NodeErrorNews)) {
        self.news.drain(..).for_each(each);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn errors(messages: &[&str]) -> Vec<String> {
        messages.iter().map(|m| (*m).to_owned()).collect()
    }

    fn news(node: &str) -> impl FnOnce() -> NodeErrorNews + '_ {
        move || NodeErrorNews { node: NodeId::new(node), object: None }
    }

    #[test]
    fn a_failing_node_is_news_once_whatever_its_text_and_flicker() {
        let start = Instant::now();
        let at = |ms: u64| move || start + Duration::from_millis(ms);
        let mut log = NodeErrors::default();
        let node = NodeInstanceId(3);
        assert_eq!(log.record(node, false, news("a"), &mut errors(&["no surface"]), at(0)).as_deref(), Some("no surface"));
        // A live count in the text, and clean frames in between, are no news.
        for frame in 1..30u64 {
            let mut this = if frame % 2 == 0 { errors(&[&format!("short buffer ({frame})")]) } else { Vec::new() };
            assert_eq!(log.record(node, false, news("a"), &mut this, at(frame * 16)), None);
        }
        let mut taken = Vec::new();
        log.take_news(|news| taken.push(news));
        assert_eq!(taken, [NodeErrorNews { node: NodeId::new("a"), object: None }]);
        // A second of clean frames forgets it; failing again is news.
        assert_eq!(log.record(node, false, news("a"), &mut Vec::new(), at(1500)), None);
        assert_eq!(log.record(node, false, news("a"), &mut errors(&["again"]), at(1516)).as_deref(), Some("again"));
        // The other hook of the same node is its own news.
        assert_eq!(log.record(node, true, news("a"), &mut errors(&["late"]), at(1516)).as_deref(), Some("late"));
        // An abort reports its cause first; the cause is what is logged.
        assert_eq!(
            log.record(NodeInstanceId(5), false, news("b"), &mut errors(&["cause", "follow-on"]), at(1516)).as_deref(),
            Some("cause")
        );
        let mut nodes = Vec::new();
        log.take_news(|news| nodes.push(news.node));
        assert_eq!(nodes, [NodeId::new("a"), NodeId::new("a"), NodeId::new("b")]);
    }
}
