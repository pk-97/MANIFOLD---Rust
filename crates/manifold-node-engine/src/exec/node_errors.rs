//! The newest error each node reported. A node that keeps failing reaches the
//! log and the stage once, not every frame; a clean evaluate forgets it, so
//! the same error coming back later is news again.

use super::effect_node::NodeInstanceId;

#[derive(Default)]
pub struct NodeErrors {
    /// Keyed by node and hook (`true` for `late_capture`), so one hook running
    /// clean never forgets the other's error.
    newest: Vec<((NodeInstanceId, bool), String)>,
    /// Bumped by every error that is news.
    sequence: u64,
    /// The message of the latest news.
    latest: String,
}

impl NodeErrors {
    /// Take the errors one hook of `node` reported this frame, empty when it
    /// ran clean. The first is kept: an abort reports its cause first and
    /// what followed from it after. Returns it when it is news.
    pub(crate) fn record(&mut self, node: NodeInstanceId, late: bool, errors: &mut Vec<String>) -> Option<&str> {
        let node = (node, late);
        if errors.is_empty() && self.newest.is_empty() {
            return None;
        }
        let known = self.newest.iter().position(|(n, _)| *n == node);
        if errors.is_empty() {
            if let Some(index) = known {
                self.newest.swap_remove(index);
            }
            return None;
        }
        let message = errors.swap_remove(0);
        errors.clear();
        let index = match known {
            Some(index) if self.newest[index].1 == message => return None,
            Some(index) => {
                self.newest[index].1 = message;
                index
            }
            None => {
                self.newest.push((node, message));
                self.newest.len() - 1
            }
        };
        self.sequence += 1;
        self.latest.clone_from(&self.newest[index].1);
        Some(&self.latest)
    }

    /// Moves whenever an error is news; `latest` is that error.
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn latest(&self) -> &str {
        &self.latest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn errors(messages: &[&str]) -> Vec<String> {
        messages.iter().map(|m| (*m).to_owned()).collect()
    }

    #[test]
    fn a_repeating_error_is_news_once_and_again_after_a_clean_frame() {
        let mut log = NodeErrors::default();
        let node = NodeInstanceId(3);
        assert_eq!(log.record(node, false, &mut errors(&["no surface"])), Some("no surface"));
        for _ in 0..10 {
            assert_eq!(log.record(node, false, &mut errors(&["no surface"])), None);
        }
        assert_eq!(log.sequence(), 1);
        assert_eq!(log.record(node, false, &mut errors(&["other"])), Some("other"));
        assert_eq!(log.record(node, false, &mut Vec::new()), None);
        assert_eq!(log.record(node, false, &mut errors(&["other"])), Some("other"));
        assert_eq!(log.sequence(), 3);
        // Another node's error is its own news.
        assert_eq!(log.record(NodeInstanceId(4), false, &mut errors(&["other"])), Some("other"));
        assert_eq!(log.latest(), "other");
        // An abort reports its cause first; the cause is what shows.
        assert_eq!(log.record(NodeInstanceId(5), false, &mut errors(&["cause", "follow-on"])), Some("cause"));
    }
}
