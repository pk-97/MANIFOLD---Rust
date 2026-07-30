//! `workflow cost` — the per-run ledger, aggregated from `transcript.jsonl`
//! (the transcript is the record; this only sums it). Tokens count EVERY HTTP
//! post the transport made — internal retries and fallbacks included.

use std::collections::BTreeMap;
use std::path::Path;

/// One step's spend. Dollars sit beside tokens, never inside them: a lane's
/// envelope reports USD and the two units do not convert.
#[derive(Debug, Default, Clone, Copy)]
pub struct StepSpend {
    pub requests: u64,
    pub tokens: u64,
    pub usd: f64,
}

/// The parsed ledger. `summarize` is one formatter over this; `workflow watch`
/// is another — the transcript is parsed in exactly one place.
#[derive(Debug, Default)]
pub struct Ledger {
    pub by_step: BTreeMap<String, StepSpend>,
    /// model actually hit → tokens
    pub by_model: BTreeMap<String, u64>,
    pub total: u64,
    /// Lane spend across the whole run, in dollars.
    pub usd_total: f64,
}

pub fn ledger(run_dir: &Path) -> Result<Ledger, String> {
    let path = run_dir.join("transcript.jsonl");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("no transcript at {}: {e}", path.display()))?;
    let Ledger { mut by_step, mut by_model, mut total, mut usd_total } = Ledger::default();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue; // a torn trailing line is tolerated everywhere else too
        };
        let step = v["step"].as_str().unwrap_or("?").to_string();
        let entry = by_step.entry(step).or_default();
        entry.requests += 1;
        // Lane lines carry dollars and no token usage at all.
        let usd = v["response"]["total_cost_usd"].as_f64().unwrap_or(0.0);
        entry.usd += usd;
        usd_total += usd;
        let attempts = v["response"]["attempts"].as_array().cloned().unwrap_or_default();
        if attempts.is_empty() {
            let tokens = v["response"]["usage"]["total_tokens"].as_u64().unwrap_or(0);
            entry.tokens += tokens;
            total += tokens;
            *by_model.entry(v["request"]["model"].as_str().unwrap_or("?").to_string()).or_default() +=
                tokens;
        } else {
            for a in &attempts {
                let tokens = a["usage"]["total_tokens"].as_u64().unwrap_or(0);
                entry.tokens += tokens;
                total += tokens;
                *by_model.entry(a["model"].as_str().unwrap_or("?").to_string()).or_default() += tokens;
            }
        }
    }
    Ok(Ledger { by_step, by_model, total, usd_total })
}

pub fn summarize(run_dir: &Path) -> Result<String, String> {
    let l = ledger(run_dir)?;
    let mut out = String::from("step                              requests    tokens       usd\n");
    for (step, s) in &l.by_step {
        out.push_str(&format!("{step:<34}{:>8}{:>10}{:>10.4}\n", s.requests, s.tokens, s.usd));
    }
    out.push_str("\nmodel                                       tokens\n");
    for (model, tokens) in &l.by_model {
        out.push_str(&format!("{model:<42}{tokens:>10}\n"));
    }
    out.push_str(&format!("\nTOTAL {} tokens\n", l.total));
    if l.usd_total > 0.0 {
        out.push_str(&format!("TOTAL ${:.4} lane spend\n", l.usd_total));
    }
    Ok(out)
}
