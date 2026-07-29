//! Typed artifacts (WORKFLOW_RUNTIME_DESIGN.md D2): serde IS the validator.
//! A parse failure is fed back to the model verbatim, up to the step's retry cap.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// Freeform prose — stored as-is, never parsed.
    #[default]
    Text,
    /// Arbitrary JSON value.
    Json,
    /// Review verdict (recorded through `gate_runner review`, D8).
    Verdict,
    /// EXECUTE's output: full-file writes + a commit message (D5). Applied in P2.
    FileWriteSet,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    /// "accept" | "reject" — anything else is a parse failure.
    pub verdict: String,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileWrite {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileWriteSet {
    pub writes: Vec<FileWrite>,
    pub commit_message: String,
}

/// A completed step's stored artifact: `{ "kind": ..., "value": ... }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub kind: ArtifactKind,
    pub value: serde_json::Value,
}

impl Artifact {
    /// Parse raw model output into a typed artifact. Errors carry the serde
    /// message — it goes back to the model as the retry context.
    pub fn parse(kind: ArtifactKind, raw: &str) -> Result<Artifact, String> {
        let body = strip_code_fences(raw);
        let value = match kind {
            ArtifactKind::Text => serde_json::Value::String(raw.to_string()),
            ArtifactKind::Json => serde_json::from_str::<serde_json::Value>(body)
                .map_err(|e| format!("output is not valid JSON: {e}"))?,
            ArtifactKind::Verdict => {
                let v: Verdict = serde_json::from_str(body)
                    .map_err(|e| format!("output does not parse as Verdict {{verdict, rationale}}: {e}"))?;
                if v.verdict != "accept" && v.verdict != "reject" {
                    return Err(format!("verdict must be \"accept\" or \"reject\", got {:?}", v.verdict));
                }
                serde_json::to_value(v).expect("Verdict serializes")
            }
            ArtifactKind::FileWriteSet => {
                let v: FileWriteSet = serde_json::from_str(body).map_err(|e| {
                    format!("output does not parse as FileWriteSet {{writes: [{{path, content}}], commit_message}}: {e}")
                })?;
                if v.writes.is_empty() {
                    return Err("FileWriteSet.writes is empty".to_string());
                }
                serde_json::to_value(v).expect("FileWriteSet serializes")
            }
        };
        Ok(Artifact { kind, value })
    }

    /// Rendering used when this artifact is an input to a later step's template.
    pub fn render(&self) -> String {
        match &self.value {
            serde_json::Value::String(s) => s.clone(),
            other => serde_json::to_string_pretty(other).expect("artifact value serializes"),
        }
    }
}

/// Models wrap JSON in markdown fences; tolerate exactly that, nothing fancier.
fn strip_code_fences(raw: &str) -> &str {
    let t = raw.trim();
    let Some(rest) = t.strip_prefix("```") else {
        return t;
    };
    let rest = rest.strip_prefix("json").unwrap_or(rest);
    rest.trim_start_matches(['\r', '\n'])
        .strip_suffix("```")
        .unwrap_or(rest)
        .trim()
}
