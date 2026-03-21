// Generic background worker for non-blocking CPU-heavy tasks.
// Companion to gpu_readback.rs — ReadbackRequest handles GPU→CPU,
// BackgroundWorker handles CPU→result on a dedicated thread.
//
// Usage pattern:
//   let worker = BackgroundWorker::new(|| {
//       let plugin = NativePlugin::new().unwrap();
//       move |req: MyRequest| -> MyResponse {
//           plugin.process(&req.data);
//           MyResponse { ... }
//       }
//   });
//
//   // Frame N:   worker.submit(request);
//   // Frame N+1: if let Some(result) = worker.try_recv() { use(result); }
//
// Latest-data semantics: if multiple requests queue while the worker is busy,
// only the newest is processed. Stale requests are discarded.

use std::sync::mpsc;
use std::thread;

/// A single-flight background worker that processes requests on a dedicated thread.
///
/// The native plugin instance lives on the worker thread — created by the
/// `make_processor` closure passed to `new()`. The main thread only sends
/// request data and receives results, never touching the plugin directly.
pub struct BackgroundWorker<Req: Send + 'static, Res: Send + 'static> {
    req_tx: Option<mpsc::Sender<Req>>,
    res_rx: mpsc::Receiver<Res>,
    thread: Option<thread::JoinHandle<()>>,
    in_flight: bool,
}

impl<Req: Send + 'static, Res: Send + 'static> BackgroundWorker<Req, Res> {
    /// Spawn a background worker thread.
    ///
    /// `make_processor` runs ONCE on the worker thread. It should create the
    /// native plugin instance and return a closure that processes requests.
    /// The returned closure captures the plugin by move — it never leaves
    /// the worker thread.
    ///
    /// The thread is spawned eagerly and blocks on the channel until the
    /// first `submit()` call.
    pub fn new<F, P>(make_processor: F) -> Self
    where
        F: FnOnce() -> P + Send + 'static,
        P: FnMut(Req) -> Res + 'static,
    {
        let (req_tx, req_rx) = mpsc::channel::<Req>();
        let (res_tx, res_rx) = mpsc::channel::<Res>();

        let thread = thread::spawn(move || {
            let mut processor = make_processor();
            // Block on first message; exit when sender drops.
            while let Ok(first) = req_rx.recv() {
                // Drain to latest — discard stale requests.
                let mut latest = first;
                while let Ok(newer) = req_rx.try_recv() {
                    latest = newer;
                }
                let result = processor(latest);
                if res_tx.send(result).is_err() {
                    break; // Receiver dropped — shut down.
                }
            }
        });

        Self {
            req_tx: Some(req_tx),
            res_rx,
            thread: Some(thread),
            in_flight: false,
        }
    }

    /// Try to create a worker where the plugin might not be available.
    ///
    /// `try_make_processor` runs on the worker thread and returns `Some(processor)`
    /// if the native plugin loaded, or `None` if not. Returns `None` from this
    /// method if the plugin failed to load (the worker thread exits cleanly).
    pub fn try_new<F, P>(try_make_processor: F) -> Option<Self>
    where
        F: FnOnce() -> Option<P> + Send + 'static,
        P: FnMut(Req) -> Res + 'static,
    {
        let (req_tx, req_rx) = mpsc::channel::<Req>();
        let (res_tx, res_rx) = mpsc::channel::<Res>();
        // Channel to signal whether plugin creation succeeded.
        let (ready_tx, ready_rx) = mpsc::channel::<bool>();

        let thread = thread::spawn(move || {
            let processor = try_make_processor();
            match processor {
                Some(mut proc) => {
                    let _ = ready_tx.send(true);
                    while let Ok(first) = req_rx.recv() {
                        let mut latest = first;
                        while let Ok(newer) = req_rx.try_recv() {
                            latest = newer;
                        }
                        let result = proc(latest);
                        if res_tx.send(result).is_err() {
                            break;
                        }
                    }
                }
                None => {
                    let _ = ready_tx.send(false);
                    // Thread exits — plugin not available.
                }
            }
        });

        // Wait for the worker thread to report plugin status.
        let available = ready_rx.recv().unwrap_or(false);
        if available {
            Some(Self {
                req_tx: Some(req_tx),
                res_rx,
                thread: Some(thread),
                in_flight: false,
            })
        } else {
            // Thread already exited. Join it.
            let _ = thread.join();
            None
        }
    }

    /// Send a request to the worker thread.
    /// If the worker is busy, the request queues — the worker will drain to
    /// the latest when it finishes its current job.
    pub fn submit(&mut self, req: Req) {
        if let Some(tx) = &self.req_tx
            && tx.send(req).is_ok() {
                self.in_flight = true;
            }
    }

    /// Non-blocking poll for a completed result.
    /// Returns `Some(result)` if the worker finished processing.
    pub fn try_recv(&mut self) -> Option<Res> {
        match self.res_rx.try_recv() {
            Ok(res) => {
                self.in_flight = false;
                Some(res)
            }
            Err(_) => None,
        }
    }

    /// True if a request has been submitted but no result received yet.
    pub fn is_busy(&self) -> bool {
        self.in_flight
    }
}

impl<Req: Send + 'static, Res: Send + 'static> Drop for BackgroundWorker<Req, Res> {
    fn drop(&mut self) {
        // Drop the sender first — this causes the worker's recv() to return Err,
        // breaking its loop so the thread can exit.
        self.req_tx.take();
        // Now join the thread.
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}
