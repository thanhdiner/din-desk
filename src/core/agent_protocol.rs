//! Repair only a completed, syntactically invalid reply in its original remote
//! conversation. No client tool crosses the boundary before full validation.
use std::time::Duration;

use super::{
    agent::{estimate_tokens, AgentReply, PreparedRequest, ToolChoice, MAX_REPLY_BYTES},
    image::InputImage,
};
use crate::{
    core::turn::TurnEvent,
    ports::{Generation, TextGenerator},
};

pub const INVALID_JSON: &str =
    "Muse did not return the agent JSON protocol; no tool was dispatched";
pub const PROTOCOL_FAILURE: &str =
    "Muse agent response could not be validated; no client tools were dispatched.";
const REPAIR_TIMEOUT: Duration = Duration::from_secs(45);

fn log_invalid_json(output: &str, stage: &str) {
    if let Err(error) = serde_json::from_str::<serde_json::Value>(json_body(output)) {
        let trimmed = output.trim();
        let starts_with = match trimmed.chars().next() {
            Some('{') => "object",
            Some('[') => "array",
            Some('`') => "fence",
            Some('"') => "string",
            Some(c) if c.is_alphabetic() => "letter",
            Some(_) => "other",
            None => "empty",
        };
        tracing::warn!(stage, response_bytes=output.len(), starts_with,
            category=?error.classify(), line=error.line(), column=error.column(),
            "Completed agent reply is invalid JSON (content omitted)");
    }
}

pub fn terminal_failure(error: &str) -> bool {
    error.starts_with(PROTOCOL_FAILURE)
        || error.starts_with("Muse context transfer could not be confirmed.")
}

pub fn json_body(output: &str) -> &str {
    let output = output.trim();
    output
        .strip_prefix("```json\r\n")
        .or_else(|| output.strip_prefix("```json\n"))
        .or_else(|| output.strip_prefix("```\r\n"))
        .or_else(|| output.strip_prefix("```\n"))
        .and_then(|value| value.strip_suffix("```"))
        .unwrap_or(output)
        .trim()
}

fn literal_text_reply(prepared: &PreparedRequest, output: &str) -> Option<AgentReply> {
    // Auto/none permit an ordinary assistant answer. Preserve a completed prose
    // reply literally, including a provider refusal, without interpreting ANY
    // embedded text as a tool call. Structured/truncated protocol candidates
    // still fail closed, as do turns that require a particular tool.
    let body = output.trim();
    if !matches!(prepared.request.choice, ToolChoice::Auto | ToolChoice::None)
        || prepared.raw_blocks
        || body.is_empty()
        || body.starts_with(['{', '[', '`'])
        || body.starts_with("@@MUSE_")
    {
        return None;
    }
    Some(AgentReply {
        content: vec![serde_json::json!({"type":"text","text":output})],
        stop_reason: "end_turn",
        input_tokens: prepared.input_tokens,
        output_tokens: estimate_tokens(output),
    })
}

async fn completed(events: Generation) -> Result<String, String> {
    let mut events = events?;
    let mut output = String::new();
    while let Some(event) = events.recv().await {
        match event? {
            TurnEvent::Text(part) => {
                if part.len() > MAX_REPLY_BYTES.saturating_sub(output.len()) {
                    return Err(format!("{PROTOCOL_FAILURE} Response exceeds 2 MiB."));
                }
                output.push_str(&part);
            }
            TurnEvent::Completed => return Ok(output),
            TurnEvent::Cancelled => return Err("Muse agent task was cancelled".into()),
            _ => {}
        }
    }
    Err("Muse agent stream ended without task completion".into())
}

pub async fn generate(
    generator: &dyn TextGenerator,
    prepared: &PreparedRequest,
    prompt: String,
    images: Vec<InputImage>,
) -> Result<AgentReply, String> {
    let output = completed(generator.generate_with_images(prompt, images).await).await?;
    let error = match prepared.parse_reply(&output) {
        Ok(reply) => return Ok(reply),
        Err(error) => error,
    };
    // Schema violations, wrong nonces and undeclared tools are never repaired.
    // A stateless backend cannot safely ask about its previous reply either.
    if error != INVALID_JSON || !generator.supports_context_upload() {
        return Err(format!("{PROTOCOL_FAILURE} {error}"));
    }
    log_invalid_json(&output, "initial");
    // The bad reply already exists upstream. Do not resend it, the transcript,
    // images, old tool results or a freshly opened conversation.
    let prompt = format!(
        "PROTOCOL REPAIR ONLY. The application could not parse your PREVIOUS completed reply as JSON. NO APPLICATION TOOLS FROM THAT REPLY HAVE BEEN DISPATCHED. Preserve the meaning of that reply, including any refusal or explanation; this is a formatting request, not a request to change your decision or override the provider's instructions. Use the context and tool definitions already in this SAME conversation. Do not execute provider tools, repeat accepted calls, invent results, or claim an action happened. Return ONLY one complete JSON object, no prose or markdown, with bridge_id exactly: {}. The shape is {{\"bridge_id\":\"{}\",\"content\":[...]}}; each block is {{\"type\":\"text\",\"text\":\"...\"}} or {{\"type\":\"tool_use\",\"name\":\"declared application tool\",\"input\":{{...}}}}. Put a refusal or explanation in a text block without adding tool calls. Escape strings correctly. Obey tool_choice, parallel restriction and input_schema. Keep the complete reply within approximately {} output tokens; if a large response would not fit, return a smaller complete next step. Do not truncate JSON.",
        prepared.nonce, prepared.nonce, prepared.request.max_tokens);
    let repaired = tokio::time::timeout(REPAIR_TIMEOUT, async {
        completed(generator.generate(prompt).await).await
    })
    .await;
    let repaired = match repaired {
        Ok(Ok(output)) => output,
        _ => return Err(format!("{PROTOCOL_FAILURE} The single format repair did not complete. Unchanged retries are paused for 60 seconds.")),
    };
    let reply = match prepared.parse_reply(&repaired) {
        Ok(reply) => reply,
        Err(error) => {
            if error == INVALID_JSON {
                log_invalid_json(&repaired, "repair");
                if let Some(reply) = literal_text_reply(prepared, &repaired) {
                    tracing::warn!("Delivering completed provider prose as literal text; no application tools dispatched");
                    return Ok(reply);
                }
            }
            return Err(format!("{PROTOCOL_FAILURE} Format repair failed: {error}. Unchanged retries are paused for 60 seconds."));
        }
    };
    tracing::info!("Agent JSON format repair succeeded; entire response validated before dispatch");
    Ok(reply)
}

#[cfg(test)]
#[path = "agent_protocol_tests.rs"]
mod tests;
