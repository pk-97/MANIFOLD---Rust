//! Secrets guard: every assembled context is scanned BEFORE it ships to the
//! API (feedback loops included — a gate tail can leak a key too). A hit is a
//! loud run abort, never a silent redaction and never a park: a secret in
//! context is an authoring/environment bug a retry cannot fix.
//!
//! High-precision patterns only, by design: a false positive blocks a run, so
//! generic "password = ..." heuristics stay out.

use std::sync::LazyLock;

use regex::Regex;

/// (name, pattern). Word-boundary'd so "task-..." never trips "sk-".
const PATTERNS: &[(&str, &str)] = &[
    ("private-key block", r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
    ("openai/anthropic-style key", r"\bsk-[A-Za-z0-9_-]{24,}"),
    ("github token", r"\bgh[pousr]_[A-Za-z0-9]{30,}"),
    ("github fine-grained token", r"\bgithub_pat_[A-Za-z0-9_]{30,}"),
    ("aws access key id", r"\bAKIA[0-9A-Z]{16}\b"),
    ("slack token", r"\bxox[bpors]-[A-Za-z0-9-]{20,}"),
    ("google api key", r"\bAIza[A-Za-z0-9_-]{30,}"),
];

static COMPILED: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    PATTERNS
        .iter()
        .map(|(name, pattern)| (*name, Regex::new(pattern).expect("scrub pattern compiles")))
        .collect()
});

/// Err carries the pattern name and a MASKED excerpt — the secret itself must
/// never round-trip through logs or transcripts.
pub fn check(text: &str) -> Result<(), String> {
    for (name, regex) in COMPILED.iter() {
        if let Some(hit) = regex.find(text) {
            return Err(format!("{name} ({})", mask(hit.as_str())));
        }
    }
    Ok(())
}

/// First 8 chars + length — enough to find the source, never the secret.
fn mask(token: &str) -> String {
    let head: String = token.chars().take(8).collect();
    format!("{head}… {} chars", token.chars().count())
}

#[cfg(test)]
mod tests {
    use super::check;

    #[test]
    fn clean_text_passes() {
        check("total_tokens = 500000; task-list; risky business").unwrap();
        check("the sk- prefix alone, and AKIA too, are fine").unwrap();
    }

    #[test]
    fn keys_are_caught_and_masked() {
        let err = check("here: sk-abcdefghijklmnopqrstuvwxyz123456 done").unwrap_err();
        assert!(err.contains("sk-abcde…"), "{err}");
        assert!(!err.contains("z123456"), "must never echo the full secret: {err}");
        check("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef").unwrap_err();
        check("id AKIAIOSFODNN7EXAMPLE ").unwrap_err();
        check("-----BEGIN RSA PRIVATE KEY-----\nMII...").unwrap_err();
    }

    #[test]
    fn embedded_prefix_is_not_a_hit() {
        // "sk-" inside a longer word: not a standalone token.
        check("asterisk-abcdefghijklmnopqrstuvwxyz123456").unwrap();
    }
}
