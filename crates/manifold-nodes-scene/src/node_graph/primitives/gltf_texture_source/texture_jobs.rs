//! Bounded CPU-only texture jobs. Paths are resolved inside workers before
//! content-key coalescing, so no cache hit can hide a missing/edited dependency.
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock, Weak, mpsc};

use ahash::AHashMap;
use sha2::{Digest, Sha256};

use crate::node_graph::gltf_load::{
    decode_texture_snapshot, dummy_image_data, parse_texture_snapshot,
};

const WORKER_COUNT: usize = 2;
const QUEUE_CAPACITY: usize = 64;

#[cfg(test)]
static WORKERS_STARTED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub struct DecodedTexture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub rgba8_sha256: [u8; 32],
    pub warning: Option<String>,
}

type LoadResult = Result<Arc<DecodedTexture>, String>;
pub type LoadReceiver = mpsc::Receiver<LoadResult>;

struct Job {
    path: PathBuf,
    texture_index: u32,
    reply: mpsc::Sender<LoadResult>,
}

enum DecodeEntry {
    Pending(Vec<mpsc::Sender<LoadResult>>),
    Ready(Weak<DecodedTexture>),
}

struct TextureJobs {
    sender: mpsc::SyncSender<Job>,
    receiver: Mutex<mpsc::Receiver<Job>>,
    decodes: Mutex<AHashMap<[u8; 32], DecodeEntry>>,
    started: Once,
    available_slots: AtomicUsize,
}

impl TextureJobs {
    fn new() -> Self {
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        Self {
            sender,
            receiver: Mutex::new(receiver),
            decodes: Mutex::new(AHashMap::new()),
            started: Once::new(),
            available_slots: AtomicUsize::new(QUEUE_CAPACITY),
        }
    }

    fn start(&'static self) {
        self.started.call_once(|| {
            for index in 0..WORKER_COUNT {
                std::thread::Builder::new()
                    .name(format!("gltf-texture-{index}"))
                    .spawn(move || self.work())
                    .expect("start glTF texture decode worker");
            }
        });
    }

    fn submit(&self, path: &str, texture_index: u32) -> Option<LoadReceiver> {
        self.available_slots
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |slots| {
                slots.checked_sub(1)
            })
            .ok()?;
        let (reply, receiver) = mpsc::channel();
        match self.sender.try_send(Job {
            path: PathBuf::from(path),
            texture_index,
            reply,
        }) {
            Ok(()) => Some(receiver),
            Err(mpsc::TrySendError::Full(_)) => {
                self.available_slots.fetch_add(1, Ordering::Release);
                None
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                unreachable!("texture workers retain their receiver for process lifetime")
            }
        }
    }

    fn receive(&self) -> Result<Job, mpsc::RecvError> {
        // Release queue capacity as soon as a worker takes the job; file I/O
        // and decoding never hold the receiver mutex or a queue reservation.
        let job = self
            .receiver
            .lock()
            .expect("texture job receiver poisoned")
            .recv()?;
        self.available_slots.fetch_add(1, Ordering::Release);
        Ok(job)
    }

    fn publish(&self, identity: [u8; 32], result: LoadResult) {
        let mut decodes = self.decodes.lock().expect("texture decode table poisoned");
        let Some(DecodeEntry::Pending(waiters)) = decodes.remove(&identity) else {
            unreachable!("decode job owns its pending entry");
        };
        if let Ok(decoded) = &result
            && decoded.warning.is_none()
        {
            decodes.insert(identity, DecodeEntry::Ready(Arc::downgrade(decoded)));
        }
        // Every exit, including a third-party decoder panic, releases the
        // pending entry and answers all coalesced receivers. Failures are uncached.
        for waiter in waiters {
            let _ = waiter.send(result.clone());
        }
    }

    fn work(&self) {
        #[cfg(test)]
        WORKERS_STARTED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        loop {
            let Ok(job) = self.receive() else { return };
            let snapshot = match catch_worker_panic("source resolution", || {
                parse_texture_snapshot(&job.path, job.texture_index)
            }) {
                Ok(Ok(snapshot)) => snapshot,
                Ok(Err(error)) | Err(error) => {
                    let _ = job.reply.send(Err(error));
                    continue;
                }
            };
            let identity = snapshot.identity;
            {
                let mut decodes = self.decodes.lock().expect("texture decode table poisoned");
                decodes.retain(|_, entry| match entry {
                    DecodeEntry::Pending(_) => true,
                    DecodeEntry::Ready(weak) => weak.strong_count() > 0,
                });
                match decodes.get_mut(&identity) {
                    Some(DecodeEntry::Pending(waiters)) => {
                        waiters.push(job.reply);
                        continue;
                    }
                    Some(DecodeEntry::Ready(weak)) => {
                        if let Some(decoded) = weak.upgrade() {
                            let _ = job.reply.send(Ok(decoded));
                            continue;
                        }
                    }
                    None => {}
                }
                decodes.insert(identity, DecodeEntry::Pending(vec![job.reply]));
            }
            let result = catch_worker_panic("image decode", || {
                let (width, height, rgba, warning) = match decode_texture_snapshot(snapshot) {
                    Ok((width, height, rgba)) => (width, height, rgba, None),
                    Err(error) => {
                        let dummy = dummy_image_data();
                        (
                            dummy.width,
                            dummy.height,
                            dummy.pixels,
                            Some(format!(
                                "{} texture {}: {error} — dummy texture substituted",
                                job.path.display(),
                                job.texture_index
                            )),
                        )
                    }
                };
                let rgba8_sha256 = Sha256::digest(&rgba).into();
                Arc::new(DecodedTexture {
                    width,
                    height,
                    rgba,
                    rgba8_sha256,
                    warning,
                })
            });
            self.publish(identity, result);
        }
    }
}

// Development defense for unforeseen third-party panics: catch only fallible
// source work with no pool locks held, so one asset cannot kill a reusable
// worker or strand coalesced requests. Known malformed URI inputs are validated
// before the pinned importer, since release builds use panic=abort.
fn catch_worker_panic<T>(stage: &str, work: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).map_err(|panic| {
        let message = panic
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("non-string panic");
        format!("glTF texture {stage} panicked: {message}")
    })
}

pub(super) fn request(path: &str, texture_index: u32) -> Option<LoadReceiver> {
    static JOBS: OnceLock<TextureJobs> = OnceLock::new();
    let jobs = JOBS.get_or_init(TextureJobs::new);
    jobs.start();
    jobs.submit(path, texture_index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texture_jobs_use_bounded_workers_coalesce_and_invalidate_dependencies() {
        use crate::node_graph::gltf_load::texture_tests::TextureFixture;
        let fixture = TextureFixture::external_png();
        let path = fixture.path.to_str().unwrap();
        let receives: Vec<_> = (0..16).map(|_| request(path, 0).unwrap()).collect();
        let images: Vec<_> = receives
            .into_iter()
            .map(|rx| {
                rx.recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap()
                    .unwrap()
            })
            .collect();
        assert!(images.iter().all(|image| Arc::ptr_eq(&images[0], image)));
        let workers = WORKERS_STARTED.load(std::sync::atomic::Ordering::SeqCst);
        assert!((1..=WORKER_COUNT).contains(&workers));
        fixture.write_png(201);
        let changed = request(path, 0)
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert!(!Arc::ptr_eq(&images[0], &changed));
        assert_eq!(changed.rgba[0], 201);
        std::fs::remove_file(fixture.image_path()).unwrap();
        assert!(
            request(path, 0)
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap()
                .is_err()
        );
        std::fs::write(fixture.image_path(), b"invalid PNG").unwrap();
        let corrupt = request(path, 0)
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert_eq!(corrupt.rgba, [255, 0, 255, 255].repeat(4));
        assert!(
            corrupt
                .warning
                .as_ref()
                .unwrap()
                .contains("dummy texture substituted")
        );
        let corrupt_again = request(path, 0)
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert!(
            !Arc::ptr_eq(&corrupt, &corrupt_again),
            "failed decode must not be cached"
        );
        fixture.write_png(99);
        let repaired = request(path, 0)
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert_eq!(repaired.rgba[0], 99);
    }

    #[test]
    fn malformed_texture_uris_do_not_kill_workers_or_block_later_jobs() {
        use crate::node_graph::gltf_load::texture_tests::TextureFixture;
        let image_fixture = TextureFixture::external_png();
        image_fixture.write_document(
            serde_json::json!([{ "source": 0 }]),
            serde_json::json!([{ "uri": "%FF.png" }]),
            &[],
        );
        let buffer_fixture = TextureFixture::external_png();
        let mut document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&buffer_fixture.path).unwrap()).unwrap();
        document["buffers"][0]["uri"] = "%FF.bin".into();
        std::fs::write(&buffer_fixture.path, serde_json::to_vec(&document).unwrap()).unwrap();
        // More malformed jobs than workers catches permanent worker loss.
        let mut failures = Vec::new();
        for _ in 0..WORKER_COUNT + 1 {
            for fixture in [&image_fixture, &buffer_fixture] {
                failures.push(request(fixture.path.to_str().unwrap(), 0).unwrap());
            }
        }
        for receiver in failures {
            let result = receiver
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            let error = result.err().expect("malformed URI must report a failure");
            assert!(error.contains("invalid resource URI"), "{error}");
            assert!(
                !error.contains("panicked"),
                "URI validation must return an ordinary error"
            );
        }
        let valid = TextureFixture::external_png();
        let decoded = request(valid.path.to_str().unwrap(), 0)
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert_eq!(decoded.rgba[0], 17);
    }

    #[test]
    fn panicked_decode_replies_to_all_waiters_and_removes_pending_entry() {
        let jobs = TextureJobs::new();
        let identity = [7; 32];
        let (sender_a, receiver_a) = mpsc::channel();
        let (sender_b, receiver_b) = mpsc::channel();
        jobs.decodes
            .lock()
            .unwrap()
            .insert(identity, DecodeEntry::Pending(vec![sender_a, sender_b]));
        let result = catch_worker_panic("image decode", || panic!("malformed decoder input"));
        jobs.publish(identity, result);
        for receiver in [receiver_a, receiver_b] {
            let error = receiver
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap()
                .err()
                .expect("every waiter must receive the panic error");
            assert!(error.contains("malformed decoder input"));
        }
        assert!(jobs.decodes.lock().unwrap().is_empty());
    }

    #[test]
    fn texture_job_queue_has_nonblocking_capacity() {
        let jobs = TextureJobs::new();
        let receivers: Vec<_> = (0..QUEUE_CAPACITY)
            .map(|_| jobs.submit("unopened.gltf", 0).expect("available slot"))
            .collect();
        assert!(jobs.submit("unopened.gltf", 0).is_none());
        drop(jobs.receive().unwrap());
        assert!(jobs.submit("unopened.gltf", 0).is_some());
        drop(receivers);
    }
}
