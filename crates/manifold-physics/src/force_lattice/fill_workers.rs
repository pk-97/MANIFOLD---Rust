//! Helper threads that lay a force field onto a big lattice. A field that
//! moves every tick (drifting gusts) is laid again each tick, which one core
//! cannot do at water resolution 128 inside a frame. The threads start once
//! and are reused every fill: a fill sends each helper one job on its own
//! bounded channel and waits for its answer, so a fill allocates nothing and
//! shares nothing but those channels. Every node is computed from its own
//! index exactly as the single-thread fill does, so the result is bit for bit
//! the same however the lattice is split.

use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::JoinHandle;

use super::{ForceLattice, NotFinite};
use crate::{FieldValue, VectorField};

/// One helper's share of the caller's values: two `[f32; 4]` per node.
struct Share {
    values: *mut [f32; 4],
    len: usize,
}

// SAFETY: a share is a part of the caller's buffer that no other thread
// touches; `FillWorkers::fill` keeps that buffer mutably borrowed until every
// helper sent a share has answered or died (`Answers` waits even on unwind).
unsafe impl Send for Share {}

/// One helper's share of a fill. Sendable only when the field is.
struct Job<F> {
    lattice: ForceLattice,
    /// The share's first node.
    first: usize,
    share: Share,
    field: F,
}

struct Helper<F> {
    jobs: Option<SyncSender<Job<F>>>,
    answers: Receiver<Result<(), NotFinite>>,
    thread: Option<JoinHandle<()>>,
}

/// Threads for big fills, owned by one [`super::TickForces`].
pub struct FillWorkers<F = FieldValue> {
    helpers: Vec<Helper<F>>,
}

impl<F: VectorField + Clone + 'static> FillWorkers<F> {
    /// `helpers` threads beside the calling one.
    pub fn start(helpers: usize) -> Self {
        let helpers = (0..helpers)
            .map(|index| {
                let (jobs, inbox) = sync_channel::<Job<F>>(1);
                let (outbox, answers) = sync_channel(1);
                let thread = std::thread::Builder::new()
                    .name(format!("force-fill-{index}"))
                    .spawn(move || {
                        for job in inbox {
                            // SAFETY: see `Share`: this share is ours alone
                            // until the answer below is received.
                            let values = unsafe {
                                std::slice::from_raw_parts_mut(job.share.values, job.share.len)
                            };
                            let answer = job.lattice.fill_forces_from(job.first, values, &|x| {
                                job.field.sample_force(x)
                            });
                            if outbox.send(answer).is_err() {
                                return;
                            }
                        }
                    })
                    .expect("spawn a force fill thread");
                Helper {
                    jobs: Some(jobs),
                    answers,
                    thread: Some(thread),
                }
            })
            .collect();
        Self { helpers }
    }

    /// Threads one fill uses, the calling thread included.
    pub fn threads(&self) -> usize {
        self.helpers.len() + 1
    }

    /// [`ForceLattice::fill_forces`] of `field`, split across the threads.
    /// A helper that panics makes the fill panic; it never hangs.
    pub fn fill(
        &mut self,
        lattice: ForceLattice,
        values: &mut [[f32; 4]],
        field: &F,
    ) -> Result<(), NotFinite> {
        let nodes = values.len() / 2;
        let per_share = nodes.div_ceil(self.threads()).max(1);
        let (own, rest) = values.split_at_mut((2 * per_share).min(values.len()));
        let mut answers = Answers {
            helpers: &self.helpers,
            sent: 0,
            received: 0,
        };
        for (index, share) in rest.chunks_mut(2 * per_share).enumerate() {
            let job = Job {
                lattice,
                first: (index + 1) * per_share,
                share: Share {
                    values: share.as_mut_ptr(),
                    len: share.len(),
                },
                field: field.clone(),
            };
            answers.helpers[index]
                .jobs
                .as_ref()
                .expect("running")
                .send(job)
                .expect("force fill thread stopped");
            answers.sent += 1;
        }
        let mine = lattice.fill_forces_from(0, own, &|x| field.sample_force(x));
        answers.wait().and(mine)
    }
}

/// The helpers a fill has sent jobs to, and how many have answered. Every
/// job is answered (or its helper gone) before the caller's buffer can be
/// used again, on return or unwind.
struct Answers<'a, F> {
    helpers: &'a [Helper<F>],
    sent: usize,
    received: usize,
}

impl<F> Answers<'_, F> {
    fn wait(&mut self) -> Result<(), NotFinite> {
        let mut result = Ok(());
        while self.received < self.sent {
            let answer = self.helpers[self.received].answers.recv();
            self.received += 1;
            result = result.and(answer.expect("force fill thread stopped"));
        }
        result
    }
}

impl<F> Drop for Answers<'_, F> {
    /// Drain only the answers still owed. A helper that died answers with a
    /// closed channel, so this never waits on it.
    fn drop(&mut self) {
        for helper in &self.helpers[self.received..self.sent] {
            let _ = helper.answers.recv();
        }
    }
}

impl<F> Drop for FillWorkers<F> {
    fn drop(&mut self) {
        for helper in &mut self.helpers {
            // Closing the channel ends the helper's loop.
            helper.jobs = None;
            if let Some(thread) = helper.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The threaded fill writes exactly the single-thread fill's bits, fill
    /// after fill on the same threads, and refuses a non-finite field the
    /// same way.
    #[test]
    fn threaded_fill_matches_the_single_thread_fill_bit_for_bit() {
        let lattice = ForceLattice::over_box([-2.0, 0.0, -2.0], [4.0, 3.0, 4.0], 41);
        let mut workers = FillWorkers::start(3);
        for travel in [0.0f32, 0.37, 12.5] {
            let gusts = FieldValue::turbulence(1.3, [travel, 0.0, -travel], 5)
                .unwrap()
                .scaled(2.0)
                .unwrap();
            let field = FieldValue::uniform([4.0, 0.0, 1.0])
                .unwrap()
                .sum(&gusts)
                .unwrap()
                .air(0.8)
                .unwrap()
                .sum(&FieldValue::vortex([0.0, 1.0, 0.0], [0.0, 1.0, 0.0], 2.0, 1.0).unwrap())
                .unwrap();
            let mut single = vec![[0.0f32; 4]; 2 * lattice.node_count()];
            let mut threaded = vec![[1.0f32; 4]; 2 * lattice.node_count()];
            lattice
                .fill_forces(&mut single, |x| field.sample_force(x))
                .unwrap();
            workers.fill(lattice, &mut threaded, &field).unwrap();
            let bits = |values: &[[f32; 4]]| {
                values
                    .iter()
                    .flatten()
                    .map(|v| v.to_bits())
                    .collect::<Vec<_>>()
            };
            assert_eq!(bits(&threaded), bits(&single), "travel {travel}");
        }
        let overflow = FieldValue::uniform([f32::MAX, 0.0, 0.0])
            .unwrap()
            .scaled(f32::MAX)
            .unwrap();
        let mut values = vec![[0.0f32; 4]; 2 * lattice.node_count()];
        assert!(
            lattice
                .fill_forces(&mut values, |x| overflow.sample_force(x))
                .is_err()
        );
        assert!(workers.fill(lattice, &mut values, &overflow).is_err());
    }

    /// A field that panics on one named thread.
    #[derive(Clone)]
    struct PanicsOn(&'static str);

    impl VectorField for PanicsOn {
        fn sample(&self, _: [f32; 3]) -> [f32; 3] {
            assert_ne!(
                std::thread::current().name(),
                Some(self.0),
                "field panics on {}",
                self.0
            );
            [0.0, 1.0, 0.0]
        }
    }

    /// A panic in one helper, or in the caller's own share, makes the fill
    /// panic and lets the workers shut down; nothing waits forever.
    #[test]
    fn a_panicking_share_panics_the_fill_and_never_hangs() {
        for victim in ["force-fill-1", "fill-caller"] {
            let (done, finished) = std::sync::mpsc::channel();
            std::thread::Builder::new()
                .name("fill-caller".into())
                .spawn(move || {
                    let lattice = ForceLattice::over_box([0.0; 3], [1.0; 3], 16);
                    let mut workers = FillWorkers::start(3);
                    let mut values = vec![[0.0f32; 4]; 2 * lattice.node_count()];
                    let fill = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        workers.fill(lattice, &mut values, &PanicsOn(victim))
                    }));
                    drop(workers);
                    done.send(fill.is_err()).unwrap();
                })
                .unwrap();
            let panicked = finished
                .recv_timeout(std::time::Duration::from_secs(30))
                .unwrap_or_else(|_| panic!("the fill hung when {victim} panicked"));
            assert!(panicked, "the fill panics when {victim} does");
        }
    }
}
