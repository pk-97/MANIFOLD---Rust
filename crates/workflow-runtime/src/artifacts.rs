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
    /// EXECUTE's output (D5/D5a): unique exact-match edits + new-file writes
    /// + a commit message.
    ChangeSet,
}

/// The empty-ChangeSet parse error, exported so the execute loop can treat it
/// as a NON-attempt: it must never overwrite a real error as feedback or park
/// reason (P3 shakedown, 2026-07-30).
pub const EMPTY_CHANGESET_ERR: &str = "ChangeSet has no edits and no writes";

/// Why an execute attempt failed. Typed, never sniffed out of an error string:
/// promotion to a lane costs real money, so a reworded message must not be
/// able to change which tier runs next.
///
/// The order IS the informativeness ranking — a red gate over a committed
/// change tells you far more than a `find` the model invented, so a later,
/// weaker failure never overwrites it in the park record (D16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// No model output at all. Costs nothing and says nothing about the work.
    Transport,
    /// Output did not parse as the artifact type.
    Parse,
    /// Parsed, but no edits and no writes.
    EmptyChangeSet,
    /// A `find` string is absent from the file, or matches more than once.
    FindMiss,
    /// A `write` targets a path that already exists, or a path is in both
    /// `edits` and `writes`.
    RejectedWrite,
    /// Applied and committed, then the gate went red.
    GateRed,
}

impl FailureKind {
    /// Substantive = the model's picture of the worktree is wrong, which a
    /// second stateless call with the error pasted in is bad at fixing. These
    /// promote on the FIRST one. Parse and transport failures are cheap and
    /// often self-correcting, so they retry one-shot (D20).
    pub fn is_substantive(self) -> bool {
        matches!(
            self,
            FailureKind::EmptyChangeSet | FailureKind::FindMiss | FailureKind::RejectedWrite | FailureKind::GateRed
        )
    }
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

/// One exact-match edit. `find` must occur exactly once in the file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edit {
    pub path: String,
    pub find: String,
    pub replace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeSet {
    #[serde(default)]
    pub edits: Vec<Edit>,
    #[serde(default)]
    pub writes: Vec<FileWrite>,
    /// Kept only so responses written against the old contract still parse.
    /// The runtime composes the message from the step and the program's task —
    /// a commit line is run metadata, not something the model gets to author.
    #[serde(default)]
    pub commit_message: Option<String>,
}

impl ChangeSet {
    /// The typed entry point the execute loop uses. `Artifact::parse` funnels
    /// here too, flattening to a string for every other caller.
    pub fn parse(raw: &str) -> Result<ChangeSet, (FailureKind, String)> {
        let body = strip_code_fences(raw);
        let v: ChangeSet = serde_json::from_str(body).map_err(|e| {
            (
                FailureKind::Parse,
                format!(
                    "output does not parse as ChangeSet {{edits: [{{path, find, replace}}], writes: [{{path, content}}]}}: {e}"
                ),
            )
        })?;
        if v.edits.is_empty() && v.writes.is_empty() {
            return Err((FailureKind::EmptyChangeSet, EMPTY_CHANGESET_ERR.to_string()));
        }
        Ok(v)
    }
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
                // Mirrors gate_runner's MIN_RATIONALE_CHARS: the why is the
                // record. Enforced here so the retry loop teaches the model,
                // not the recording step (D8).
                if v.rationale.trim().chars().count() < 20 {
                    return Err(
                        "rationale is too short (< 20 chars) — the why is the record; 'looks good' is not a why"
                            .to_string(),
                    );
                }
                serde_json::to_value(v).expect("Verdict serializes")
            }
            ArtifactKind::ChangeSet => {
                let v = ChangeSet::parse(raw).map_err(|(_, text)| text)?;
                serde_json::to_value(v).expect("ChangeSet serializes")
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
