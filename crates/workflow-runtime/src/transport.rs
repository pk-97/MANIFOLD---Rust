//! The model seam (WORKFLOW_RUNTIME_DESIGN.md section 3, Design body). Unit tests and the
//! P1 CLI use `MockTransport`; the live proxy transport lands in P2 (D4).

use std::cell::RefCell;
use std::collections::VecDeque;

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
    /// Raw usage object from the provider, for the transcript. Empty for mocks.
    pub usage: serde_json::Value,
}

#[derive(Debug)]
pub struct TransportError(pub String);

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

pub trait ModelTransport {
    fn complete(&self, req: &CompletionRequest) -> Result<CompletionResponse, TransportError>;
}

/// Canned responses, consumed in order. Exhaustion is an error — a resumed run
/// that re-requests a completed step MUST fail loudly, not silently re-answer.
pub struct MockTransport {
    responses: RefCell<VecDeque<String>>,
    pub requests_served: RefCell<u32>,
}

impl MockTransport {
    pub fn new(responses: Vec<String>) -> Self {
        MockTransport {
            responses: RefCell::new(responses.into()),
            requests_served: RefCell::new(0),
        }
    }
}

impl ModelTransport for MockTransport {
    fn complete(&self, _req: &CompletionRequest) -> Result<CompletionResponse, TransportError> {
        let Some(content) = self.responses.borrow_mut().pop_front() else {
            return Err(TransportError("mock transport exhausted".to_string()));
        };
        *self.requests_served.borrow_mut() += 1;
        Ok(CompletionResponse {
            content,
            usage: serde_json::Value::Null,
        })
    }
}
