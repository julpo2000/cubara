//! The order streamed geometry reaches the arena in.
//!
//! The caller hands over node updates in batches ([`Renderer::apply_node_updates`]
//! (crate::Renderer::apply_node_updates)): meshes to upload and nodes to drop.
//! Uploads are paced -- a whole ring's worth at once would spike the frame --
//! so they wait in a queue. **Drops wait in the same queue, behind the uploads
//! handed over with them.**
//!
//! They used to happen at once. The caller drops a node in the same batch as
//! the meshes that replace it, precisely so the two swap on one frame (#262) --
//! but with the drop immediate and the replacements queued, a batch of a few
//! hundred left the ground they covered undrawn for as many frames as the
//! queue took to reach them. Measured by `--bench flight` as the ground under
//! the camera vanishing for a moment every time it crossed a chunk layer.
//!
//! Now a drop can at worst come a frame or two *late*, while the last of its
//! replacements upload: the old node and some of the new drawn together, which
//! is two nearly identical surfaces rather than none.
//!
//! How much goes up each frame is [`upload_budget`].

use std::collections::{HashSet, VecDeque};

use crate::arena::NodeId;

/// One thing for the arena to do.
#[derive(Debug, PartialEq)]
pub(crate) enum ArenaOp<T> {
    /// Upload (or replace) this node's geometry.
    Insert(NodeId, T),
    /// Stop drawing this node.
    Remove(NodeId),
}

/// Node updates waiting for their turn.
pub(crate) struct Uploads<T> {
    /// The nodes meant to be in the arena once everything queued is done:
    /// what decides whether a queued step still applies when its turn comes.
    desired: HashSet<NodeId>,
    queue: VecDeque<ArenaOp<T>>,
}

impl<T> Uploads<T> {
    pub(crate) fn new() -> Self {
        Self {
            desired: HashSet::new(),
            queue: VecDeque::new(),
        }
    }

    /// Queue one batch: `meshed` first, then the drops that must not happen
    /// before them.
    pub(crate) fn push(
        &mut self,
        to_unload: impl IntoIterator<Item = NodeId>,
        meshed: impl IntoIterator<Item = (NodeId, T)>,
    ) {
        for (id, item) in meshed {
            self.desired.insert(id);
            self.queue.push_back(ArenaOp::Insert(id, item));
        }
        for id in to_unload {
            self.desired.remove(&id);
            self.queue.push_back(ArenaOp::Remove(id));
        }
    }

    /// The next step still wanted, in order. A step a later batch overruled
    /// is skipped: an upload for a node dropped since, a drop for a node
    /// handed over again since (its new geometry replaces the old in place).
    pub(crate) fn pop(&mut self) -> Option<ArenaOp<T>> {
        while let Some(op) = self.queue.pop_front() {
            let current = match &op {
                ArenaOp::Insert(id, _) => self.desired.contains(id),
                ArenaOp::Remove(id) => !self.desired.contains(id),
            };
            if current {
                return Some(op);
            }
        }
        None
    }

    /// Steps still waiting.
    pub(crate) fn len(&self) -> usize {
        self.queue.len()
    }
}

/// How long uploads may take in a frame that followed one of `last_frame`:
/// [`UPLOAD_SHARE`] of it, and never more than of a 30 FPS frame -- a frame
/// that hitched must not make the next one hitch too.
///
/// **A share of the frame, not a number of nodes.** It was 32 nodes a frame,
/// chosen at a thousand frames a second -- 32,000 a second. At a 60 Hz
/// display's pace the same 32 was under 2,000 a second, and dropping one
/// chunk layer while flying hands over about 2,400 at once: over a second of
/// ground missing its detail (`--bench flight`). Uploading is cheap -- about
/// 1,000 nodes in 3.6 ms on an M3 -- so a quarter of a 60 Hz frame clears such
/// a batch in a handful of frames, and a quarter of a 1 ms frame still spikes
/// nothing.
pub(crate) fn upload_budget(last_frame: std::time::Duration) -> std::time::Duration {
    last_frame
        .min(std::time::Duration::from_micros(33_333))
        .mul_f64(UPLOAD_SHARE)
}

/// The share of a frame uploads may take.
const UPLOAD_SHARE: f64 = 0.25;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn id(n: i32) -> NodeId {
        NodeId {
            level: 0,
            pos: [n, 0, 0],
        }
    }

    fn drain<T>(q: &mut Uploads<T>) -> Vec<ArenaOp<T>> {
        std::iter::from_fn(|| q.pop()).collect()
    }

    #[test]
    fn a_node_is_dropped_only_after_what_replaces_it_is_uploaded() {
        let mut q = Uploads::new();
        // A parent (9) replaced by its children (1..=3).
        q.push([id(9)], (1..=3).map(|n| (id(n), n)));
        assert_eq!(
            drain(&mut q),
            vec![
                ArenaOp::Insert(id(1), 1),
                ArenaOp::Insert(id(2), 2),
                ArenaOp::Insert(id(3), 3),
                ArenaOp::Remove(id(9)),
            ],
            "the parent left before its children arrived"
        );
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn a_step_a_later_batch_overruled_is_skipped() {
        let mut q = Uploads::new();
        q.push([], [(id(1), 1)]);
        q.push([id(1)], []);
        assert_eq!(
            drain(&mut q),
            vec![ArenaOp::Remove(id(1))],
            "uploaded a node already dropped"
        );
        q.push([id(2)], []);
        q.push([], [(id(2), 2)]);
        assert_eq!(
            drain(&mut q),
            vec![ArenaOp::Insert(id(2), 2)],
            "dropped a node handed over again -- a gap before its new geometry"
        );
    }

    #[test]
    fn uploads_get_a_quarter_of_the_frame_at_any_frame_rate() {
        assert_eq!(
            upload_budget(Duration::from_micros(16_667)),
            Duration::from_nanos(4_166_750)
        );
        assert_eq!(
            upload_budget(Duration::from_millis(1)),
            Duration::from_micros(250)
        );
        assert_eq!(
            upload_budget(Duration::from_millis(500)),
            Duration::from_micros(33_333).mul_f64(0.25),
            "a hitch handed on to the next frame"
        );
    }
}
