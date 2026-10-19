//! Reusable OSC listener.
//! Mechanical translation of Unity OscReceiver.cs.
//!
//! Wraps an OSC UDP server to provide a clean subscribe/unsubscribe API
//! with main-thread marshalling.
//!
//! Unity's OscJack callbacks fire on a background thread. This class queues
//! incoming messages and dispatches them on the main thread in update().
//! The Rust port preserves this threading model exactly.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Callback type for OSC subscribers. Receives (address, values) on the main thread.
/// Port of Unity `Action<string, float[]>`.
pub type OscCallback = Box<dyn Fn(&str, &[f32]) + Send + Sync + 'static>;

/// A subscriber that needs full precision and the arrival time: the values
/// as `f64` (doubles kept), and the instant the receive thread decoded the
/// packet (FRAME_EXACT_TRANSPORT_DESIGN.md D7).
pub type OscStampedCallback = Box<dyn Fn(&str, &[f64], Instant) + Send + Sync + 'static>;

/// Pending OSC message captured from the background thread.
/// Preserves the latest-message-per-address semantics: intermediate values
/// between frames are discarded (only the last arriving value is dispatched).
struct PendingMessage {
    address: String,
    /// Floats and ints, as plain subscribers have always seen them.
    values: Vec<f32>,
    /// Floats, ints and doubles at full precision, for stamped subscribers.
    wide: Vec<f64>,
    at: Instant,
}

/// Thread-safe message store shared between the background receive thread and
/// the main-thread dispatch loop.
#[derive(Default)]
struct MessageQueue {
    /// Each address's slot in `latest_list`.
    latest: HashMap<String, usize>,
    /// Latest message per address, in first-arrival order.
    latest_list: Vec<PendingMessage>,
}

impl MessageQueue {
    /// Record or replace the latest message for its address.
    /// Called from the background receive thread under lock.
    fn push(&mut self, message: PendingMessage) {
        if let Some(&slot) = self.latest.get(&message.address) {
            self.latest_list[slot] = message;
        } else {
            self.latest.insert(message.address.clone(), self.latest_list.len());
            self.latest_list.push(message);
        }
    }

    /// Drain all pending messages into `out`, clearing internal state.
    /// Called from the main thread under lock.
    fn drain(&mut self, out: &mut Vec<PendingMessage>) {
        out.append(&mut self.latest_list);
        self.latest.clear();
    }

    fn is_empty(&self) -> bool {
        self.latest_list.is_empty()
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
    /// Stamped subscribers, dispatched after the plain ones.
    stamped: HashMap<String, Vec<OscStampedCallback>>,

    /// Pre-allocated buffer for draining latestMessages on main thread.
    /// Port of `dispatchBuffer`.
    dispatch_buffer: Vec<PendingMessage>,

    /// Signals the background receive thread to exit.
    shutdown_flag: Arc<AtomicBool>,
    /// Background UDP receive thread handle.
    recv_thread: Option<std::thread::JoinHandle<()>>,
}

impl OscReceiver {
    pub fn new() -> Self {
        Self {
            listen_port: 9000,
            show_debug_logs: false,
            is_listening: false,
            queue: Arc::new(Mutex::new(MessageQueue::default())),
            subscribers: HashMap::new(),
            stamped: HashMap::new(),
            dispatch_buffer: Vec::new(),
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            recv_thread: None,
        }
    }

    pub fn is_listening(&self) -> bool {
        self.is_listening
    }
    pub fn listen_port(&self) -> i32 {
        self.listen_port
    }

    // =================================================================
    // Lifecycle
    // =================================================================

    /// Start the OSC UDP server.
    /// Port of Unity OscReceiver.StartListening().
    pub fn start_listening(&mut self) {
        if self.is_listening {
            return;
        }

        // Outside the live app, listen on an ephemeral loopback port so a test
        // never takes the app's fixed port or hears the network.
        let addr = if manifold_core::device_guard::allowed(manifold_core::device_guard::RealDevice::OscListen) {
            format!("0.0.0.0:{}", self.listen_port)
        } else {
            "127.0.0.1:0".to_string()
        };
        let socket = match std::net::UdpSocket::bind(&addr) {
            Ok(s) => s,
            Err(e) => {
                log::error!("[OscReceiver] Failed to bind UDP socket on {}: {}", addr, e);
                return;
            }
        };
        // Port 0 asks the OS for a free port; report the one actually bound.
        if let Ok(local) = socket.local_addr() {
            self.listen_port = i32::from(local.port());
        }

        // Short timeout so the thread checks shutdown_flag periodically.
        if let Err(e) = socket.set_read_timeout(Some(std::time::Duration::from_millis(100))) {
            log::error!("[OscReceiver] Failed to set socket timeout: {}", e);
            return;
        }

        let queue = Arc::clone(&self.queue);
        let shutdown_flag = Arc::clone(&self.shutdown_flag);
        self.shutdown_flag.store(false, Ordering::Relaxed);

        let handle = std::thread::spawn(move || {
            let mut buf = [0u8; 65536];
            loop {
                if shutdown_flag.load(Ordering::Relaxed) {
                    break;
                }

                let size = match socket.recv_from(&mut buf) {
                    Ok((sz, _)) => sz,
                    Err(ref e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::TimedOut =>
                    {
                        continue;
                    }
                    Err(e) => {
                        log::error!("[OscReceiver] recv_from error: {}", e);
                        continue;
                    }
                };

                let at = Instant::now();
                match rosc::decoder::decode_udp(&buf[..size]) {
                    Ok((_, packet)) => Self::handle_packet(packet, &queue, at),
                    Err(e) => log::error!("[OscReceiver] OSC decode error: {}", e),
                }
            }
        });

        self.recv_thread = Some(handle);
        self.is_listening = true;
        log::info!("[OscReceiver] Listening on port {}", self.listen_port);
    }

    fn handle_packet(packet: rosc::OscPacket, queue: &Arc<Mutex<MessageQueue>>, at: Instant) {
        match packet {
            rosc::OscPacket::Message(msg) => {
                let values: Vec<f32> = msg
                    .args
                    .iter()
                    .filter_map(|a| match a {
                        rosc::OscType::Float(f) => Some(*f),
                        rosc::OscType::Int(i) => Some(*i as f32),
                        _ => None,
                    })
                    .collect();
                let wide: Vec<f64> = msg
                    .args
                    .iter()
                    .filter_map(|a| match a {
                        rosc::OscType::Float(f) => Some(f64::from(*f)),
                        rosc::OscType::Int(i) => Some(f64::from(*i)),
                        rosc::OscType::Double(d) => Some(*d),
                        _ => None,
                    })
                    .collect();
                queue.lock().push(PendingMessage { address: msg.addr, values, wide, at });
            }
            rosc::OscPacket::Bundle(bundle) => {
                for inner in bundle.content {
                    Self::handle_packet(inner, queue, at);
                }
            }
        }
    }

    /// Stop the OSC UDP server and clear pending messages.
    /// Port of Unity OscReceiver.StopListening().
    pub fn stop_listening(&mut self) {
        if !self.is_listening {
            return;
        }

        // Signal the background thread to exit and wait for it.
        self.shutdown_flag.store(true, Ordering::Relaxed);
        if let Some(handle) = self.recv_thread.take() {
            let _ = handle.join();
        }

        self.is_listening = false;
        {
            let mut q = self.queue.lock();
            q.latest.clear();
            q.latest_list.clear();
        }
        // Reset flag so start_listening() can be called again.
        self.shutdown_flag.store(false, Ordering::Relaxed);

        log::info!("[OscReceiver] Stopped");
    }

    /// Change the listen port. Restarts the server if it was running.
    /// Port of Unity OscReceiver.SetPort().
    pub fn set_port(&mut self, port: i32) {
        let was_listening = self.is_listening;
        if was_listening {
            self.stop_listening();
        }
        self.listen_port = port;
        if was_listening {
            self.start_listening();
        }
    }

    // =================================================================
    // Main-thread dispatch (call once per frame, replaces Unity Update())
    // =================================================================

    /// Drain pending messages from the queue and dispatch to subscribers.
    /// Must be called from the main thread each frame.
    /// Port of Unity OscReceiver.Update().
    /// Queue a message exactly as the UDP thread does, so a headless test
    /// drives the real `update` → subscriber → router path.
    #[cfg(any(test, feature = "testkit"))]
    pub fn inject(&self, address: &str, values: &[f32]) {
        self.queue.lock().push(PendingMessage {
            address: address.to_string(),
            values: values.to_vec(),
            wide: values.iter().map(|&v| f64::from(v)).collect(),
            at: Instant::now(),
        });
    }

    pub fn update(&mut self) {
        // Snapshot latest messages under lock, dispatch outside lock.
        {
            let mut q = self.queue.lock();
            if q.is_empty() {
                return;
            }
            q.drain(&mut self.dispatch_buffer);
        }

        // Take ownership of the buffer to break the self-borrow:
        // dispatch_to_subscribers borrows self.subscribers, which conflicts
        // with self.dispatch_buffer. mem::take moves the Vec out (O(1)),
        // letting us iterate owned entries without cloning address/values.
        let mut buf = std::mem::take(&mut self.dispatch_buffer);
        for msg in &buf {
            self.dispatch_to_subscribers(&msg.address, &msg.values);
            if let Some(callbacks) = self.stamped.get(&msg.address) {
                for cb in callbacks {
                    cb(&msg.address, &msg.wide, msg.at);
                }
            }
        }
        buf.clear();
        self.dispatch_buffer = buf;
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
        self.stamped.remove(address);
    }

    /// Subscribe with full-precision values and the receive-thread arrival
    /// instant. `unsubscribe_all` removes it.
    pub fn subscribe_stamped(&mut self, address: &str, callback: OscStampedCallback) {
        self.stamped.entry(address.to_string()).or_default().push(callback);
    }
}

impl Default for OscReceiver {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for OscReceiver {
    fn drop(&mut self) {
        self.stop_listening();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type RecordedOsc = Arc<Mutex<Vec<(String, Vec<f32>)>>>;

    /// Subscribes a callback that records every dispatch into a shared log,
    /// so tests can assert exactly what (and how often) the receiver fired.
    fn recording_receiver(address: &str) -> (OscReceiver, RecordedOsc) {
        let mut receiver = OscReceiver::new();
        let log: RecordedOsc = Arc::new(Mutex::new(Vec::new()));
        let log_clone = Arc::clone(&log);
        receiver.subscribe(
            address,
            Box::new(move |addr, values| {
                log_clone.lock().push((addr.to_string(), values.to_vec()));
            }),
        );
        (receiver, log)
    }

    /// Round-trips a message through rosc's real encode/decode (the same
    /// `decode_udp` the background UDP thread calls), feeds it through
    /// `handle_packet` (the real dispatch path), then drains via the public
    /// `update()` — the same sequence a real UDP packet goes through, minus
    /// the socket itself.
    #[test]
    fn osc_message_round_trip_preserves_address_and_args() {
        let (mut receiver, log) = recording_receiver("/manifold/test");
        let packet = rosc::OscPacket::Message(rosc::OscMessage {
            addr: "/manifold/test".to_string(),
            args: vec![rosc::OscType::Float(1.5), rosc::OscType::Float(-2.25)],
        });
        let bytes = rosc::encoder::encode(&packet).expect("encode");
        let (_, decoded) = rosc::decoder::decode_udp(&bytes).expect("decode");

        OscReceiver::handle_packet(decoded, &receiver.queue, Instant::now());
        receiver.update();

        let entries = log.lock();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "/manifold/test");
        assert_eq!(entries[0].1, vec![1.5, -2.25]);
    }

    /// `handle_packet` converts `OscType::Int` to f32 and silently drops
    /// non-numeric arg types (matches the filter_map in `handle_packet`).
    #[test]
    fn osc_message_filters_non_numeric_args_and_converts_ints() {
        let (mut receiver, log) = recording_receiver("/manifold/mixed");
        let packet = rosc::OscPacket::Message(rosc::OscMessage {
            addr: "/manifold/mixed".to_string(),
            args: vec![
                rosc::OscType::Int(7),
                rosc::OscType::String("ignored".to_string()),
                rosc::OscType::Float(3.5),
            ],
        });
        let bytes = rosc::encoder::encode(&packet).expect("encode");
        let (_, decoded) = rosc::decoder::decode_udp(&bytes).expect("decode");

        OscReceiver::handle_packet(decoded, &receiver.queue, Instant::now());
        receiver.update();

        let entries = log.lock();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].1,
            vec![7.0, 3.5],
            "Int converts to f32; String is dropped"
        );
    }

    /// Two arrivals at the SAME address before a drain must coalesce: only
    /// the newest values dispatch, and the subscriber fires exactly once —
    /// `MessageQueue::push`'s latest-per-address semantics.
    #[test]
    fn osc_latest_per_address_coalesces_to_newest() {
        let (mut receiver, log) = recording_receiver("/manifold/coalesce");

        for value in [1.0_f32, 2.0_f32] {
            let packet = rosc::OscPacket::Message(rosc::OscMessage {
                addr: "/manifold/coalesce".to_string(),
                args: vec![rosc::OscType::Float(value)],
            });
            let bytes = rosc::encoder::encode(&packet).expect("encode");
            let (_, decoded) = rosc::decoder::decode_udp(&bytes).expect("decode");
            OscReceiver::handle_packet(decoded, &receiver.queue, Instant::now());
        }
        receiver.update();

        let entries = log.lock();
        assert_eq!(entries.len(), 1, "only one dispatch per address per drain");
        assert_eq!(entries[0].1, vec![2.0], "the newest value must win");
    }

    /// `handle_packet` recurses into nested bundles (bundle-of-bundle).
    #[test]
    fn osc_bundle_recursion_dispatches_nested_messages() {
        let (mut receiver, log) = recording_receiver("/manifold/nested");
        let inner_message = rosc::OscPacket::Message(rosc::OscMessage {
            addr: "/manifold/nested".to_string(),
            args: vec![rosc::OscType::Float(9.0)],
        });
        let inner_bundle = rosc::OscPacket::Bundle(rosc::OscBundle {
            timetag: rosc::OscTime::from((0u32, 0u32)),
            content: vec![inner_message],
        });
        let outer_bundle = rosc::OscPacket::Bundle(rosc::OscBundle {
            timetag: rosc::OscTime::from((0u32, 0u32)),
            content: vec![inner_bundle],
        });
        let bytes = rosc::encoder::encode(&outer_bundle).expect("encode");
        let (_, decoded) = rosc::decoder::decode_udp(&bytes).expect("decode");

        OscReceiver::handle_packet(decoded, &receiver.queue, Instant::now());
        receiver.update();

        let entries = log.lock();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "/manifold/nested");
        assert_eq!(entries[0].1, vec![9.0]);
    }

    /// A stamped subscriber gets doubles at full precision and the receive
    /// thread's arrival instant; a plain one on the same address still sees
    /// floats only.
    #[test]
    fn stamped_subscribers_get_doubles_and_the_arrival() {
        let (mut receiver, log) = recording_receiver("/time");
        type Stamped = Arc<Mutex<Vec<(Vec<f64>, Instant)>>>;
        let stamped: Stamped = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&stamped);
        receiver.subscribe_stamped(
            "/time",
            Box::new(move |_, values, at| sink.lock().push((values.to_vec(), at))),
        );
        let seconds = 3_600.123_456_789_f64;
        let packet = rosc::OscPacket::Message(rosc::OscMessage {
            addr: "/time".to_string(),
            args: vec![rosc::OscType::Double(seconds)],
        });
        let bytes = rosc::encoder::encode(&packet).expect("encode");
        let (_, decoded) = rosc::decoder::decode_udp(&bytes).expect("decode");
        let at = Instant::now();

        OscReceiver::handle_packet(decoded, &receiver.queue, at);
        receiver.update();

        assert_eq!(*stamped.lock(), vec![(vec![seconds], at)]);
        assert_eq!(log.lock()[0].1, Vec::<f32>::new(), "plain subscribers never saw doubles");
        receiver.unsubscribe_all("/time");
        assert!(receiver.stamped.is_empty());
    }
}
