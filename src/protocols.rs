use axum::{
    extract::{Extension, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    adapters::{CLAIM_HANDOFF_TOOL, COMPLETE_HANDOFF_TOOL, CREATE_HANDOFF_TOOL, HANDOFF_TOOLS_V1},
    api::{claim_handoff_inner, complete_handoff_inner, create_handoff_inner, get_handoff_inner},
    auth::AppState,
    error::{AppError, AppResult},
    model::{
        ClaimHandoffRequest, CompleteHandoffRequest, CreateHandoffRequest, HandoffResponse,
        PrincipalContext,
    },
};

const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const A2A_PROTOCOL_VERSION: &str = "0.2.6";

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

pub async fn mcp(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    Json(request): Json<JsonRpcRequest>,
) -> Response {
    if request.jsonrpc != "2.0" {
        return rpc_error(request.id, -32600, "Invalid Request");
    }
    if request.id.is_none() {
        return StatusCode::NO_CONTENT.into_response();
    }
    let id = request.id.clone();
    let result = match request.method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "llm-memshare", "version": env!("CARGO_PKG_VERSION")},
            "instructions": "Use MemShare tools to exchange authorized tasks between agent principals."
        })),
        "ping" => Ok(json!({})),
        "tools/list" => mcp_tools(),
        "tools/call" => mcp_call(&state, &principal, request.params).await,
        _ => return rpc_error(id, -32601, "Method not found"),
    };
    match result {
        Ok(value) => rpc_result(id, value),
        Err(error) => rpc_error(
            id,
            if error.status == StatusCode::BAD_REQUEST {
                -32602
            } else {
                -32000
            },
            &error.message,
        ),
    }
}

fn mcp_tools() -> AppResult<Value> {
    let contract: Value = serde_json::from_str(HANDOFF_TOOLS_V1)
        .map_err(|error| AppError::config(format!("invalid tool contract: {error}")))?;
    let definitions = contract["$defs"].clone();
    let tools = contract["tools"]
        .as_array()
        .ok_or_else(|| AppError::config("tool contract has no tools"))?
        .iter()
        .map(|tool| {
            let mut input_schema = tool["input_schema"].clone();
            input_schema["$defs"] = definitions.clone();
            if matches!(
                tool["name"].as_str(),
                Some(CLAIM_HANDOFF_TOOL | COMPLETE_HANDOFF_TOOL)
            ) {
                input_schema["properties"]["handoff_id"] =
                    json!({"type": "string", "format": "uuid"});
                if let Some(required) = input_schema["required"].as_array_mut() {
                    required.push(json!("handoff_id"));
                }
            }
            json!({
                "name": tool["name"],
                "description": tool["description"],
                "inputSchema": input_schema
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({"tools": tools}))
}

async fn mcp_call(
    state: &AppState,
    principal: &PrincipalContext,
    params: Value,
) -> AppResult<Value> {
    let name = params["name"]
        .as_str()
        .ok_or_else(|| AppError::invalid("tools/call requires params.name"))?;
    let mut arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let response = match name {
        CREATE_HANDOFF_TOOL => {
            let body: CreateHandoffRequest = serde_json::from_value(arguments)
                .map_err(|_| AppError::invalid("invalid create-handoff arguments"))?;
            let mut headers = HeaderMap::new();
            headers.insert(
                "idempotency-key",
                HeaderValue::from_str(&format!("mcp-{}", principal.request_id))
                    .map_err(|_| AppError::invalid("invalid request id"))?,
            );
            create_handoff_inner(state, principal, &headers, body)
                .await?
                .1
                 .0
        }
        CLAIM_HANDOFF_TOOL => {
            let handoff_id = take_handoff_id(&mut arguments)?;
            let body: ClaimHandoffRequest = serde_json::from_value(arguments)
                .map_err(|_| AppError::invalid("invalid claim-handoff arguments"))?;
            claim_handoff_inner(state, principal, handoff_id, body)
                .await?
                .0
        }
        COMPLETE_HANDOFF_TOOL => {
            let handoff_id = take_handoff_id(&mut arguments)?;
            let body: CompleteHandoffRequest = serde_json::from_value(arguments)
                .map_err(|_| AppError::invalid("invalid complete-handoff arguments"))?;
            complete_handoff_inner(state, principal, handoff_id, body)
                .await?
                .0
        }
        _ => return Err(AppError::invalid("unknown MemShare tool")),
    };
    let structured = serde_json::to_value(&response)
        .map_err(|_| AppError::config("handoff response cannot be encoded"))?;
    Ok(json!({
        "content": [{"type": "text", "text": structured.to_string()}],
        "structuredContent": structured,
        "isError": false
    }))
}

fn take_handoff_id(arguments: &mut Value) -> AppResult<Uuid> {
    let object = arguments
        .as_object_mut()
        .ok_or_else(|| AppError::invalid("tool arguments must be an object"))?;
    let value = object
        .remove("handoff_id")
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| AppError::invalid("handoff_id is required"))?;
    Uuid::parse_str(&value).map_err(|_| AppError::invalid("handoff_id must be a UUID"))
}

pub async fn a2a_agent_card(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "name": "LLM-MemShare",
        "description": "Permissioned multi-tenant memory and task handoff middleware",
        "url": format!("{}/v1/a2a", state.public_base_url),
        "version": env!("CARGO_PKG_VERSION"),
        "protocolVersion": A2A_PROTOCOL_VERSION,
        "capabilities": {"streaming": false, "pushNotifications": false, "stateTransitionHistory": true},
        "defaultInputModes": ["application/json"],
        "defaultOutputModes": ["application/json"],
        "skills": [{
            "id": "memshare-handoff",
            "name": "Permissioned agent handoff",
            "description": "Create and inspect task handoffs using A2A data parts.",
            "tags": ["memory", "handoff", "multi-tenant"],
            "examples": ["Send a data part whose operation is create_handoff."]
        }],
        "securitySchemes": {"bearer": {"type": "http", "scheme": "bearer"}},
        "security": [{"bearer": []}]
    }))
}

pub async fn a2a(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    Json(request): Json<JsonRpcRequest>,
) -> Response {
    if request.jsonrpc != "2.0" || request.id.is_none() {
        return rpc_error(request.id, -32600, "Invalid Request");
    }
    let id = request.id.clone();
    let result = match request.method.as_str() {
        "message/send" => a2a_send(&state, &principal, request.params).await,
        "tasks/get" => a2a_get(&state, &principal, request.params).await,
        _ => return rpc_error(id, -32601, "Method not found"),
    };
    match result {
        Ok(value) => rpc_result(id, value),
        Err(error) => rpc_error(id, -32000, &error.message),
    }
}

async fn a2a_send(
    state: &AppState,
    principal: &PrincipalContext,
    params: Value,
) -> AppResult<Value> {
    let parts = params["message"]["parts"]
        .as_array()
        .ok_or_else(|| AppError::invalid("message.parts is required"))?;
    let data = parts
        .iter()
        .find_map(|part| part.get("data"))
        .cloned()
        .ok_or_else(|| AppError::invalid("a MemShare data part is required"))?;
    if data["operation"] != "create_handoff" {
        return Err(AppError::invalid("unsupported A2A operation"));
    }
    let body: CreateHandoffRequest = serde_json::from_value(data["request"].clone())
        .map_err(|_| AppError::invalid("invalid create_handoff request"))?;
    let idempotency_key = data["idempotency_key"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("a2a-{}", principal.request_id));
    let mut headers = HeaderMap::new();
    headers.insert(
        "idempotency-key",
        HeaderValue::from_str(&idempotency_key)
            .map_err(|_| AppError::invalid("invalid idempotency key"))?,
    );
    let response = create_handoff_inner(state, principal, &headers, body)
        .await?
        .1
         .0;
    Ok(a2a_task(&response))
}

async fn a2a_get(
    state: &AppState,
    principal: &PrincipalContext,
    params: Value,
) -> AppResult<Value> {
    let id = params["id"]
        .as_str()
        .or_else(|| params["taskId"].as_str())
        .ok_or_else(|| AppError::invalid("task id is required"))?;
    let id = Uuid::parse_str(id).map_err(|_| AppError::invalid("task id must be a UUID"))?;
    let response = get_handoff_inner(state, principal, id).await?;
    Ok(a2a_task(&response))
}

fn a2a_task(response: &HandoffResponse) -> Value {
    let state = match response.status.as_str() {
        "queued" => "submitted",
        "claimed" => "working",
        "completed" => "completed",
        "cancelled" => "canceled",
        _ => "unknown",
    };
    let mut task = json!({
        "kind": "task",
        "id": response.id,
        "contextId": response.workspace_id,
        "status": {"state": state, "timestamp": response.updated_at},
        "history": [{
            "kind": "message",
            "messageId": response.id,
            "role": "user",
            "parts": [{"kind": "data", "data": response.task}]
        }]
    });
    if let Some(result) = &response.result {
        task["artifacts"] = json!([{
            "artifactId": format!("{}-result", response.id),
            "name": "MemShare handoff result",
            "parts": [{"kind": "data", "data": result}]
        }]);
    }
    task
}

fn rpc_result(id: Option<Value>, result: Value) -> Response {
    Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}

fn rpc_error(id: Option<Value>, code: i64, message: &str) -> Response {
    Json(json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message}
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_contract_exposes_all_handoff_tools() {
        let tools = mcp_tools().unwrap();
        assert_eq!(tools["tools"].as_array().unwrap().len(), 3);
        assert!(tools["tools"][0].get("inputSchema").is_some());
        assert!(tools["tools"][1]["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("handoff_id")));
    }
}
