use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct PrincipalContext {
    pub credential_id: Uuid,
    pub tenant_id: Uuid,
    pub principal_id: Uuid,
    pub kind: String,
    pub request_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateAgentRequest {
    pub name: String,
    pub workspace_id: Uuid,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct IssuedCredential {
    pub principal_id: Uuid,
    pub credential_id: Uuid,
    pub token: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatePrincipalRequest {
    pub kind: String,
    pub name: String,
    pub tenant_role: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateWorkspaceRequest {
    pub slug: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WorkspaceResponse {
    pub id: Uuid,
    pub slug: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateProjectRequest {
    pub slug: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ProjectResponse {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub slug: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpsertMembershipRequest {
    pub role: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatePrincipalStatusRequest {
    pub status: Option<String>,
    pub tenant_role: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, sqlx::FromRow)]
pub struct PrincipalResponse {
    pub id: Uuid,
    pub status: String,
    pub tenant_role: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateTenantStatusRequest {
    pub status: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateGrantRequest {
    pub subject_principal_id: Uuid,
    pub workspace_id: Uuid,
    pub scope: String,
    pub scope_id: Uuid,
    pub actions: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GrantResponse {
    pub id: Uuid,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateMemoryRequest {
    pub workspace_id: Uuid,
    pub project_id: Option<Uuid>,
    pub scope: String,
    pub scope_id: Uuid,
    pub kind: String,
    pub content: String,
    #[serde(default)]
    pub source: Value,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateMemoryRequest {
    pub version: i32,
    pub content: String,
}

#[derive(Debug, Deserialize, Serialize, sqlx::FromRow)]
pub struct MemoryResponse {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub project_id: Option<Uuid>,
    pub scope: String,
    pub scope_id: Uuid,
    pub owner_principal_id: Option<Uuid>,
    pub creator_principal_id: Uuid,
    pub kind: String,
    pub content: String,
    pub source: Value,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct ListResponse<T> {
    pub items: Vec<T>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CursorListResponse<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct MemoryVersionResponse {
    pub version: i32,
    pub content: String,
    pub changed_by: Uuid,
    pub changed_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PrincipalSummary {
    pub id: Uuid,
    pub kind: String,
    pub name: String,
    pub status: String,
    pub tenant_role: String,
    pub owner_principal_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct WorkspaceSummary {
    pub id: Uuid,
    pub slug: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct GrantSummary {
    pub id: Uuid,
    pub issuer_principal_id: Uuid,
    pub subject_principal_id: Uuid,
    pub workspace_id: Uuid,
    pub scope: String,
    pub scope_id: Uuid,
    pub actions: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct CredentialSummary {
    pub id: Uuid,
    pub principal_id: Uuid,
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct AuditEventResponse {
    pub id: Uuid,
    pub actor_principal_id: Uuid,
    pub credential_id: Option<Uuid>,
    pub action: String,
    pub outcome: String,
    pub target_type: String,
    pub target_id: Option<Uuid>,
    pub request_id: Option<String>,
    pub chain_position: i64,
    pub previous_hash: String,
    pub event_hash: String,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct AuditChainVerification {
    pub event_count: i64,
    pub valid: bool,
    pub head_hash: String,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct TenantLimitsResponse {
    pub max_principals: i64,
    pub max_memories: i64,
    pub max_handoffs: i64,
    pub requests_per_minute: i64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateTenantLimitsRequest {
    pub max_principals: Option<i64>,
    pub max_memories: Option<i64>,
    pub max_handoffs: Option<i64>,
    pub requests_per_minute: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct StatusResponse {
    pub status: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelIdentity {
    pub provider: String,
    pub model: String,
    pub framework: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffTask {
    pub protocol_version: String,
    pub summary: String,
    pub instructions: Option<String>,
    #[serde(default)]
    pub data: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffResult {
    pub protocol_version: String,
    pub summary: String,
    pub outcome: String,
    #[serde(default)]
    pub data: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateHandoffRequest {
    pub workspace_id: Uuid,
    pub target_agent_id: Uuid,
    pub task: HandoffTask,
    #[serde(default)]
    pub input_memory_ids: Vec<Uuid>,
    pub source: ModelIdentity,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimHandoffRequest {
    pub version: i32,
    pub lease_seconds: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransitionHandoffRequest {
    pub version: i32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteHandoffRequest {
    pub version: i32,
    pub result: HandoffResult,
    #[serde(default)]
    pub result_memory_ids: Vec<Uuid>,
    pub source: ModelIdentity,
}

#[derive(Debug, sqlx::FromRow)]
pub struct HandoffRecord {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub source_agent_id: Uuid,
    pub target_agent_id: Uuid,
    pub status: String,
    pub task: Value,
    pub input_memory_ids: Vec<Uuid>,
    pub result: Option<Value>,
    pub result_memory_ids: Vec<Uuid>,
    pub source_metadata: Value,
    pub result_metadata: Option<Value>,
    pub version: i32,
    pub expires_at: Option<DateTime<Utc>>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct HandoffResponse {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub source_agent_id: Uuid,
    pub target_agent_id: Uuid,
    pub status: String,
    pub task: Value,
    pub input_memory_ids: Vec<Uuid>,
    pub omitted_input_references: usize,
    pub result: Option<Value>,
    pub result_memory_ids: Vec<Uuid>,
    pub omitted_result_references: usize,
    pub source_metadata: Value,
    pub result_metadata: Option<Value>,
    pub version: i32,
    pub expires_at: Option<DateTime<Utc>>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct HandoffListResponse {
    pub items: Vec<HandoffResponse>,
    pub next_cursor: Option<String>,
}
