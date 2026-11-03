//! Asking the person, not the agent: MCP elicitation.
//!
//! An agent can call any tool with any argument, so `mode="write"` is not
//! consent. Elicitation is a request from the server to the client, which
//! shows it to the user and sends back only their answer, so the agent never
//! sees the question. `HUMAN_APPROVAL=human` (the default) routes every
//! gated change through [`ask_human`]; the contradiction gate reuses it.

use std::time::Duration;

use rmcp::RoleServer;
use rmcp::model::{ElicitRequestParams, ElicitationAction, ElicitationSchema};
use rmcp::service::{ElicitationMode, Peer};
use serde_json::Value;
use tracing::warn;

/// How long a person gets to read a diff before the question is dropped.
pub const APPROVAL_TIMEOUT: Duration = Duration::from_mins(10);

/// What came of asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// The person said yes.
    Accepted,
    /// The person said no.
    Declined,
    /// The person dismissed the question, or it went unanswered (timed out,
    /// or the client dropped the request).
    Cancelled,
    /// The client never declared elicitation, so nobody could be asked.
    Unsupported,
}

impl Answer {
    /// The `status` a tool result carries when the answer wasn't yes.
    pub fn status(self) -> &'static str {
        match self {
            Self::Accepted => "approved",
            Self::Declined => "declined",
            Self::Cancelled => "cancelled",
            Self::Unsupported => "awaiting_approval",
        }
    }
}

/// Whether the connected client can be asked (it declared the `elicitation`
/// capability, with form mode or the older bare form of it).
pub fn can_ask(peer: &Peer<RoleServer>) -> bool {
    peer.supported_elicitation_modes()
        .contains(&ElicitationMode::Form)
}

/// Show `message` to the person behind the client and wait up to `timeout`
/// for accept or decline. Must run inside a tool call: protocol 2026-07-28
/// only allows a server to ask while it is answering a client request.
///
/// The form is one boolean, `approve`, defaulting to true, because clients
/// differ in how they draw forms but all draw accept and decline. Accept
/// with `approve` unticked counts as a no.
pub async fn ask_human(peer: &Peer<RoleServer>, message: &str, timeout: Duration) -> Answer {
    if !can_ask(peer) {
        return Answer::Unsupported;
    }
    let schema = match ElicitationSchema::builder()
        .optional_bool("approve", true)
        .build()
    {
        Ok(schema) => schema,
        Err(e) => {
            warn!(error = %e, "could not build the approval form");
            return Answer::Cancelled;
        }
    };
    let params = ElicitRequestParams::FormElicitationParams {
        meta: None,
        message: message.to_owned(),
        requested_schema: schema,
    };
    match peer
        .create_elicitation_with_timeout(params, Some(timeout))
        .await
    {
        Ok(reply) => match reply.action {
            ElicitationAction::Accept => {
                let ticked = reply
                    .content
                    .as_ref()
                    .and_then(|c| c.get("approve"))
                    .and_then(Value::as_bool);
                if ticked == Some(false) {
                    Answer::Declined
                } else {
                    Answer::Accepted
                }
            }
            ElicitationAction::Decline => Answer::Declined,
            _ => Answer::Cancelled,
        },
        Err(e) => {
            warn!(error = %e, "the approval request got no answer");
            Answer::Cancelled
        }
    }
}
