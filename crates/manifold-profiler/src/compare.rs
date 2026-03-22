//! Session comparison — diff two profiling sessions for before/after analysis.

use serde::Serialize;
use std::path::Path;

use crate::SessionSummary;

/// Result of comparing two profiling sessions.
#[derive(Debug, Clone, Serialize)]
pub struct SessionComparison {
    pub baseline_session: String,
    pub current_session: String,
    pub overall_delta_ms: f64,
    pub overall_delta_pct: f64,
    pub budget_improvement_pct: f64,
    pub per_pass_deltas: Vec<PassDelta>,
    pub new_passes: Vec<String>,
    pub removed_passes: Vec<String>,
    pub recommendations: Vec<String>,
}

/// Per-pass before/after delta.
#[derive(Debug, Clone, Serialize)]
pub struct PassDelta {
    pub name: String,
    pub before_mean_ms: f64,
    pub after_mean_ms: f64,
    pub delta_ms: f64,
    pub delta_pct: f64,
}

/// Compare two profiling sessions. Reads summary.json from each directory.
pub fn compare_sessions(dir_a: &Path, dir_b: &Path) -> Result<SessionComparison, String> {
    let summary_a = load_summary(dir_a)?;
    let summary_b = load_summary(dir_b)?;

    let overall_delta = summary_b.mean_frame_ms - summary_a.mean_frame_ms;
    let overall_delta_pct = if summary_a.mean_frame_ms > 0.0 {
        overall_delta / summary_a.mean_frame_ms * 100.0
    } else {
        0.0
    };

    // Per-pass comparison
    let mut per_pass_deltas = Vec::new();
    let mut new_passes = Vec::new();
    let mut removed_passes = Vec::new();

    // Build lookup for session B
    let b_by_name: std::collections::HashMap<&str, f64> = summary_b.gpu_pass_aggregates
        .iter()
        .map(|p| (p.name.as_str(), p.mean_ms))
        .collect();

    let a_by_name: std::collections::HashMap<&str, f64> = summary_a.gpu_pass_aggregates
        .iter()
        .map(|p| (p.name.as_str(), p.mean_ms))
        .collect();

    // Compare passes in A
    for pass in &summary_a.gpu_pass_aggregates {
        if let Some(&after) = b_by_name.get(pass.name.as_str()) {
            let delta = after - pass.mean_ms;
            let pct = if pass.mean_ms > 0.0 { delta / pass.mean_ms * 100.0 } else { 0.0 };
            per_pass_deltas.push(PassDelta {
                name: pass.name.clone(),
                before_mean_ms: pass.mean_ms,
                after_mean_ms: after,
                delta_ms: delta,
                delta_pct: pct,
            });
        } else {
            removed_passes.push(pass.name.clone());
        }
    }

    // Find new passes in B
    for pass in &summary_b.gpu_pass_aggregates {
        if !a_by_name.contains_key(pass.name.as_str()) {
            new_passes.push(pass.name.clone());
        }
    }

    // Sort deltas by absolute delta descending
    per_pass_deltas.sort_by(|a, b| {
        b.delta_ms.abs().partial_cmp(&a.delta_ms.abs()).unwrap_or(std::cmp::Ordering::Equal)
    });

    // Budget improvement
    let budget_improvement = if summary_a.mean_frame_ms > 0.0 {
        (1.0 - summary_b.mean_frame_ms / summary_a.mean_frame_ms) * 100.0
    } else {
        0.0
    };

    // Generate recommendations
    let mut recs = Vec::new();
    if overall_delta < -0.5 {
        recs.push(format!(
            "Frame time improved by {:.1}ms ({:.1}% faster).",
            -overall_delta, -overall_delta_pct
        ));
    } else if overall_delta > 0.5 {
        recs.push(format!(
            "Frame time regressed by {:.1}ms ({:.1}% slower).",
            overall_delta, overall_delta_pct
        ));
    }
    for delta in per_pass_deltas.iter().take(3) {
        if delta.delta_ms.abs() > 0.1 {
            let direction = if delta.delta_ms < 0.0 { "improved" } else { "regressed" };
            recs.push(format!(
                "{}: {} by {:.2}ms ({:.1}%)",
                delta.name, direction, delta.delta_ms.abs(), delta.delta_pct.abs()
            ));
        }
    }

    Ok(SessionComparison {
        baseline_session: dir_a.display().to_string(),
        current_session: dir_b.display().to_string(),
        overall_delta_ms: overall_delta,
        overall_delta_pct,
        budget_improvement_pct: budget_improvement,
        per_pass_deltas,
        new_passes,
        removed_passes,
        recommendations: recs,
    })
}

/// Load summary.json from a session directory.
fn load_summary(dir: &Path) -> Result<SessionSummary, String> {
    let path = dir.join("summary.json");
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse {}: {}", path.display(), e))
}
