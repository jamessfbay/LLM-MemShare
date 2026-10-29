use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    error::{AppError, AppResult},
    model::{ClaimHandoffRequest, CompleteHandoffRequest, CreateHandoffRequest},
};

pub const CREATE_HANDOFF_TOOL: &str = "memshare_create_handoff";
pub const CLAIM_HANDOFF_TOOL: &str = "memshare_claim_handoff";
pub const COMPLETE_HANDOFF_TOOL: &str = "memshare_complete_handoff";
pub const HANDOFF_TOOLS_V1: &str = include_str!("../contracts/handoff-tools-v1.json");

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiFunctionCall {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub arguments: String,
    pub call_id: Option<String>,
    pub id: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicToolUse {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub input: Value,
    pub id: Option<String>,
}

pub fn openai_create(call: OpenAiFunctionCall) -> AppResult<CreateHandoffRequest> {
    validate_openai(&call, CREATE_HANDOFF_TOOL)?;
    serde_json::from_str(&call.arguments)
        .map_err(|_| AppError::invalid("OpenAI tool arguments are not a valid handoff request"))
}

pub fn openai_claim(call: OpenAiFunctionCall) -> AppResult<ClaimHandoffRequest> {
    parse_openai(call, CLAIM_HANDOFF_TOOL, "claim")
}

pub fn openai_complete(call: OpenAiFunctionCall) -> AppResult<CompleteHandoffRequest> {
    parse_openai(call, COMPLETE_HANDOFF_TOOL, "completion")
}

pub fn anthropic_create(call: AnthropicToolUse) -> AppResult<CreateHandoffRequest> {
    parse_anthropic(call, CREATE_HANDOFF_TOOL, "handoff")
}

pub fn anthropic_claim(call: AnthropicToolUse) -> AppResult<ClaimHandoffRequest> {
    parse_anthropic(call, CLAIM_HANDOFF_TOOL, "claim")
}

pub fn anthropic_complete(call: AnthropicToolUse) -> AppResult<CompleteHandoffRequest> {
    parse_anthropic(call, COMPLETE_HANDOFF_TOOL, "completion")
}

fn parse_openai<T: serde::de::DeserializeOwned>(
    call: OpenAiFunctionCall,
    expected: &str,
    label: &str,
) -> AppResult<T> {
    validate_openai(&call, expected)?;
    serde_json::from_str(&call.arguments).map_err(|_| {
        AppError::invalid(format!(
            "OpenAI tool arguments are not a valid {label} request"
        ))
    })
}

fn parse_anthropic<T: serde::de::DeserializeOwned>(
    call: AnthropicToolUse,
    expected: &str,
    label: &str,
) -> AppResult<T> {
    validate_anthropic(&call, expected)?;
    serde_json::from_value(call.input).map_err(|_| {
        AppError::invalid(format!(
            "Anthropic tool input is not a valid {label} request"
        ))
    })
}

fn validate_openai(call: &OpenAiFunctionCall, expected: &str) -> AppResult<()> {
    if call.kind != "function_call" || call.name != expected {
        return Err(AppError::invalid("unexpected OpenAI tool call"));
    }
    if call.call_id.as_deref().is_some_and(str::is_empty) {
        return Err(AppError::invalid("call_id cannot be empty"));
    }
    Ok(())
}

fn validate_anthropic(call: &AnthropicToolUse, expected: &str) -> AppResult<()> {
    if call.kind != "tool_use" || call.name != expected {
        return Err(AppError::invalid("unexpected Anthropic tool use"));
    }
    if call.id.as_deref().is_some_and(str::is_empty) {
        return Err(AppError::invalid("tool-use id cannot be empty"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_openai_function_arguments() {
        let target = uuid::Uuid::new_v4();
        let workspace = uuid::Uuid::new_v4();
        let arguments = json!({
            "workspace_id": workspace,
            "target_agent_id": target,
            "task": {"protocol_version":"1.0","summary":"review","instructions":null,"data":{}},
            "input_memory_ids": [],
            "source": {"provider":"openai","model":"test-model","framework":"responses"},
            "expires_at": null
        })
        .to_string();
        let parsed = openai_create(OpenAiFunctionCall {
            kind: "function_call".into(),
            name: CREATE_HANDOFF_TOOL.into(),
            arguments,
            call_id: Some("call_1".into()),
            id: Some("fc_1".into()),
            status: Some("completed".into()),
        })
        .unwrap();
        assert_eq!(parsed.target_agent_id, target);
    }

    #[test]
    fn parses_anthropic_tool_input() {
        let parsed = anthropic_complete(AnthropicToolUse {
            kind: "tool_use".into(),
            name: COMPLETE_HANDOFF_TOOL.into(),
            input: json!({
                "version": 2,
                "result": {"protocol_version":"1.0","summary":"done","outcome":"succeeded","data":{}},
                "result_memory_ids": [],
                "source": {"provider":"anthropic","model":"test-model","framework":"messages"}
            }),
            id: Some("toolu_1".into()),
        })
        .unwrap();
        assert_eq!(parsed.version, 2);
    }

    #[test]
    fn tool_contract_is_valid_json() {
        let contract: Value = serde_json::from_str(HANDOFF_TOOLS_V1).unwrap();
        assert_eq!(contract["version"], "1.0");
        assert_eq!(contract["tools"].as_array().unwrap().len(), 3);
    }
}
