//! Central registry for OSC-controllable float parameters.
//! Mechanical translation of Unity OscParameterRegistry.cs.
//!
//! Address convention: /{subsystem}/{param} (e.g. /master/opacity)
//!
//! Usage:
//!   OscParameterRegistry::global().register("/master/bloom", |v| set_bloom(v));
//!   OscParameterRegistry::global().unregister_by_prefix("/master/");
//!
//! Unity is a MonoBehaviour singleton (self-creates via Instance property).
//! Rust port: a process-global instance accessed via OscParameterRegistry::global().
//! The singleton is initialised on first access.
//!
//! STUB: OscReceiver native I/O is not available — see osc_receiver.rs.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use crate::osc_receiver::OscReceiver;

/// Setter callback for a registered float parameter.
/// Port of Unity `Action<float>`.
pub type FloatSetter = Box<dyn Fn(f32) + Send + Sync + 'static>;

/// Process-global singleton instance.
/// Port of Unity `private static OscParameterRegistry instance`.
static INSTANCE: OnceLock<Mutex<OscParameterRegistry>> = OnceLock::new();

/// Central registry for OSC-controllable float parameters.
/// Port of Unity OscParameterRegistry.cs.
pub struct OscParameterRegistry {
    /// Shared OSC receiver (created/ensured on first Register call).
    /// Port of Unity `private OscReceiver oscReceiver`.
    osc_receiver: Option<OscReceiver>,

    /// Address → float setter.
    /// Port of Unity `Dictionary<string, Action<float>> parameters`.
    parameters: HashMap<String, FloatSetter>,

    /// Address → subscription key for clean unsubscription.
    /// Port of Unity `Dictionary<string, Action<string, float[]>> oscCallbacks`.
    osc_callback_keys: HashMap<String, usize>,

    /// Pre-allocated buffer for safe dictionary iteration during prefix unregister.
    /// Port of Unity `List<string> releaseBuffer`.
    release_buffer: Vec<String>,
}

impl OscParameterRegistry {
    fn new() -> Self {
        let mut registry = Self {
            osc_receiver: None,
            parameters: HashMap::new(),
            osc_callback_keys: HashMap::new(),
            release_buffer: Vec::new(),
        };
        registry.ensure_receiver();
        registry
    }

    /// Shared singleton. Self-creates if not already initialised.
    /// Ensures OscReceiver exists and starts listening on first access.
    /// Port of Unity `OscParameterRegistry.Instance`.
    pub fn global() -> &'static Mutex<OscParameterRegistry> {
        INSTANCE.get_or_init(|| Mutex::new(OscParameterRegistry::new()))
    }

    /// Find or create OscReceiver and start listening.
    /// Port of Unity OscParameterRegistry.EnsureReceiver().
    fn ensure_receiver(&mut self) {
        if let Some(ref r) = self.osc_receiver {
            if r.is_listening() { return; }
        }

        if self.osc_receiver.is_none() {
            self.osc_receiver = Some(OscReceiver::new());
        }

        if let Some(ref mut r) = self.osc_receiver {
            if !r.is_listening() {
                r.start_listening();
            }
        }
    }

    pub fn registered_count(&self) -> usize { self.parameters.len() }

    // =================================================================
    // Register / Unregister
    // =================================================================

    /// Register a float parameter at an OSC address.
    /// Re-registering the same address replaces the previous setter.
    /// Port of Unity OscParameterRegistry.Register().
    pub fn register(&mut self, address: &str, setter: FloatSetter) {
        if address.is_empty() { return; }

        // Replace existing registration for same address.
        if self.parameters.contains_key(address) {
            let addr = address.to_string();
            self.unregister(&addr);
        }

        self.parameters.insert(address.to_string(), setter);

        self.ensure_receiver();

        // Subscribe on the OscReceiver.
        // Unity creates a closure: (addr, values) => if values.Length > 0 && parameters.TryGetValue(addr) → setter(values[0])
        // In Rust we cannot close over self (borrow conflict), so the dispatch
        // happens in update() → dispatch_to_subscribers → back to this registry.
        //
        // TODO: When the OscReceiver is fully wired (native UDP live), subscribe
        // the per-address callback here. The pattern is:
        //   let key = receiver.subscribe_keyed(address, Box::new(move |addr, values| {
        //       if let Some(v) = values.first() { setter(*v); }
        //   }));
        //   self.osc_callback_keys.insert(address.to_string(), key);
        //
        // For the stub phase the OscReceiver dispatches through update() and
        // OscParameterRegistry::dispatch() must be called by the host each frame.
        let _ = self.osc_callback_keys.entry(address.to_string()).or_insert(0);
    }

    /// Unregister a single parameter by address.
    /// Port of Unity OscParameterRegistry.Unregister().
    pub fn unregister(&mut self, address: &str) {
        if !self.parameters.remove(address).is_some() { return; }

        if let Some(ref mut receiver) = self.osc_receiver {
            if let Some(_key) = self.osc_callback_keys.remove(address) {
                receiver.unsubscribe_all(address);
            }
        }
    }

    /// Unregister all parameters whose address starts with prefix.
    /// Use for subsystem cleanup: e.g. unregister_by_prefix("/master/")
    /// Port of Unity OscParameterRegistry.UnregisterByPrefix().
    pub fn unregister_by_prefix(&mut self, prefix: &str) {
        self.release_buffer.clear();
        for addr in self.parameters.keys() {
            if addr.starts_with(prefix) {
                self.release_buffer.push(addr.clone());
            }
        }
        // Drain release_buffer without holding a borrow on parameters.
        let to_remove: Vec<String> = self.release_buffer.drain(..).collect();
        for addr in to_remove {
            self.unregister(&addr);
        }
    }

    /// Dispatch an incoming OSC message to any registered parameter setter.
    /// Call this from the main-thread update loop after OscReceiver::update().
    /// This is the Rust equivalent of the per-address closure Unity creates in Register().
    ///
    /// Port of Unity: the inline closure `(addr, values) => if values.Length > 0 && parameters.TryGetValue(addr, out var s) → s(values[0])`.
    pub fn dispatch(&self, address: &str, values: &[f32]) {
        if let (Some(v), Some(setter)) = (values.first(), self.parameters.get(address)) {
            setter(*v);
        }
    }

    // =================================================================
    // Lifecycle (called by host on shutdown — replaces Unity OnDestroy)
    // =================================================================

    /// Clean up all subscriptions. Call on application shutdown.
    /// Port of Unity OscParameterRegistry.OnDestroy().
    pub fn destroy(&mut self) {
        if let Some(ref mut receiver) = self.osc_receiver {
            for addr in self.osc_callback_keys.keys() {
                receiver.unsubscribe_all(addr);
            }
        }
        self.parameters.clear();
        self.osc_callback_keys.clear();
    }
}
