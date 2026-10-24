//! GPU resets recorded by macOS (BUG-k1n2w). When the GPU firmware resets the
//! GPU, macOS writes `gpuEvent-<process>-<time>.ips` to the system
//! DiagnosticReports folder. It is the only record of why a "GPU Hang Error"
//! happened, the Metal error itself carries no reason, and later resets in
//! the same boot may write no report at all. A "CDM Kill timeout" reset means
//! the firmware could not stop compute work in time; after one, more hangs in
//! the same boot are likely whatever the scene, so hosts attach the reason to
//! fatal reports and warn before long exports.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SYSTEM_REPORTS: &str = "/Library/Logs/DiagnosticReports";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirmwareGpuReset {
    pub report: PathBuf,
    pub process: String,
    /// `restart_reason_desc`, or `restart_reason <n>` when the text is absent.
    pub reason: String,
    pub at: SystemTime,
}

/// When this machine last booted.
pub fn boot_time() -> Option<SystemTime> {
    let mut boot = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut size = std::mem::size_of::<libc::timeval>();
    let name = c"kern.boottime";
    let status = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&mut boot as *mut libc::timeval).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (status == 0 && boot.tv_sec > 0)
        .then(|| UNIX_EPOCH + Duration::new(boot.tv_sec as u64, boot.tv_usec as u32 * 1000))
}

/// Every firmware GPU reset since boot, any process, oldest first.
pub fn resets_since_boot() -> Vec<FirmwareGpuReset> {
    boot_time().map_or_else(Vec::new, |boot| {
        resets_since(Path::new(SYSTEM_REPORTS), boot)
    })
}

/// One line for logs and crash reports; `None` when the boot is clean.
pub fn summary_since_boot() -> Option<String> {
    let resets = resets_since_boot();
    let latest = resets.last()?;
    Some(format!(
        "macOS reset the GPU {} time(s) since boot; latest: process={} reason=\"{}\" report={}",
        resets.len(),
        latest.process,
        latest.reason,
        latest.report.display(),
    ))
}

fn resets_since(dir: &Path, since: SystemTime) -> Vec<FirmwareGpuReset> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut resets: Vec<FirmwareGpuReset> = entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("gpuEvent-") && name.ends_with(".ips")
        })
        .filter_map(|entry| {
            let at = entry.metadata().ok()?.modified().ok()?;
            if at < since {
                return None;
            }
            let text = std::fs::read_to_string(entry.path()).ok()?;
            let (process, reason) = parse_report(&text)?;
            Some(FirmwareGpuReset {
                report: entry.path(),
                process,
                reason,
                at,
            })
        })
        .collect();
    resets.sort_by_key(|reset| reset.at);
    resets
}

fn parse_report(text: &str) -> Option<(String, String)> {
    let process = string_field(text, "process_name")?;
    let reason = string_field(text, "restart_reason_desc")
        .or_else(|| number_field(text, "restart_reason").map(|n| format!("restart_reason {n}")))
        .unwrap_or_else(|| "unknown".into());
    Some((process, reason))
}

/// The value after `"key"`, a colon and optional spaces. The report is
/// pretty-printed JSON in some sections and compact in others.
fn value_after<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let pattern = format!("\"{key}\"");
    let rest = &text[text.find(&pattern)? + pattern.len()..];
    rest.trim_start().strip_prefix(':').map(str::trim_start)
}

fn string_field(text: &str, key: &str) -> Option<String> {
    let rest = value_after(text, key)?.strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_string())
}

fn number_field(text: &str, key: &str) -> Option<u64> {
    let rest = value_after(text, key)?;
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The 2026-10-13 14:02 Murmur report, abridged; both JSON spacings occur.
    const REPORT: &str = r#"{"bug_type":"284","timestamp":"2026-10-13 14:02:56.00 +1100"}
{
  "bug_type" : "284",
  "process_name" : "manifold",
  "analysis" : {"fw_power_state":0,"guilty_dm":3,"restart_reason":9,"restart_reason_desc":"CDM Kill timeout"}
}"#;

    #[test]
    fn reads_process_and_reason_from_a_real_report() {
        assert_eq!(
            parse_report(REPORT),
            Some(("manifold".into(), "CDM Kill timeout".into()))
        );
    }

    #[test]
    fn falls_back_to_the_numeric_reason() {
        let text = REPORT.replace(r#","restart_reason_desc":"CDM Kill timeout""#, "");
        assert_eq!(
            parse_report(&text),
            Some(("manifold".into(), "restart_reason 9".into()))
        );
    }

    #[test]
    fn rejects_reports_without_a_process() {
        assert_eq!(
            parse_report(r#"{"restart_reason_desc":"CDM Kill timeout"}"#),
            None
        );
    }

    #[test]
    fn scans_only_gpu_event_reports_since_the_cutoff() {
        let dir = std::env::temp_dir().join(format!("firmware-reports-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("gpuEvent-manifold-2026-10-13-140256.ips"), REPORT).unwrap();
        std::fs::write(dir.join("manifold-2026-10-13-140256.ips"), REPORT).unwrap();
        let found = resets_since(&dir, UNIX_EPOCH);
        let later = resets_since(&dir, SystemTime::now() + Duration::from_secs(60));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].reason, "CDM Kill timeout");
        assert!(later.is_empty());
    }

    #[test]
    fn boot_time_is_in_the_past() {
        let boot = boot_time().expect("kern.boottime");
        assert!(boot < SystemTime::now());
    }
}
