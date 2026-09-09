//! Bounded persistent copy of the normal env_logger stream.

use std::io::{self, Write};
use std::sync::OnceLock;

const SESSION_LOGS_KEPT: usize = 20;

struct TeeWriter {
    file: std::fs::File,
}
static SESSION_FILE: OnceLock<std::fs::File> = OnceLock::new();
static SESSION_PATH: OnceLock<std::path::PathBuf> = OnceLock::new();

impl Write for TeeWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let file_result = self.file.write_all(bytes);
        if let Err(err) = &file_result {
            let _ = writeln!(
                io::stderr(),
                "MANIFOLD: session log file write failed: {err}"
            );
        }
        if let Err(err) = io::stderr().write_all(bytes) {
            let _ = writeln!(
                io::stderr(),
                "MANIFOLD: session log stderr mirror failed: {err}"
            );
        }
        file_result.map(|()| bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()?;
        if let Err(err) = io::stderr().flush() {
            let _ = writeln!(
                io::stderr(),
                "MANIFOLD: session log stderr flush failed: {err}"
            );
        }
        Ok(())
    }
}

fn log_dir() -> Option<std::path::PathBuf> {
    match std::env::var_os("HOME") {
        Some(home) => {
            Some(std::path::PathBuf::from(home).join("Library/Logs/com.latentspace.manifold"))
        }
        None => {
            let _ = writeln!(
                io::stderr(),
                "MANIFOLD: HOME is unset; persistent session logging unavailable"
            );
            None
        }
    }
}

fn prune(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("session-") && n.ends_with(".log"))
        })
        .collect();
    paths.sort();
    let excess = paths.len().saturating_sub(SESSION_LOGS_KEPT);
    for path in paths.into_iter().take(excess) {
        if let Err(err) = std::fs::remove_file(&path) {
            let _ = writeln!(
                io::stderr(),
                "MANIFOLD: cannot prune session log {path:?}: {err}"
            );
        }
    }
}

pub fn init() {
    let env = env_logger::Env::default().default_filter_or("info");
    let writer = log_dir().and_then(|dir| {
        std::fs::create_dir_all(&dir)
            .map_err(|e| {
                let _ = writeln!(
                    io::stderr(),
                    "MANIFOLD: cannot create session log directory {dir:?}: {e}"
                );
            })
            .ok()?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let path = dir.join(format!("session-{ts}-{}.log", std::process::id()));
        match std::fs::File::create(&path) {
            Ok(file) => {
                if let Ok(sync_file) = file.try_clone() {
                    let _ = SESSION_FILE.set(sync_file);
                } else {
                    let _ = writeln!(
                        io::stderr(),
                        "MANIFOLD: cannot retain session log handle for fatal-exit sync"
                    );
                }
                let _ = SESSION_PATH.set(path.clone());
                let result = Some((TeeWriter { file }, path));
                prune(&dir);
                result
            }
            Err(e) => {
                let _ = writeln!(
                    io::stderr(),
                    "MANIFOLD: cannot open session log {path:?}: {e}"
                );
                None
            }
        }
    });
    let mut builder = env_logger::Builder::from_env(env);
    if let Some((writer, path)) = writer {
        builder.target(env_logger::Target::Pipe(Box::new(writer)));
        builder.init();
        log::info!("persistent session log: {path:?}");
        return;
    }
    builder.init();
}

pub fn path() -> Option<&'static std::path::Path> {
    SESSION_PATH.get().map(std::path::PathBuf::as_path)
}

/// Flush and sync the persistent session stream before an intentional exit.
pub fn flush() {
    log::logger().flush();
    if let Some(file) = SESSION_FILE.get()
        && let Err(err) = file.sync_all()
    {
        let _ = writeln!(
            io::stderr(),
            "MANIFOLD: failed to sync session log before exit: {err}"
        );
    }
}

/// Bounded crash attachment. The full session remains alongside the report.
pub fn crash_tail() -> String {
    use std::io::{Read, Seek, SeekFrom};
    flush();
    let read = || -> io::Result<String> {
        let path = path().ok_or_else(|| io::Error::other("session path unavailable"))?;
        let mut file = std::fs::File::open(path)?;
        let total = file.metadata()?.len();
        const LIMIT: u64 = 512 * 1024;
        let offset = total.saturating_sub(LIMIT);
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::with_capacity(total.min(LIMIT) as usize);
        file.take(LIMIT).read_to_end(&mut bytes)?;
        Ok(format!("session_tail_offset={offset} total_bytes={total}\n{}",
            String::from_utf8_lossy(&bytes)))
    };
    read().unwrap_or_else(|err| format!("session_tail_unavailable={err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_preserves_crash_reports() {
        let dir =
            std::env::temp_dir().join(format!("manifold-session-rotation-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..25 {
            std::fs::write(dir.join(format!("session-{i:04}.log")), "log").unwrap();
        }
        std::fs::write(dir.join("crash-0001.log"), "crash").unwrap();
        prune(&dir);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 21);
        assert!(!dir.join("session-0004.log").exists());
        assert!(dir.join("session-0005.log").exists());
        assert!(dir.join("crash-0001.log").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn fatal_exit_helper() {
        if std::env::var_os("MANIFOLD_TEST_FATAL_LOG").is_none() {
            return;
        }
        init();
        log::info!("test project: Corrosion copy");
        log::error!("test warmup GPU hang");
        crate::abort_gpu_work("test submissions ignored");
    }

    #[test]
    fn fatal_exit_preserves_session_and_crash_report() {
        let dir = std::env::temp_dir().join(format!("manifold-fatal-audit-{}", std::process::id()));
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "session_log::tests::fatal_exit_helper",
                "--nocapture",
            ])
            .env("MANIFOLD_TEST_FATAL_LOG", "1")
            .env("HOME", &dir)
            .env("RUST_LOG", "info")
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(70), "{result:?}");
        let logs = dir.join("Library/Logs/com.latentspace.manifold");
        let entries: Vec<_> = std::fs::read_dir(logs)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        let session = entries
            .iter()
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("session-")
            })
            .unwrap();
        let crash = entries
            .iter()
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("crash-")
            })
            .unwrap();
        let session_text = std::fs::read_to_string(session).unwrap();
        assert!(session_text.contains("test project: Corrosion copy"));
        assert!(session_text.contains("test warmup GPU hang"));
        let crash_text = std::fs::read_to_string(crash).unwrap();
        assert!(crash_text.contains("exit_code=70"));
        assert!(crash_text.contains("test project: Corrosion copy"));
        assert!(crash_text.contains("test warmup GPU hang"));
        assert!(crash_text.contains("diagnostic_limitations="));
        assert!(crash_text.contains("test submissions ignored"));
        assert!(crash_text.contains(session.to_str().unwrap()));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn tee_writes_file() {
        let path =
            std::env::temp_dir().join(format!("manifold-session-test-{}", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        let mut tee = TeeWriter { file };
        tee.write_all(b"audit\n").unwrap();
        tee.flush().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "audit\n");
        let _ = std::fs::remove_file(path);
    }
}
