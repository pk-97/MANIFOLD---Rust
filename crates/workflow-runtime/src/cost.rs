//! `workflow cost` — the per-run ledger, aggregated from `transcript.jsonl`
//! (the transcript is the record; this only sums it). Tokens count EVERY HTTP
//! post the transport made — internal retries and fallbacks included.

use std::collections::BTreeMap;
use std::path::Path;

pub fn summarize(run_dir: &Path) -> Result<String, String> {
    let path = run_dir.join("transcript.jsonl");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("no transcript at {}: {e}", path.display()))?;
    // (requests, tokens) keyed by step label / by model actually hit.
    let mut by_step: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut by_model: BTreeMap<String, u64> = BTreeMap::new();
    let mut total: u64 = 0;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue; // a torn trailing line is tolerated everywhere else too
        };
        let step = v["step"].as_str().unwrap_or("?").to_string();
        let entry = by_step.entry(step).or_default();
        entry.0 += 1;
        let attempts = v["response"]["attempts"].as_array().cloned().unwrap_or_default();
        if attempts.is_empty() {
            let tokens = v["response"]["usage"]["total_tokens"].as_u64().unwrap_or(0);
            entry.1 += tokens;
            total += tokens;
            *by_model.entry(v["request"]["model"].as_str().unwrap_or("?").to_string()).or_default() +=
                tokens;
        } else {
            for a in &attempts {
                let tokens = a["usage"]["total_tokens"].as_u64().unwrap_or(0);
                entry.1 += tokens;
                total += tokens;
                *by_model.entry(a["model"].as_str().unwrap_or("?").to_string()).or_default() += tokens;
            }
        }
    }
    let mut out = String::from("step                              requests    tokens\n");
    for (step, (requests, tokens)) in &by_step {
        out.push_str(&format!("{step:<34}{requests:>8}{tokens:>10}\n"));
    }
    out.push_str("\nmodel                                       tokens\n");
    for (model, tokens) in &by_model {
        out.push_str(&format!("{model:<42}{tokens:>10}\n"));
    }
    out.push_str(&format!("\nTOTAL {total} tokens\n"));
    Ok(out)
}
