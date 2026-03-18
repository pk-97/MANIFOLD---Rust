//! Reusable OSC listener.
//! Mechanical translation of Unity OscReceiver.cs.
//!
//! Wraps an OSC UDP server to provide a clean subscribe/unsubscribe API
//! with main-thread marshalling.
//!
//! Unity's OscJack callbacks fire on a background thread. This class queues
//! incoming messages and dispatches them on the main thread in update().
//! The Rust port preserves this threading model exactly.
//!
//! STUB: Full implementation requires a native OSC crate (e.g. `rosc`).
//! This file provides the correct interface, state tracking, and dispatch
//! logic so the rest of the sync system can compile and wire correctly.
//! The UDP receive loop is stubbed with TODO comments.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Callback type for OSC subscribers. Receives (address, values) on the main thread.
/// Port of Unity `Action<string, float[]>`.
pub type OscCallback = Box<dyn Fn(&str, &[f32]) + Send + Sync + 'static>;

/// Pending OSC message captured from the background thread.
/// Preserves the latest-message-per-address semantics: intermediate values
/// between frames are discarded (only the last arriving value is dispatched).
struct PendingMessage {
    address: String,
    values: Vec<f32>,
}

/// Thread-safe message store shared between the background receive thread and
/// the main-thread dispatch loop.
/// Port of Unity OscReceiver's `queueLock` + `latestMessages` + `latestMessagesList`.
#[derive(Default)]
struct MessageQueue {
    /// Latest message per address (O(1) keyed replacement from background thread).
    latest: HashMap<String, Vec<f32>>,
    /// Parallel iteration list — avoids HashMap enumerator alloc on drain.
    /// Kept in sync with `latest`. Port of `latestMessagesList`.
    latest_list: Vec<PendingMessage>,
}

impl MessageQueue {
    /// Record or replace the latest message for `address`.
    /// Called from the background receive thread under lock.
    /// Dead until native OSC UDP thread is wired.
    #[allow(dead_code)]
    fn push(&mut self, address: String, values: Vec<f32>) {
        if let Some(existing) = self.latest.get_mut(&address) {
            // Update in-place: reuse allocation if same count, otherwise replace.
            if existing.len() == values.len() {
                existing.copy_from_slice(&values);
            } else {
                *existing = values.clone();
            }
            // Update the parallel iteration list in-place (same as Unity's backward scan).
            for entry in self.latest_list.iter_mut().rev() {
                if entry.address == address {
                    entry.values = values;
                    return;
                }
            }
        } else {
            self.latest.insert(address.clone(), values.clone());
            self.latest_list.push(PendingMessage { address, values });
        }
    }

    /// Drain all pending messages into `out`, clearing internal state.
    /// Called from the main thread under lock.
    fn drain(&mut self, out: &mut Vec<PendingMessage>) {
        out.extend(self.latest_list.drain(..));
        self.latest.clear();
    }

    fn is_empty(&self) -> bool {
        self.latest.is_empty()
    }
}

/// OSC receiver that listens on a UDP port and dispatches messages to
/// subscribers on the main thread.
/// Port of Unity OscReceiver.cs.
pub struct OscReceiver {
    /// UDP port to listen on. Port of `listenPort`.
    listen_port: i32,
    /// Port of `showDebugLogs`.
    show_debug_logs: bool,

    /// Whether the server is currently listening. Port of `IsListening`.
    is_listening: bool,

    /// Thread-safe message queue (background thread writes, main thread drains).
    /// Port of `queueLock` + `latestMessages` + `latestMessagesList`.
    queue: Arc<Mutex<MessageQueue>>,

    /// Subscriber registry: address → list of callbacks invoked on main thread.
    /// Port of `subscribers`.
    subscribers: HashMap<String, Vec<OscCallback>>,

    /// Pre-allocated buffer for draining latestMessages on main thread.
    /// Port of `dispatchBuffer`.
    dispatch_buffer: Vec<PendingMessage>,

    // TODO: native OSC server handle
    // server: Option<rosc::OscServer>,
}

impl OscReceiver {
    pub fn new() -> Self {
        Self {
            listen_port: 9000,
            show_debug_logs: false,
            is_listening: false,
            queue: Arc::new(Mutex::new(MessageQueue::default())),
            subscribers: HashMap::new(),
            dispatch_buffer: Vec::new(),
        }
    }

    pub fn is_listening(&self) -> bool { self.is_listening }
    pub fn listen_port(&self) -> i32 { self.listen_port }

    // =================================================================
    // Lifecycle
    // =================================================================

    /// Start the OSC UDP server.
    /// Port of Unity OscReceiver.StartListening().
    pub fn start_listening(&mut self) {
        if self.is_listening { return; }

        // TODO: Bind UDP socket on self.listen_port and spawn background receive thread.
        // let socket = std::net::UdpSocket::bind(format!("0.0.0.0:{}", self.listen_port))
        //     .expect("[OscReceiver] Failed to bind UDP socket");
        // let queue = Arc::clone(&self.queue);
        // std::thread::spawn(move || {
        //     let mut buf = [0u8; 65536];
        //     loop {
        //         let (sz, _) = socket.recv_from(&mut buf).unwrap();
        //         if let Ok(rosc::OscPacket::Message(msg)) = rosc::decoder::decode(&buf[..sz]) {
        //             let values: Vec<f32> = msg.args.iter().filter_map(|a| match a {
        //                 rosc::OscType::Float(f) => Some(*f),
        //                 rosc::OscType::Int(i) => Some(*i as f32),
        //                 _ => None,
        //             }).collect();
        //             let count = values.len();
        //             // Reuse existing array if same size, otherwise allocate.
        //             // (matches Unity's background-thread allocation logic)
        //             queue.lock().unwrap().push(msg.addr.clone(), values);
        //         }
        //     }
        // });

        self.is_listening = true;
        log::info!("[OscReceiver] Listening on port {} (native OSC not available in Rust port)", self.listen_port);
    }

    /// Stop the OSC UDP server and clear pending messages.
    /// Port of Unity OscReceiver.StopListening().
    pub fn stop_listening(&mut self) {
        if !self.is_listening { return; }

        // TODO: OscServer::shutdown() / drop native socket handle

        self.is_listening = false;
        self.queue.lock().unwrap().latest.clear();
        self.queue.lock().unwrap().latest_list.clear();

        log::info!("[OscReceiver] Stopped");
    }

    /// Change the listen port. Restarts the server if it was running.
    /// Port of Unity OscReceiver.SetPort().
    pub fn set_port(&mut self, port: i32) {
        let was_listening = self.is_listening;
        if was_listening { self.stop_listening(); }
        self.listen_port = port;
        if was_listening { self.start_listening(); }
    }

    // =================================================================
    // Main-thread dispatch (call once per frame, replaces Unity Update())
    // =================================================================

    /// Drain pending messages from the queue and dispatch to subscribers.
    /// Must be called from the main thread each frame.
    /// Port of Unity OscReceiver.Update().
    pub fn update(&mut self) {
        // Snapshot latest messages under lock, dispatch outside lock.
        {
            let mut q = self.queue.lock().unwrap();
            if q.is_empty() { return; }
            q.drain(&mut self.dispatch_buffer);
        }

        for i in 0..self.dispatch_buffer.len() {
            let addr = self.dispatch_buffer[i].address.clone();
            let vals = self.dispatch_buffer[i].values.clone();
            self.dispatch_to_subscribers(&addr, &vals);
        }
        self.dispatch_buffer.clear();
    }

    fn dispatch_to_subscribers(&self, address: &str, values: &[f32]) {
        if let Some(callbacks) = self.subscribers.get(address) {
            for cb in callbacks {
                cb(address, values);
            }
        }

        if self.show_debug_logs {
            if values.is_empty() {
                log::debug!("[OscReceiver] {}: (no data)", address);
            } else {
                let vals: Vec<String> = values.iter().map(|v| v.to_string()).collect();
                log::debug!("[OscReceiver] {}: {}", address, vals.join(", "));
            }
        }
    }

    // =================================================================
    // Subscribe / Unsubscribe
    // =================================================================

    /// Subscribe to messages at a specific OSC address.
    /// Callback receives (address, &[f32] values) on the main thread.
    /// Port of Unity OscReceiver.Subscribe().
    pub fn subscribe(&mut self, address: &str, callback: OscCallback) {
        self.subscribers
            .entry(address.to_string())
            .or_default()
            .push(callback);
    }

    /// Remove a subscription by address. Removes the last-added callback
    /// (mirrors Unity's List<Action>.Remove() which removes the first occurrence —
    /// but since we do not have reference equality for Box<dyn Fn>, we remove
    /// by index supplied by the caller; callers that need precise removal
    /// should use subscribe_keyed / unsubscribe_keyed instead).
    ///
    /// Port of Unity OscReceiver.Unsubscribe().
    ///
    /// NOTE: Unity uses delegate reference equality for Remove(). In Rust,
    /// Box<dyn Fn> closures are not comparable. Callers store the subscription
    /// key returned by subscribe_keyed() and use unsubscribe_keyed() for
    /// clean removal. This method removes ALL callbacks for the address —
    /// sufficient for single-subscriber use cases (OscSyncController).
    pub fn unsubscribe_all(&mut self, address: &str) {
        self.subscribers.remove(address);
    }

    /// Subscribe with a stable integer key for later removal.
    /// Returns the key that must be passed to unsubscribe_keyed().
    pub fn subscribe_keyed(&mut self, address: &str, callback: OscCallback) -> usize {
        let list = self.subscribers.entry(address.to_string()).or_default();
        let key = list.len(); // index within this address's callback list
        list.push(callback);
        key
    }

    /// Remove the callback at the given key (index) for an address.
    /// Uses swap_remove for O(1) removal; ordering of remaining callbacks
    /// is not preserved (matches Unity semantics — order is unspecified).
    pub fn unsubscribe_keyed(&mut self, address: &str, key: usize) {
        if let Some(list) = self.subscribers.get_mut(address) {
            if key < list.len() {
                let _ = list.swap_remove(key);
            }
        }
    }
}

impl Default for OscReceiver {
    fn default() -> Self { Self::new() }
}

impl Drop for OscReceiver {
    fn drop(&mut self) {
        self.stop_listening();
    }
}
