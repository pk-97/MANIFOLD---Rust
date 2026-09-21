//! Background discovery of MIDI input source names.
//!
//! Port enumeration can involve a slow operating-system call.  Keep that
//! work off the content thread and publish complete, changed snapshots for
//! callers to consume at their own pace.

use std::sync::mpsc::{self, Receiver, TryRecvError, TrySendError};
use std::thread;
use std::time::Duration;

const DISCOVERY_INTERVAL: Duration = Duration::from_secs(2);

trait SourceScanner: Send {
    fn scan(&mut self) -> Result<Vec<String>, String>;
}

struct MidirSourceScanner {
    midi_input: Option<midir::MidiInput>,
}

impl SourceScanner for MidirSourceScanner {
    fn scan(&mut self) -> Result<Vec<String>, String> {
        // Client construction can be as slow as enumeration. Both happen on
        // the worker, and a transient creation failure is retried next pass.
        if self.midi_input.is_none() {
            self.midi_input = Some(
                midir::MidiInput::new("manifold-source-discovery")
                    .map_err(|error| format!("failed to create MIDI input client: {error}"))?,
            );
        }
        let midi_input = self.midi_input.as_ref().expect("initialized above");
        let ports = midi_input.ports();
        let mut names = Vec::with_capacity(ports.len());
        for port in ports {
            let name = midi_input
                .port_name(&port)
                .map_err(|error| format!("failed to read MIDI input port name: {error}"))?;
            names.push(name);
        }
        Ok(names)
    }
}

/// Asynchronously discovers the ordered names of MIDI input sources.
pub struct MidiSourceDiscovery {
    result_rx: Receiver<Vec<String>>,
    stop_tx: Option<mpsc::SyncSender<()>>,
}

impl MidiSourceDiscovery {
    /// Starts the single background discovery worker.
    pub fn new() -> Self {
        Self::spawn(
            Box::new(MidirSourceScanner { midi_input: None }),
            DISCOVERY_INTERVAL,
        )
    }

    /// Returns a completed snapshot without waiting.
    pub fn poll(&mut self) -> Option<Vec<String>> {
        self.result_rx.try_recv().ok()
    }

    fn spawn(scanner: Box<dyn SourceScanner>, interval: Duration) -> Self {
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let (stop_tx, stop_rx) = mpsc::sync_channel(1);

        thread::Builder::new()
            .name("manifold-midi-source-discovery".to_owned())
            .spawn(move || run_worker(scanner, result_tx, stop_rx, interval))
            .expect("failed to start MIDI source discovery worker");

        Self {
            result_rx,
            stop_tx: Some(stop_tx),
        }
    }

    #[cfg(test)]
    fn with_scanner<S>(scanner: S, interval: Duration) -> Self
    where
        S: SourceScanner + 'static,
    {
        Self::spawn(Box::new(scanner), interval)
    }
}

impl Default for MidiSourceDiscovery {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for MidiSourceDiscovery {
    fn drop(&mut self) {
        // A scan may be blocked inside the operating-system MIDI API.  Drop
        // the stop sender immediately instead of joining the worker; the
        // worker checks the disconnected receiver after its current scan.
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.try_send(());
        }
    }
}

fn stop_requested(stop_rx: &Receiver<()>) -> bool {
    match stop_rx.try_recv() {
        Ok(()) | Err(TryRecvError::Disconnected) => true,
        Err(TryRecvError::Empty) => false,
    }
}

fn run_worker(
    mut scanner: Box<dyn SourceScanner>,
    result_tx: mpsc::SyncSender<Vec<String>>,
    stop_rx: Receiver<()>,
    interval: Duration,
) {
    let mut last_published: Option<Vec<String>> = None;
    loop {
        if stop_requested(&stop_rx) {
            return;
        }

        match scanner.scan() {
            Ok(names) => {
                if stop_requested(&stop_rx) {
                    return;
                }
                let changed = last_published
                    .as_ref()
                    .is_none_or(|previous| previous != &names);
                if changed {
                    let published_names = names.clone();
                    match result_tx.try_send(names) {
                        Ok(()) => {
                            // The channel owns the sent vector, so retain a
                            // comparison copy for the next discovery pass.
                            // This only occurs for changed background results.
                            last_published = Some(published_names);
                        }
                        Err(TrySendError::Full(_names)) => {
                            // Keep the previous published snapshot.  The next
                            // pass retries the current complete snapshot once
                            // the consumer drains the bounded queue.
                        }
                        Err(TrySendError::Disconnected(_names)) => return,
                    }
                }
            }
            Err(error) => {
                log::warn!("[MidiSourceDiscovery] MIDI input scan failed: {error}");
            }
        }

        match stop_rx.recv_timeout(interval) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MidiSourceDiscovery, SourceScanner};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::time::Duration;

    type Scan = Result<Vec<String>, String>;
    struct ControlledScanner {
        updates: Receiver<Scan>,
        scanned: Sender<()>,
        dropped: Sender<()>,
    }

    impl SourceScanner for ControlledScanner {
        fn scan(&mut self) -> Scan {
            let _ = self.scanned.send(());
            self.updates.recv().map_err(|error| error.to_string())?
        }
    }

    impl Drop for ControlledScanner {
        fn drop(&mut self) {
            let _ = self.dropped.send(());
        }
    }

    fn controlled() -> (
        MidiSourceDiscovery,
        Sender<Scan>,
        Receiver<()>,
        Receiver<()>,
    ) {
        let (updates, updates_rx) = mpsc::channel();
        let (scanned_tx, scanned) = mpsc::channel();
        let (dropped_tx, dropped) = mpsc::channel();
        let discovery = MidiSourceDiscovery::with_scanner(
            ControlledScanner {
                updates: updates_rx,
                scanned: scanned_tx,
                dropped: dropped_tx,
            },
            Duration::from_millis(1),
        );
        (discovery, updates, scanned, dropped)
    }

    fn next_snapshot(discovery: &MidiSourceDiscovery) -> Vec<String> {
        discovery
            .result_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker did not publish a snapshot")
    }

    #[test]
    fn publishes_ordered_empty_and_hotplug_replacements_without_duplicates() {
        let (discovery, updates, _scanned, _dropped) = controlled();
        for names in [
            vec!["Keyboard".into(), "Pad".into()],
            vec![],
            vec!["New device".into()],
        ] {
            updates.send(Ok(names.clone())).unwrap();
            assert_eq!(next_snapshot(&discovery), names);
        }
        updates.send(Ok(vec!["New device".into()])).unwrap();
        assert!(
            discovery
                .result_rx
                .recv_timeout(Duration::from_millis(20))
                .is_err()
        );
    }

    #[test]
    fn scan_failure_preserves_last_good_snapshot() {
        let (discovery, updates, _scanned, _dropped) = controlled();
        updates.send(Ok(vec!["Stable".into()])).unwrap();
        assert_eq!(next_snapshot(&discovery), vec!["Stable"]);
        updates.send(Err("device list unavailable".into())).unwrap();
        updates.send(Ok(vec!["Stable".into()])).unwrap();
        assert!(
            discovery
                .result_rx
                .recv_timeout(Duration::from_millis(20))
                .is_err()
        );
        updates.send(Ok(vec!["Recovered".into()])).unwrap();
        assert_eq!(next_snapshot(&discovery), vec!["Recovered"]);
    }

    #[test]
    fn full_result_queue_retries_an_unpublished_snapshot() {
        let (discovery, updates, scanned, _dropped) = controlled();
        for name in ["First", "Second", "Second"] {
            updates.send(Ok(vec![name.into()])).unwrap();
            scanned.recv_timeout(Duration::from_secs(1)).unwrap();
        }
        // The third scan proves the second send already hit the full queue.
        assert_eq!(next_snapshot(&discovery), vec!["First"]);
        updates.send(Ok(vec!["Second".into()])).unwrap();
        assert_eq!(next_snapshot(&discovery), vec!["Second"]);
    }

    #[test]
    fn poll_and_drop_do_not_wait_for_a_blocked_scan_and_worker_exits() {
        let (mut discovery, updates, scanned, dropped) = controlled();
        scanned.recv_timeout(Duration::from_secs(1)).unwrap();
        // The scanner blocks on updates until the test releases it. Run both
        // consumer operations behind a timeout so a regression cannot hang.
        let (done_tx, done_rx) = mpsc::channel();
        let consumer = std::thread::spawn(move || {
            assert!(discovery.poll().is_none());
            drop(discovery);
            done_tx.send(()).unwrap();
        });
        let finished = done_rx.recv_timeout(Duration::from_secs(1));
        drop(updates);
        consumer.join().unwrap();
        assert!(finished.is_ok(), "poll/drop waited for the scanner");
        dropped
            .recv_timeout(Duration::from_secs(1))
            .expect("worker leaked after scan returned");
    }
}
