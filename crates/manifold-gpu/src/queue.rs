//! Machine-wide GPU queue: one GPU-using process at a time, across every agent
//! and worktree.
//!
//! Concurrent GPU processes give flaky black renders and AGX firmware faults,
//! so the first [`GpuDevice::new`](crate::GpuDevice::new) in a process takes an
//! flock on `~/.cache/manifold/gpu.lock` (override the directory with
//! `MANIFOLD_GPU_QUEUE_DIR`) and keeps it until the process exits. The kernel
//! drops it if the process dies. Every test, headless render, example and bin
//! that makes a device queues here without knowing about it.
//!
//! The live app is the one exemption ([`exempt_live_process`]): it must never
//! wait behind a test run, and a test run must never wait behind a show.
//!
//! `scripts/gpu_queue.py` speaks the same protocol (same lock file, same
//! `gpu.holder` record) and holds the lock across a whole multi-process run.
//! A process whose ancestor holds the lock does not wait for it: on contention
//! it checks whether the holder's pid is one of its own ancestors, so a gate
//! script that holds the lock and runs cargo does not deadlock its own tests.
//!
//! Obsolete when: GPU admission moves to a device-level scheduler that
//! replaces this file lock.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const POLL: Duration = Duration::from_millis(250);
const REPORT_EVERY: Duration = Duration::from_secs(30);

static LIVE_PROCESS: AtomicBool = AtomicBool::new(false);
static PROCESS_LOCK: OnceLock<Held> = OnceLock::new();

/// The live app calls this before it creates its first device. It then never
/// takes or waits for the queue.
pub fn exempt_live_process() {
    LIVE_PROCESS.store(true, Ordering::SeqCst);
}

/// Take the queue for the rest of this process. Idempotent; blocks while
/// another process holds the GPU. Called by `GpuDevice::new`.
pub fn acquire_for_process() {
    if LIVE_PROCESS.load(Ordering::SeqCst) {
        return;
    }
    PROCESS_LOCK.get_or_init(|| {
        let dir = queue_dir();
        acquire_in(&dir, &process_label(), POLL, REPORT_EVERY, &mut std::io::stderr())
            .unwrap_or_else(|e| {
                panic!(
                    "GPU queue: cannot take {}: {e}. Set MANIFOLD_GPU_QUEUE_DIR to a writable directory.",
                    dir.join("gpu.lock").display()
                )
            })
    });
}

pub fn queue_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("MANIFOLD_GPU_QUEUE_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").expect("GPU queue: HOME not set");
    PathBuf::from(home).join(".cache").join("manifold")
}

/// What `acquire_in` hands back. Dropping it releases the lock (a held file
/// closes); an `Inherited` value means an ancestor process holds it.
#[derive(Debug)]
pub enum Held {
    Owned { _file: File, holder_path: PathBuf },
    Inherited,
}

impl Held {
    pub fn is_inherited(&self) -> bool {
        matches!(self, Held::Inherited)
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Held::Owned { holder_path, .. } = self {
            // Clear the record before the fd closes so a waiter never reads
            // a record for a lock nobody holds.
            let _ = std::fs::remove_file(holder_path);
        }
    }
}

/// Block until the GPU is ours (or an ancestor's). `directory` is created.
pub fn acquire_in(
    directory: &Path,
    label: &str,
    poll: Duration,
    report_every: Duration,
    out: &mut dyn Write,
) -> std::io::Result<Held> {
    acquire_with(directory, label, poll, report_every, out, &ancestor_pids)
}

fn acquire_with(
    directory: &Path,
    label: &str,
    poll: Duration,
    report_every: Duration,
    out: &mut dyn Write,
    ancestors: &dyn Fn() -> Vec<u32>,
) -> std::io::Result<Held> {
    std::fs::create_dir_all(directory)?;
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(directory.join("gpu.lock"))?;
    let started = Instant::now();
    let mut last_report: Option<Instant> = None;
    let mut ancestor_cache: Option<Vec<u32>> = None;
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(e)) => return Err(e),
        }
        let info = read_holder(directory);
        let holder_pid = info.get("pid").and_then(|p| p.parse::<u32>().ok());
        let mine = ancestor_cache.get_or_insert_with(ancestors);
        if holder_pid.is_some_and(|p| mine.contains(&p)) {
            return Ok(Held::Inherited);
        }
        if last_report.is_none_or(|t| t.elapsed() >= report_every) {
            let verb = if last_report.is_none() {
                "waiting for the GPU"
            } else {
                "still waiting for the GPU"
            };
            let _ = writeln!(out, "[gpu-queue] {verb}: held by {}", describe(&info));
            last_report = Some(Instant::now());
        }
        std::thread::sleep(poll);
    }
    if last_report.is_some() {
        let _ = writeln!(
            out,
            "[gpu-queue] acquired after {}",
            format_age(started.elapsed().as_secs())
        );
    }
    let holder_path = directory.join("gpu.holder");
    write_holder(directory, &holder_path, label);
    Ok(Held::Owned { _file: file, holder_path })
}

fn process_label() -> String {
    let args: Vec<String> = std::env::args().collect();
    let exe = args
        .first()
        .map(|a| a.rsplit('/').next().unwrap_or(a).to_string())
        .unwrap_or_default();
    format!("{exe} {}", args[1.min(args.len())..].join(" "))
}

fn one_line(s: &str, max: usize) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(max).collect()
}

fn write_holder(directory: &Path, holder_path: &Path, label: &str) {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let cwd = std::env::current_dir()
        .map(|p| one_line(&p.display().to_string(), 300))
        .unwrap_or_default();
    let body = format!(
        "pid={}\nsince={since:.3}\nlabel={}\ncwd={cwd}\n",
        std::process::id(),
        one_line(label, 300)
    );
    // Best effort: the lock is what matters, the record only names the holder.
    let tmp = directory.join(format!("gpu.holder.tmp.{}", std::process::id()));
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, holder_path);
    }
}

fn read_holder(directory: &Path) -> std::collections::HashMap<String, String> {
    let mut info = std::collections::HashMap::new();
    if let Ok(text) = std::fs::read_to_string(directory.join("gpu.holder")) {
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                info.insert(k.to_string(), v.to_string());
            }
        }
    }
    info
}

fn describe(info: &std::collections::HashMap<String, String>) -> String {
    if info.is_empty() {
        return "an unknown holder".to_string();
    }
    let age = info
        .get("since")
        .and_then(|s| s.parse::<f64>().ok())
        .and_then(|since| {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs_f64();
            Some(format_age((now - since).max(0.0) as u64))
        })
        .unwrap_or_else(|| "unknown time".to_string());
    let get = |k: &str| info.get(k).map(String::as_str).unwrap_or("?");
    format!(
        "pid {} `{}` in {}, running for {age}",
        get("pid"),
        get("label"),
        get("cwd")
    )
}

fn format_age(seconds: u64) -> String {
    match seconds {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m{:02}s", s / 60, s % 60),
        s => format!("{}h{:02}m", s / 3600, s % 3600 / 60),
    }
}

/// Pids of this process's ancestors, nearest first; empty if `ps` fails.
fn ancestor_pids() -> Vec<u32> {
    let mut pids = Vec::new();
    let mut pid = std::process::id();
    for _ in 0..64 {
        let Ok(out) = std::process::Command::new("ps")
            .args(["-o", "ppid=", "-p", &pid.to_string()])
            .output()
        else {
            break;
        };
        let Some(parent) = String::from_utf8_lossy(&out.stdout).trim().parse::<u32>().ok() else {
            break;
        };
        if parent <= 1 {
            break;
        }
        pids.push(parent);
        pid = parent;
    }
    pids
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpu-queue-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    const FAST: Duration = Duration::from_millis(5);

    #[test]
    fn two_holders_serialize_and_waiter_names_the_holder() {
        let dir = temp_dir("serialize");
        let first = acquire_with(&dir, "first job", FAST, FAST, &mut std::io::sink(), &Vec::new)
            .unwrap();
        assert!(!first.is_inherited());

        let inside = Arc::new(AtomicUsize::new(0));
        let waiter = {
            let (dir, inside) = (dir.clone(), Arc::clone(&inside));
            std::thread::spawn(move || {
                let mut out: Vec<u8> = Vec::new();
                let held = acquire_with(&dir, "second job", FAST, FAST, &mut out, &Vec::new)
                    .unwrap();
                inside.fetch_add(1, Ordering::SeqCst);
                drop(held);
                String::from_utf8(out).unwrap()
            })
        };
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(inside.load(Ordering::SeqCst), 0, "waiter got in while the lock was held");
        drop(first);
        let message = waiter.join().unwrap();
        assert_eq!(inside.load(Ordering::SeqCst), 1);
        assert!(message.contains("waiting for the GPU"), "{message}");
        assert!(message.contains("first job"), "waiter must name the holder: {message}");
        assert!(message.contains(&format!("pid {}", std::process::id())), "{message}");
        assert!(message.contains("acquired after"), "{message}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ancestor_holder_is_inherited_not_waited_on() {
        let dir = temp_dir("inherit");
        let holder = acquire_with(&dir, "gate", FAST, FAST, &mut std::io::sink(), &Vec::new)
            .unwrap();
        // The record names this process; pretend this process is the "child"
        // by reporting it as its own ancestor.
        let me = std::process::id();
        let child = acquire_with(&dir, "cargo test", FAST, FAST, &mut std::io::sink(), &|| vec![me])
            .unwrap();
        assert!(child.is_inherited());
        drop(holder);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unrelated_holder_is_not_inherited() {
        let dir = temp_dir("unrelated");
        let holder = acquire_with(&dir, "someone else", FAST, FAST, &mut std::io::sink(), &Vec::new)
            .unwrap();
        let finished = Arc::new(AtomicBool::new(false));
        let waiter = {
            let (dir, finished) = (dir.clone(), Arc::clone(&finished));
            std::thread::spawn(move || {
                // Ancestors that do not include the holder's pid.
                let held = acquire_with(&dir, "x", FAST, FAST, &mut std::io::sink(), &|| vec![2])
                    .unwrap();
                finished.store(true, Ordering::SeqCst);
                assert!(!held.is_inherited());
            })
        };
        std::thread::sleep(Duration::from_millis(100));
        assert!(!finished.load(Ordering::SeqCst));
        drop(holder);
        waiter.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The Rust and Python halves must contend on the same lock: scripts hold
    /// it around cargo runs, tests take it in-process.
    #[test]
    fn rust_and_python_halves_exclude_each_other() {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/gpu_queue.py");
        let dir = temp_dir("interop");
        let run = |args: &[&str]| {
            std::process::Command::new(&script)
                .args(args)
                .env("MANIFOLD_GPU_QUEUE_DIR", &dir)
                .output()
                .expect("run scripts/gpu_queue.py")
        };

        // Rust holds: Python sees the GPU busy.
        let rust = acquire_with(&dir, "rust job", FAST, FAST, &mut std::io::sink(), &Vec::new)
            .unwrap();
        let status = run(&["status"]);
        assert_eq!(status.status.code(), Some(1), "python must see the Rust holder");
        assert!(String::from_utf8_lossy(&status.stdout).contains("rust job"));
        drop(rust);
        assert_eq!(run(&["status"]).status.code(), Some(0));

        // Python holds: Rust waits until the holder is killed.
        let mut python = std::process::Command::new(&script)
            .args(["--", "sleep", "3"])
            .env("MANIFOLD_GPU_QUEUE_DIR", &dir)
            // The orphaned `sleep` outlives the kill; keep it off the test's pipes.
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn python holder");
        let record = dir.join("gpu.holder");
        let started = Instant::now();
        while !record.exists() {
            assert!(started.elapsed() < Duration::from_secs(10), "python never took the lock");
            std::thread::sleep(Duration::from_millis(20));
        }
        let done = Arc::new(AtomicBool::new(false));
        let waiter = {
            let (dir, done) = (dir.clone(), Arc::clone(&done));
            std::thread::spawn(move || {
                let held = acquire_with(&dir, "rust job", FAST, FAST, &mut std::io::sink(), &|| {
                    Vec::new()
                })
                .unwrap();
                done.store(true, Ordering::SeqCst);
                drop(held);
            })
        };
        std::thread::sleep(Duration::from_millis(200));
        assert!(!done.load(Ordering::SeqCst), "Rust slipped past a Python holder");
        python.kill().unwrap(); // SIGKILL: the lock must free without cleanup code
        python.wait().unwrap();
        waiter.join().unwrap();
        assert!(done.load(Ordering::SeqCst));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn real_ancestor_lookup_finds_a_parent() {
        assert!(!ancestor_pids().is_empty(), "ps lookup returned no ancestors");
    }

    #[test]
    fn age_formats() {
        assert_eq!(format_age(5), "5s");
        assert_eq!(format_age(125), "2m05s");
        assert_eq!(format_age(3725), "1h02m");
    }
}
