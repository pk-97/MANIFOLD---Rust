//! The model seam (WORKFLOW_RUNTIME_DESIGN.md section 3, Design body).
//! `LiveTransport` is `.claude/hooks/oneshot`'s pattern in-process (D4):
//! blocking POST to the litellm proxy, key from `cc-fleet keyget` (never
//! env/argv), reasoning-exhaustion budget doubling, deepseek→kimi fallback.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct CompletionRequest {
    pub model: String,
    pub max_tokens: u32,
    pub system: Option<String>,
    pub user: String,
}

#[derive(Debug, Clone)]
pub struct CompletionResponse {
    pub content: String,
    /// Usage of the FINAL response (kept for artifact context).
    pub usage: serde_json::Value,
    /// One record per HTTP post, internal retries and fallbacks included:
    /// {"model": ..., "usage": ...} or {"model": ..., "error": ...}.
    /// The budget sums these; the transcript logs them (finding 3 of the
    /// 2026-07-29 adversarial review: hidden retries must not be free).
    pub attempts: Vec<serde_json::Value>,
}

#[derive(Debug)]
pub struct TransportError(pub String);

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Sync: parallel generate steps share one transport across threads.
pub trait ModelTransport: Sync {
    fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, TransportError>;
}

/// Canned responses, consumed in order. Exhaustion is an error — a resumed run
/// that re-requests a completed step MUST fail loudly, not silently re-answer.
/// Keyed responses (`keyed`) match on a needle in the request text — the only
/// deterministic mode under parallel steps, where pop order is racy.
pub struct MockTransport {
    responses: Mutex<VecDeque<String>>,
    keyed: Vec<(String, String)>,
    requests_served: Mutex<u32>,
    tokens_per_response: u64,
}

impl MockTransport {
    pub fn new(responses: Vec<String>) -> Self {
        Self::with_tokens_per_response(responses, 0)
    }

    /// Each canned response reports this `total_tokens` — for budget tests.
    pub fn with_tokens_per_response(responses: Vec<String>, total_tokens: u64) -> Self {
        MockTransport {
            responses: Mutex::new(responses.into()),
            keyed: Vec::new(),
            requests_served: Mutex::new(0),
            tokens_per_response: total_tokens,
        }
    }

    /// (needle, response) pairs: the FIRST pair whose needle occurs in the
    /// request's user text answers it. No match is a loud error.
    pub fn keyed(pairs: Vec<(String, String)>) -> Self {
        MockTransport {
            responses: Mutex::new(VecDeque::new()),
            keyed: pairs,
            requests_served: Mutex::new(0),
            tokens_per_response: 0,
        }
    }

    pub fn requests_served(&self) -> u32 {
        *self.requests_served.lock().expect("mock lock")
    }
}

impl ModelTransport for MockTransport {
    fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, TransportError> {
        let content = if self.keyed.is_empty() {
            match self.responses.lock().expect("mock lock").pop_front() {
                Some(c) => c,
                None => return Err(TransportError("mock transport exhausted".to_string())),
            }
        } else {
            match self.keyed.iter().find(|(needle, _)| req.user.contains(needle)) {
                Some((_, response)) => response.clone(),
                None => {
                    return Err(TransportError(format!(
                        "keyed mock: no needle matches request starting {:?}",
                        req.user.chars().take(60).collect::<String>()
                    )));
                }
            }
        };
        *self.requests_served.lock().expect("mock lock") += 1;
        let usage = serde_json::json!({ "total_tokens": self.tokens_per_response });
        Ok(CompletionResponse {
            content,
            attempts: vec![serde_json::json!({"model": "mock", "usage": usage})],
            usage,
        })
    }
}

const PROXY: &str = "http://127.0.0.1:4000/v1/chat/completions";
const BUDGET_CAP: u32 = 32768;
const FALLBACK_MODEL: &str = "kimi-for-coding";

pub struct LiveTransport {
    key: String,
    agent: ureq::Agent,
}

impl LiveTransport {
    /// Key read subprocess-side (the crate's one non-gate, non-worktree spawn).
    pub fn new() -> Result<LiveTransport, TransportError> {
        let out = std::process::Command::new("cc-fleet")
            .args(["keyget", "kimi"])
            .output()
            .map_err(|e| TransportError(format!("cc-fleet keyget spawn failed: {e}")))?;
        if !out.status.success() {
            return Err(TransportError("cc-fleet keyget failed".to_string()));
        }
        Ok(LiveTransport {
            key: String::from_utf8_lossy(&out.stdout).trim().to_string(),
            agent: ureq::AgentBuilder::new().timeout(Duration::from_secs(600)).build(),
        })
    }

    fn post(&self, model: &str, max_tokens: u32, req: &CompletionRequest) -> Result<serde_json::Value, TransportError> {
        let mut messages = Vec::new();
        if let Some(sys) = &req.system {
            messages.push(serde_json::json!({"role": "system", "content": sys}));
        }
        messages.push(serde_json::json!({"role": "user", "content": req.user}));
        let body = serde_json::json!({"model": model, "max_tokens": max_tokens, "messages": messages});
        self.agent
            .post(PROXY)
            .set("Authorization", &format!("Bearer {}", self.key))
            .set("Content-Type", "application/json")
            .send_string(&body.to_string())
            .map_err(|e| TransportError(format!("proxy request failed: {e}")))?
            .into_json::<serde_json::Value>()
            .map_err(|e| TransportError(format!("proxy response is not JSON: {e}")))
    }
}

impl ModelTransport for LiveTransport {
    fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, TransportError> {
        let mut model = req.model.clone();
        let mut budget = req.max_tokens;
        let mut attempts: Vec<serde_json::Value> = Vec::new();
        loop {
            let resp = match self.post(&model, budget, req) {
                Ok(v) => v,
                // Quota/transport failure on the default seat → one hop to the
                // kimi fast tier (oneshot's verified fallback), then surface.
                Err(e) if model == "deepseek-v4-flash" => {
                    attempts.push(serde_json::json!({"model": model, "error": e.to_string()}));
                    eprintln!("workflow: {model} failed ({e}) — falling back to {FALLBACK_MODEL}");
                    model = FALLBACK_MODEL.to_string();
                    continue;
                }
                Err(e) => return Err(e),
            };
            let content = resp["choices"][0]["message"]["content"].as_str().unwrap_or("").trim().to_string();
            let finish = resp["choices"][0]["finish_reason"].as_str().unwrap_or("").to_string();
            let usage = resp["usage"].clone();
            attempts.push(serde_json::json!({"model": model, "usage": usage}));
            // Budget exhaustion — empty OR truncated mid-output (the D-54
            // reasoning wall truncates non-empty JSON too) — double and retry.
            if (content.is_empty() || finish == "length") && budget < BUDGET_CAP {
                budget = (budget * 2).min(BUDGET_CAP);
                eprintln!("workflow: budget exhausted (finish={finish}, {} chars) — retrying at {budget}", content.len());
                continue;
            }
            if !content.is_empty() {
                return Ok(CompletionResponse { content, usage, attempts });
            }
            return Err(TransportError(format!("empty content from {model} (usage: {usage})")));
        }
    }
}
