use axum::{
    extract::{
        rejection::{JsonRejection, PathRejection, QueryRejection},
        Extension, FromRequest, FromRequestParts, Path, Query, Request, State,
    },
    http::{request::Parts, HeaderMap, StatusCode},
    middleware,
    routing::{delete, get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction};
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};
use uuid::Uuid;

use crate::{
    adapters::{
        anthropic_claim, anthropic_complete, anthropic_create, openai_claim, openai_complete,
        openai_create, AnthropicToolUse, OpenAiFunctionCall, HANDOFF_TOOLS_V1,
    },
    auth::{authenticate, AppState},
    db::{begin_tenant, issue_token, token_hash},
    error::{AppError, AppResult},
    model::{
        ClaimHandoffRequest, CompleteHandoffRequest, CreateAgentRequest, CreateGrantRequest,
        CreateHandoffRequest, CreateMemoryRequest, CreatePrincipalRequest, CreateProjectRequest,
        CreateWorkspaceRequest, GrantResponse, HandoffListResponse, HandoffRecord, HandoffResponse,
        IssuedCredential, ListResponse, MemoryResponse, ModelIdentity, PrincipalContext,
        PrincipalResponse, ProjectResponse, StatusResponse, TransitionHandoffRequest,
        UpdateMemoryRequest, UpdatePrincipalStatusRequest, UpdateTenantStatusRequest,
        UpsertMembershipRequest, WorkspaceResponse,
    },
};

struct ApiJson<T>(T);
struct ApiPath<T>(T);
struct ApiQuery<T>(T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned,
{
    type Rejection = AppError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|error: JsonRejection| {
                AppError::new(error.status(), "invalid_json", "Invalid JSON request")
            })
    }
}

impl<S, T> FromRequestParts<S> for ApiPath<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned + Send,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Path::<T>::from_request_parts(parts, state)
            .await
            .map(|Path(value)| Self(value))
            .map_err(|error: PathRejection| {
                AppError::new(error.status(), "invalid_path", "Invalid path parameter")
            })
    }
}

impl<S, T> FromRequestParts<S> for ApiQuery<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned + Send,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|error: QueryRejection| {
                AppError::new(error.status(), "invalid_query", "Invalid query parameters")
            })
    }
}

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/v1/principals", post(create_principal))
        .route(
            "/v1/principals/{principal_id}",
            axum::routing::patch(update_principal_status),
        )
        .route(
            "/v1/principals/{principal_id}/credentials",
            post(rotate_credential),
        )
        .route("/v1/tenant", axum::routing::patch(update_tenant_status))
        .route("/v1/workspaces", post(create_workspace))
        .route(
            "/v1/workspaces/{workspace_id}/members/{principal_id}",
            axum::routing::put(upsert_workspace_membership).delete(remove_workspace_membership),
        )
        .route(
            "/v1/workspaces/{workspace_id}/projects",
            post(create_project),
        )
        .route(
            "/v1/workspaces/{workspace_id}/projects/{project_id}/members/{principal_id}",
            axum::routing::put(upsert_project_membership).delete(remove_project_membership),
        )
        .route("/v1/agents", post(create_agent))
        .route("/v1/grants", post(create_grant))
        .route("/v1/grants/{grant_id}", delete(revoke_grant))
        .route("/v1/credentials/{credential_id}", delete(revoke_credential))
        .route("/v1/memories", post(create_memory).get(list_memories))
        .route(
            "/v1/memories/{memory_id}",
            get(get_memory).patch(update_memory).delete(delete_memory),
        )
        .route("/v1/handoffs", post(create_handoff).get(list_handoffs))
        .route("/v1/handoffs/{handoff_id}", get(get_handoff))
        .route("/v1/handoffs/{handoff_id}/claim", post(claim_handoff))
        .route("/v1/handoffs/{handoff_id}/complete", post(complete_handoff))
        .route("/v1/handoffs/{handoff_id}/release", post(release_handoff))
        .route("/v1/handoffs/{handoff_id}/cancel", post(cancel_handoff))
        .route(
            "/v1/adapters/openai/handoffs",
            post(create_handoff_from_openai),
        )
        .route(
            "/v1/adapters/openai/handoffs/{handoff_id}/claim",
            post(claim_handoff_from_openai),
        )
        .route(
            "/v1/adapters/openai/handoffs/{handoff_id}/complete",
            post(complete_handoff_from_openai),
        )
        .route(
            "/v1/adapters/anthropic/handoffs",
            post(create_handoff_from_anthropic),
        )
        .route(
            "/v1/adapters/anthropic/handoffs/{handoff_id}/claim",
            post(claim_handoff_from_anthropic),
        )
        .route(
            "/v1/adapters/anthropic/handoffs/{handoff_id}/complete",
            post(complete_handoff_from_anthropic),
        )
        .layer(middleware::from_fn_with_state(state.clone(), authenticate));

    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/v1/openapi.json", get(openapi_contract))
        .route("/v1/tool-contracts/handoff", get(handoff_tool_contract))
        .merge(protected)
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(TraceLayer::new_for_http())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .method_not_allowed_fallback(method_not_allowed)
        .fallback(route_not_found)
        .with_state(state)
}

async fn route_not_found() -> AppError {
    AppError::not_found()
}

async fn method_not_allowed() -> AppError {
    AppError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "Method not allowed",
    )
}

async fn live() -> Json<StatusResponse> {
    Json(StatusResponse { status: "ok" })
}

async fn ready(State(state): State<AppState>) -> AppResult<Json<StatusResponse>> {
    let schema_ready: bool = sqlx::query_scalar(
        "SELECT to_regclass('memshare.memories') IS NOT NULL AND to_regclass('memshare.handoffs') IS NOT NULL AND to_regprocedure('memshare.authenticate_credential(text)') IS NOT NULL",
    )
    .fetch_one(&state.pool)
    .await?;
    if !schema_ready {
        return Err(AppError::config("database schema is not ready"));
    }
    Ok(Json(StatusResponse { status: "ready" }))
}

async fn handoff_tool_contract() -> AppResult<Json<serde_json::Value>> {
    let contract = serde_json::from_str(HANDOFF_TOOLS_V1)
        .map_err(|error| AppError::config(format!("invalid embedded tool contract: {error}")))?;
    Ok(Json(contract))
}

async fn openapi_contract() -> AppResult<Json<serde_json::Value>> {
    let contract = serde_json::from_str(include_str!("../contracts/openapi-v1.json"))
        .map_err(|error| AppError::config(format!("invalid embedded OpenAPI contract: {error}")))?;
    Ok(Json(contract))
}

async fn create_principal(
    State(state): State<AppState>,
    Extension(actor): Extension<PrincipalContext>,
    ApiJson(body): ApiJson<CreatePrincipalRequest>,
) -> AppResult<(StatusCode, Json<IssuedCredential>)> {
    if actor.kind != "user" {
        return Err(AppError::forbidden());
    }
    if !matches!(body.kind.as_str(), "user" | "service") {
        return Err(AppError::invalid("kind must be user or service"));
    }
    let tenant_role = body.tenant_role.as_deref().unwrap_or("member");
    if !matches!(tenant_role, "admin" | "member")
        || (body.kind == "service" && tenant_role == "admin")
    {
        return Err(AppError::invalid(
            "tenant_role must be member, or admin for a user",
        ));
    }
    validate_display_name(&body.name)?;
    validate_future(body.expires_at)?;
    let mut tx = begin_tenant(&state.pool, &actor).await?;
    require_tenant_admin(&mut tx, &actor).await?;
    let principal_id = Uuid::new_v4();
    let credential_id = Uuid::new_v4();
    let token = issue_token();
    sqlx::query("INSERT INTO memshare.principals(tenant_id,id,kind,name,status,tenant_role) VALUES ($1,$2,$3,$4,'active',$5)")
        .bind(actor.tenant_id).bind(principal_id).bind(&body.kind).bind(body.name.trim()).bind(tenant_role)
        .execute(&mut *tx).await?;
    sqlx::query("INSERT INTO memshare.credentials(id,tenant_id,principal_id,token_hash,status,expires_at) VALUES ($1,$2,$3,$4,'active',$5)")
        .bind(credential_id).bind(actor.tenant_id).bind(principal_id).bind(token_hash(&token))
        .bind(body.expires_at).execute(&mut *tx).await?;
    audit(
        &mut tx,
        &actor,
        "principal.created",
        "principal",
        Some(principal_id),
        "allowed",
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(IssuedCredential {
            principal_id,
            credential_id,
            token,
        }),
    ))
}

async fn update_principal_status(
    State(state): State<AppState>,
    Extension(actor): Extension<PrincipalContext>,
    ApiPath(principal_id): ApiPath<Uuid>,
    ApiJson(body): ApiJson<UpdatePrincipalStatusRequest>,
) -> AppResult<Json<PrincipalResponse>> {
    if actor.kind != "user" {
        return Err(AppError::forbidden());
    }
    if body.status.is_none() && body.tenant_role.is_none() {
        return Err(AppError::invalid("status or tenant_role is required"));
    }
    if body
        .status
        .as_deref()
        .is_some_and(|value| !matches!(value, "active" | "disabled"))
    {
        return Err(AppError::invalid("status must be active or disabled"));
    }
    if body
        .tenant_role
        .as_deref()
        .is_some_and(|value| !matches!(value, "admin" | "member"))
    {
        return Err(AppError::invalid("tenant_role must be admin or member"));
    }
    if principal_id == actor.principal_id && body.status.as_deref() == Some("disabled") {
        return Err(AppError::invalid(
            "an administrator cannot disable its own principal",
        ));
    }
    if principal_id == actor.principal_id && body.tenant_role.as_deref() == Some("member") {
        return Err(AppError::invalid(
            "an administrator cannot demote its own tenant role",
        ));
    }
    let mut tx = begin_tenant(&state.pool, &actor).await?;
    require_tenant_admin(&mut tx, &actor).await?;
    if body.tenant_role.as_deref() == Some("admin") {
        let is_user: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memshare.principals WHERE tenant_id=$1 AND id=$2 AND kind='user')")
            .bind(actor.tenant_id).bind(principal_id).fetch_one(&mut *tx).await?;
        if !is_user {
            return Err(AppError::invalid("only users may be tenant administrators"));
        }
    }
    if body.status.as_deref() == Some("active") {
        let owner_id: Option<Uuid> = sqlx::query_scalar::<_, Uuid>("SELECT owner_principal_id FROM memshare.principals WHERE tenant_id=$1 AND id=$2 AND kind='agent'")
            .bind(actor.tenant_id).bind(principal_id).fetch_optional(&mut *tx).await?;
        if let Some(owner_id) = owner_id {
            let owner_active: Option<bool> = sqlx::query_scalar("SELECT status='active' FROM memshare.principals WHERE tenant_id=$1 AND id=$2 FOR UPDATE")
                .bind(actor.tenant_id).bind(owner_id).fetch_optional(&mut *tx).await?;
            if owner_active != Some(true) {
                return Err(AppError::invalid(
                    "an agent cannot be activated while its owner is inactive",
                ));
            }
        }
    }
    let result = sqlx::query_as::<_, PrincipalResponse>("UPDATE memshare.principals SET status=COALESCE($1,status),tenant_role=COALESCE($2,tenant_role),updated_at=now() WHERE tenant_id=$3 AND id=$4 RETURNING id,status,tenant_role")
        .bind(&body.status).bind(&body.tenant_role).bind(actor.tenant_id).bind(principal_id)
        .fetch_optional(&mut *tx).await?.ok_or_else(AppError::not_found)?;
    if body.status.as_deref() == Some("disabled") {
        sqlx::query("UPDATE memshare.principals SET status='disabled',updated_at=now() WHERE tenant_id=$1 AND owner_principal_id=$2 AND kind='agent' AND status='active'")
            .bind(actor.tenant_id).bind(principal_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE memshare.credentials c SET status='revoked',revoked_at=now() WHERE c.tenant_id=$1 AND c.status='active' AND (c.principal_id=$2 OR EXISTS(SELECT 1 FROM memshare.principals owned WHERE owned.tenant_id=c.tenant_id AND owned.id=c.principal_id AND owned.owner_principal_id=$2 AND owned.kind='agent'))")
            .bind(actor.tenant_id).bind(principal_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE memshare.grants g SET revoked_at=now() WHERE g.tenant_id=$1 AND g.revoked_at IS NULL AND (g.subject_principal_id=$2 OR g.issuer_principal_id=$2 OR (g.scope='agent' AND g.scope_id=$2) OR EXISTS(SELECT 1 FROM memshare.principals owned WHERE owned.tenant_id=g.tenant_id AND owned.owner_principal_id=$2 AND owned.kind='agent' AND (owned.id IN (g.subject_principal_id,g.issuer_principal_id) OR (g.scope='agent' AND owned.id=g.scope_id))))")
            .bind(actor.tenant_id).bind(principal_id).execute(&mut *tx).await?;
    }
    audit(
        &mut tx,
        &actor,
        "principal.updated",
        "principal",
        Some(principal_id),
        "allowed",
    )
    .await?;
    tx.commit().await?;
    Ok(Json(result))
}

async fn rotate_credential(
    State(state): State<AppState>,
    Extension(actor): Extension<PrincipalContext>,
    ApiPath(principal_id): ApiPath<Uuid>,
) -> AppResult<(StatusCode, Json<IssuedCredential>)> {
    let mut tx = begin_tenant(&state.pool, &actor).await?;
    if actor.principal_id != principal_id {
        require_tenant_admin(&mut tx, &actor).await?;
    }
    let active: Option<bool> = sqlx::query_scalar(
        "SELECT status='active' FROM memshare.principals WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
    )
    .bind(actor.tenant_id)
    .bind(principal_id)
    .fetch_optional(&mut *tx)
    .await?;
    if active != Some(true) {
        return Err(AppError::not_found());
    }
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!(
            "{}:{}:credential.rotate",
            actor.tenant_id, principal_id
        ))
        .execute(&mut *tx)
        .await?;
    let expires_at: Option<DateTime<Utc>> = sqlx::query_scalar(
        "SELECT CASE WHEN bool_or(expires_at IS NULL) THEN NULL ELSE max(expires_at) END FROM memshare.credentials WHERE tenant_id=$1 AND principal_id=$2 AND status='active' AND (expires_at IS NULL OR expires_at>now())",
    )
    .bind(actor.tenant_id)
    .bind(principal_id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("UPDATE memshare.credentials SET status='revoked',revoked_at=now() WHERE tenant_id=$1 AND principal_id=$2 AND status='active'")
        .bind(actor.tenant_id).bind(principal_id).execute(&mut *tx).await?;
    let credential_id = Uuid::new_v4();
    let token = issue_token();
    sqlx::query("INSERT INTO memshare.credentials(id,tenant_id,principal_id,token_hash,status,expires_at) VALUES ($1,$2,$3,$4,'active',$5)")
        .bind(credential_id).bind(actor.tenant_id).bind(principal_id).bind(token_hash(&token)).bind(expires_at).execute(&mut *tx).await?;
    audit(
        &mut tx,
        &actor,
        "credential.rotated",
        "credential",
        Some(credential_id),
        "allowed",
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(IssuedCredential {
            principal_id,
            credential_id,
            token,
        }),
    ))
}

async fn update_tenant_status(
    State(state): State<AppState>,
    Extension(actor): Extension<PrincipalContext>,
    ApiJson(body): ApiJson<UpdateTenantStatusRequest>,
) -> AppResult<Json<StatusResponse>> {
    if actor.kind != "user" {
        return Err(AppError::forbidden());
    }
    if !matches!(body.status.as_str(), "active" | "suspended") {
        return Err(AppError::invalid("status must be active or suspended"));
    }
    let mut tx = begin_tenant(&state.pool, &actor).await?;
    require_tenant_admin(&mut tx, &actor).await?;
    sqlx::query("UPDATE memshare.tenants SET status=$1,updated_at=now() WHERE id=$2")
        .bind(&body.status)
        .bind(actor.tenant_id)
        .execute(&mut *tx)
        .await?;
    audit(
        &mut tx,
        &actor,
        "tenant.status_updated",
        "tenant",
        Some(actor.tenant_id),
        "allowed",
    )
    .await?;
    tx.commit().await?;
    Ok(Json(StatusResponse {
        status: if body.status == "active" {
            "active"
        } else {
            "suspended"
        },
    }))
}

async fn create_workspace(
    State(state): State<AppState>,
    Extension(actor): Extension<PrincipalContext>,
    ApiJson(body): ApiJson<CreateWorkspaceRequest>,
) -> AppResult<(StatusCode, Json<WorkspaceResponse>)> {
    validate_slug(&body.slug)?;
    let mut tx = begin_tenant(&state.pool, &actor).await?;
    require_tenant_admin(&mut tx, &actor).await?;
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO memshare.workspaces(tenant_id,id,slug,status) VALUES ($1,$2,$3,'active')",
    )
    .bind(actor.tenant_id)
    .bind(id)
    .bind(&body.slug)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO memshare.workspace_memberships(tenant_id,workspace_id,principal_id,role,status) VALUES ($1,$2,$3,'admin','active')")
        .bind(actor.tenant_id).bind(id).bind(actor.principal_id).execute(&mut *tx).await?;
    audit(
        &mut tx,
        &actor,
        "workspace.created",
        "workspace",
        Some(id),
        "allowed",
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(WorkspaceResponse {
            id,
            slug: body.slug,
        }),
    ))
}

async fn upsert_workspace_membership(
    State(state): State<AppState>,
    Extension(actor): Extension<PrincipalContext>,
    ApiPath((workspace_id, principal_id)): ApiPath<(Uuid, Uuid)>,
    ApiJson(body): ApiJson<UpsertMembershipRequest>,
) -> AppResult<Json<StatusResponse>> {
    validate_role(&body.role)?;
    let mut tx = begin_tenant(&state.pool, &actor).await?;
    require_workspace_role(
        &mut tx,
        actor.tenant_id,
        workspace_id,
        actor.principal_id,
        "admin",
    )
    .await?;
    require_active_principal(&mut tx, actor.tenant_id, principal_id).await?;
    if principal_id == actor.principal_id && body.role == "member" {
        return Err(AppError::invalid(
            "an administrator cannot demote its own workspace role",
        ));
    }
    sqlx::query("INSERT INTO memshare.workspace_memberships(tenant_id,workspace_id,principal_id,role,status) VALUES ($1,$2,$3,$4,'active') ON CONFLICT (tenant_id,workspace_id,principal_id) DO UPDATE SET role=EXCLUDED.role,status='active',updated_at=now()")
        .bind(actor.tenant_id).bind(workspace_id).bind(principal_id).bind(&body.role).execute(&mut *tx).await?;
    audit(
        &mut tx,
        &actor,
        "workspace.member_upserted",
        "principal",
        Some(principal_id),
        "allowed",
    )
    .await?;
    tx.commit().await?;
    Ok(Json(StatusResponse { status: "active" }))
}

async fn remove_workspace_membership(
    State(state): State<AppState>,
    Extension(actor): Extension<PrincipalContext>,
    ApiPath((workspace_id, principal_id)): ApiPath<(Uuid, Uuid)>,
) -> AppResult<Json<StatusResponse>> {
    let mut tx = begin_tenant(&state.pool, &actor).await?;
    require_workspace_role(
        &mut tx,
        actor.tenant_id,
        workspace_id,
        actor.principal_id,
        "admin",
    )
    .await?;
    if principal_id == actor.principal_id {
        return Err(AppError::invalid(
            "an administrator cannot remove its own workspace membership",
        ));
    }
    let result = sqlx::query("UPDATE memshare.workspace_memberships SET status='removed',updated_at=now() WHERE tenant_id=$1 AND workspace_id=$2 AND principal_id=$3 AND status='active'")
        .bind(actor.tenant_id).bind(workspace_id).bind(principal_id).execute(&mut *tx).await?;
    if result.rows_affected() != 1 {
        return Err(AppError::not_found());
    }
    sqlx::query("UPDATE memshare.project_memberships SET status='removed',updated_at=now() WHERE tenant_id=$1 AND workspace_id=$2 AND principal_id=$3 AND status='active'")
        .bind(actor.tenant_id).bind(workspace_id).bind(principal_id).execute(&mut *tx).await?;
    sqlx::query("UPDATE memshare.grants SET revoked_at=now() WHERE tenant_id=$1 AND workspace_id=$2 AND revoked_at IS NULL AND (subject_principal_id=$3 OR issuer_principal_id=$3 OR (scope='agent' AND scope_id=$3))")
        .bind(actor.tenant_id).bind(workspace_id).bind(principal_id).execute(&mut *tx).await?;
    audit(
        &mut tx,
        &actor,
        "workspace.member_removed",
        "principal",
        Some(principal_id),
        "allowed",
    )
    .await?;
    tx.commit().await?;
    Ok(Json(StatusResponse { status: "removed" }))
}

async fn create_project(
    State(state): State<AppState>,
    Extension(actor): Extension<PrincipalContext>,
    ApiPath(workspace_id): ApiPath<Uuid>,
    ApiJson(body): ApiJson<CreateProjectRequest>,
) -> AppResult<(StatusCode, Json<ProjectResponse>)> {
    validate_slug(&body.slug)?;
    let mut tx = begin_tenant(&state.pool, &actor).await?;
    require_workspace_role(
        &mut tx,
        actor.tenant_id,
        workspace_id,
        actor.principal_id,
        "admin",
    )
    .await?;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO memshare.projects(tenant_id,workspace_id,id,slug,status) VALUES ($1,$2,$3,$4,'active')")
        .bind(actor.tenant_id).bind(workspace_id).bind(id).bind(&body.slug).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO memshare.project_memberships(tenant_id,workspace_id,project_id,principal_id,role,status) VALUES ($1,$2,$3,$4,'admin','active')")
        .bind(actor.tenant_id).bind(workspace_id).bind(id).bind(actor.principal_id).execute(&mut *tx).await?;
    audit(
        &mut tx,
        &actor,
        "project.created",
        "project",
        Some(id),
        "allowed",
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(ProjectResponse {
            id,
            workspace_id,
            slug: body.slug,
        }),
    ))
}

async fn upsert_project_membership(
    State(state): State<AppState>,
    Extension(actor): Extension<PrincipalContext>,
    ApiPath((workspace_id, project_id, principal_id)): ApiPath<(Uuid, Uuid, Uuid)>,
    ApiJson(body): ApiJson<UpsertMembershipRequest>,
) -> AppResult<Json<StatusResponse>> {
    validate_role(&body.role)?;
    let mut tx = begin_tenant(&state.pool, &actor).await?;
    if !project_admin(
        &mut tx,
        actor.tenant_id,
        workspace_id,
        project_id,
        actor.principal_id,
    )
    .await?
    {
        return Err(AppError::forbidden());
    }
    if !workspace_member(&mut tx, actor.tenant_id, workspace_id, principal_id).await? {
        return Err(AppError::not_found());
    }
    if principal_id == actor.principal_id && body.role == "member" {
        return Err(AppError::invalid(
            "an administrator cannot demote its own project role",
        ));
    }
    sqlx::query("INSERT INTO memshare.project_memberships(tenant_id,workspace_id,project_id,principal_id,role,status) VALUES ($1,$2,$3,$4,$5,'active') ON CONFLICT (tenant_id,workspace_id,project_id,principal_id) DO UPDATE SET role=EXCLUDED.role,status='active',updated_at=now()")
        .bind(actor.tenant_id).bind(workspace_id).bind(project_id).bind(principal_id).bind(&body.role).execute(&mut *tx).await?;
    audit(
        &mut tx,
        &actor,
        "project.member_upserted",
        "principal",
        Some(principal_id),
        "allowed",
    )
    .await?;
    tx.commit().await?;
    Ok(Json(StatusResponse { status: "active" }))
}

async fn remove_project_membership(
    State(state): State<AppState>,
    Extension(actor): Extension<PrincipalContext>,
    ApiPath((workspace_id, project_id, principal_id)): ApiPath<(Uuid, Uuid, Uuid)>,
) -> AppResult<Json<StatusResponse>> {
    let mut tx = begin_tenant(&state.pool, &actor).await?;
    if !project_admin(
        &mut tx,
        actor.tenant_id,
        workspace_id,
        project_id,
        actor.principal_id,
    )
    .await?
    {
        return Err(AppError::forbidden());
    }
    if principal_id == actor.principal_id {
        return Err(AppError::invalid(
            "an administrator cannot remove its own project membership",
        ));
    }
    let result = sqlx::query("UPDATE memshare.project_memberships SET status='removed',updated_at=now() WHERE tenant_id=$1 AND workspace_id=$2 AND project_id=$3 AND principal_id=$4 AND status='active'")
        .bind(actor.tenant_id).bind(workspace_id).bind(project_id).bind(principal_id).execute(&mut *tx).await?;
    if result.rows_affected() != 1 {
        return Err(AppError::not_found());
    }
    audit(
        &mut tx,
        &actor,
        "project.member_removed",
        "principal",
        Some(principal_id),
        "allowed",
    )
    .await?;
    tx.commit().await?;
    Ok(Json(StatusResponse { status: "removed" }))
}

async fn create_agent(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    ApiJson(body): ApiJson<CreateAgentRequest>,
) -> AppResult<(StatusCode, Json<IssuedCredential>)> {
    if principal.kind != "user" {
        return Err(AppError::forbidden());
    }
    if body.name.trim().is_empty() || body.name.len() > 200 {
        return Err(AppError::invalid("name must contain 1-200 characters"));
    }
    if body.expires_at.is_some_and(|value| value <= Utc::now()) {
        return Err(AppError::invalid("expires_at must be in the future"));
    }
    let mut tx = begin_tenant(&state.pool, &principal).await?;
    require_active_principal(&mut tx, principal.tenant_id, principal.principal_id).await?;
    require_workspace_role(
        &mut tx,
        principal.tenant_id,
        body.workspace_id,
        principal.principal_id,
        "admin",
    )
    .await?;
    let principal_id = Uuid::new_v4();
    let credential_id = Uuid::new_v4();
    let token = issue_token();
    sqlx::query("INSERT INTO memshare.principals(tenant_id,id,kind,name,status,owner_principal_id) VALUES ($1,$2,'agent',$3,'active',$4)")
        .bind(principal.tenant_id)
        .bind(principal_id)
        .bind(body.name.trim())
        .bind(principal.principal_id)
        .execute(&mut *tx).await?;
    sqlx::query("INSERT INTO memshare.workspace_memberships(tenant_id,workspace_id,principal_id,role,status) VALUES ($1,$2,$3,'member','active')")
        .bind(principal.tenant_id).bind(body.workspace_id).bind(principal_id)
        .execute(&mut *tx).await?;
    sqlx::query("INSERT INTO memshare.credentials(id,tenant_id,principal_id,token_hash,status,expires_at) VALUES ($1,$2,$3,$4,'active',$5)")
        .bind(credential_id).bind(principal.tenant_id).bind(principal_id)
        .bind(token_hash(&token)).bind(body.expires_at)
        .execute(&mut *tx).await?;
    audit(
        &mut tx,
        &principal,
        "agent.created",
        "principal",
        Some(principal_id),
        "allowed",
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(IssuedCredential {
            principal_id,
            credential_id,
            token,
        }),
    ))
}

async fn create_grant(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    ApiJson(body): ApiJson<CreateGrantRequest>,
) -> AppResult<(StatusCode, Json<GrantResponse>)> {
    if principal.kind != "user" {
        return Err(AppError::forbidden());
    }
    validate_actions(&body.actions)?;
    if body.actions.iter().any(|action| action == "handoff") && body.scope != "agent" {
        return Err(AppError::invalid(
            "the handoff action is valid only for agent scope",
        ));
    }
    if body.expires_at.is_some_and(|value| value <= Utc::now()) {
        return Err(AppError::invalid("expires_at must be in the future"));
    }
    let mut tx = begin_tenant(&state.pool, &principal).await?;
    let mut participant_ids = if principal.principal_id == body.subject_principal_id {
        vec![principal.principal_id]
    } else {
        vec![principal.principal_id, body.subject_principal_id]
    };
    if body.scope == "agent" {
        participant_ids.push(body.scope_id);
    }
    participant_ids.sort_unstable();
    participant_ids.dedup();
    let actor_active: Option<bool> = sqlx::query_scalar(
        "SELECT status='active' FROM memshare.principals WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
    )
    .bind(principal.tenant_id)
    .bind(principal.principal_id)
    .fetch_optional(&mut *tx)
    .await?;
    if actor_active != Some(true) {
        return Err(AppError::forbidden());
    }
    let related_ids: Vec<Uuid> = participant_ids
        .iter()
        .copied()
        .filter(|id| *id != principal.principal_id)
        .collect();
    let active_principals: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM memshare.principals WHERE tenant_id=$1 AND id=ANY($2) AND status='active' ORDER BY id FOR UPDATE",
    )
    .bind(principal.tenant_id)
    .bind(&related_ids)
    .fetch_all(&mut *tx)
    .await?;
    if body.subject_principal_id != principal.principal_id
        && !active_principals.contains(&body.subject_principal_id)
    {
        return Err(AppError::not_found());
    }
    if body.scope == "agent"
        && body.scope_id != principal.principal_id
        && !active_principals.contains(&body.scope_id)
    {
        return Err(AppError::not_found());
    }
    let actor_is_member: Option<bool> = sqlx::query_scalar(
        "SELECT status='active' FROM memshare.workspace_memberships WHERE tenant_id=$1 AND workspace_id=$2 AND principal_id=$3 FOR UPDATE",
    )
    .bind(principal.tenant_id)
    .bind(body.workspace_id)
    .bind(principal.principal_id)
    .fetch_optional(&mut *tx)
    .await?;
    if actor_is_member != Some(true) {
        return Err(AppError::forbidden());
    }
    let active_members: Vec<Uuid> = sqlx::query_scalar(
        "SELECT principal_id FROM memshare.workspace_memberships WHERE tenant_id=$1 AND workspace_id=$2 AND principal_id=ANY($3) AND status='active' ORDER BY principal_id FOR UPDATE",
    )
    .bind(principal.tenant_id)
    .bind(body.workspace_id)
    .bind(&related_ids)
    .fetch_all(&mut *tx)
    .await?;
    if body.subject_principal_id != principal.principal_id
        && !active_members.contains(&body.subject_principal_id)
    {
        return Err(AppError::not_found());
    }
    if body.scope == "agent"
        && body.scope_id != principal.principal_id
        && !active_members.contains(&body.scope_id)
    {
        return Err(AppError::not_found());
    }
    authorize_grant_issuer(&mut tx, &principal, body.workspace_id, &body).await?;
    let grant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO memshare.grants(tenant_id,id,issuer_principal_id,subject_principal_id,workspace_id,scope,scope_id,actions,expires_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)")
        .bind(principal.tenant_id).bind(grant_id).bind(principal.principal_id)
        .bind(body.subject_principal_id).bind(body.workspace_id).bind(&body.scope)
        .bind(body.scope_id).bind(&body.actions).bind(body.expires_at)
        .execute(&mut *tx).await?;
    audit(
        &mut tx,
        &principal,
        "grant.created",
        "grant",
        Some(grant_id),
        "allowed",
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(GrantResponse { id: grant_id })))
}

async fn revoke_grant(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    ApiPath(grant_id): ApiPath<Uuid>,
) -> AppResult<Json<StatusResponse>> {
    if principal.kind != "user" {
        return Err(AppError::forbidden());
    }
    let mut tx = begin_tenant(&state.pool, &principal).await?;
    let result = sqlx::query("UPDATE memshare.grants SET revoked_at=now() WHERE tenant_id=$1 AND id=$2 AND issuer_principal_id=$3 AND revoked_at IS NULL")
        .bind(principal.tenant_id).bind(grant_id).bind(principal.principal_id)
        .execute(&mut *tx).await?;
    if result.rows_affected() == 0 {
        return Err(AppError::not_found());
    }
    audit(
        &mut tx,
        &principal,
        "grant.revoked",
        "grant",
        Some(grant_id),
        "allowed",
    )
    .await?;
    tx.commit().await?;
    Ok(Json(StatusResponse { status: "revoked" }))
}

async fn revoke_credential(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    ApiPath(credential_id): ApiPath<Uuid>,
) -> AppResult<Json<StatusResponse>> {
    if principal.kind != "user" {
        return Err(AppError::forbidden());
    }
    let mut tx = begin_tenant(&state.pool, &principal).await?;
    let result = sqlx::query("UPDATE memshare.credentials c SET status='revoked',revoked_at=now() FROM memshare.principals p WHERE c.id=$1 AND c.tenant_id=$2 AND p.tenant_id=c.tenant_id AND p.id=c.principal_id AND p.owner_principal_id=$3 AND c.status='active'")
        .bind(credential_id).bind(principal.tenant_id).bind(principal.principal_id)
        .execute(&mut *tx).await?;
    if result.rows_affected() == 0 {
        return Err(AppError::not_found());
    }
    audit(
        &mut tx,
        &principal,
        "credential.revoked",
        "credential",
        Some(credential_id),
        "allowed",
    )
    .await?;
    tx.commit().await?;
    Ok(Json(StatusResponse { status: "revoked" }))
}

async fn create_memory(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<CreateMemoryRequest>,
) -> AppResult<(StatusCode, Json<MemoryResponse>)> {
    validate_memory(&body)?;
    let key = idempotency_key(&headers)?;
    let request_hash = canonical_hash(&body)?;
    let mut tx = begin_tenant(&state.pool, &principal).await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!(
            "{}:{}:memory.create:{}",
            principal.tenant_id, principal.principal_id, key
        ))
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM memshare.idempotency WHERE tenant_id=$1 AND principal_id=$2 AND operation='memory.create' AND key=$3 AND expires_at<=now()")
        .bind(principal.tenant_id).bind(principal.principal_id).bind(&key)
        .execute(&mut *tx).await?;
    if let Some((old_hash,result_id)) = sqlx::query_as::<_,(String,Uuid)>("SELECT input_hash,result_id FROM memshare.idempotency WHERE tenant_id=$1 AND principal_id=$2 AND operation='memory.create' AND key=$3 AND expires_at>now()")
        .bind(principal.tenant_id).bind(principal.principal_id).bind(&key)
        .fetch_optional(&mut *tx).await? {
        if old_hash != request_hash { return Err(AppError::conflict("idempotency_conflict","Idempotency key was used with different input")); }
        let memory = fetch_authorized_memory(&mut tx,&principal,result_id,"read").await?;
        tx.commit().await?;
        return Ok((StatusCode::OK,Json(memory)));
    }
    authorize_scope(&mut tx, &principal, &body, "write").await?;
    let id = Uuid::new_v4();
    let owner = match body.scope.as_str() {
        "private" | "agent" => Some(body.scope_id),
        _ => None,
    };
    let source = json!({"verified":{"actor_principal_id":principal.principal_id,"method":"api"},"client":body.source});
    sqlx::query("INSERT INTO memshare.memories(tenant_id,id,workspace_id,project_id,scope,scope_id,owner_principal_id,creator_principal_id,kind,content,source,expires_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)")
        .bind(principal.tenant_id).bind(id).bind(body.workspace_id).bind(body.project_id)
        .bind(&body.scope).bind(body.scope_id).bind(owner).bind(principal.principal_id)
        .bind(&body.kind).bind(body.content.trim()).bind(source).bind(body.expires_at)
        .execute(&mut *tx).await?;
    sqlx::query("INSERT INTO memshare.idempotency(tenant_id,principal_id,operation,key,input_hash,result_id,expires_at) VALUES ($1,$2,'memory.create',$3,$4,$5,$6)")
        .bind(principal.tenant_id).bind(principal.principal_id).bind(key).bind(request_hash)
        .bind(id).bind(Utc::now()+Duration::hours(24)).execute(&mut *tx).await?;
    audit(
        &mut tx,
        &principal,
        "memory.created",
        "memory",
        Some(id),
        "allowed",
    )
    .await?;
    let memory = fetch_authorized_memory(&mut tx, &principal, id, "read").await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(memory)))
}

async fn create_handoff(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<CreateHandoffRequest>,
) -> AppResult<(StatusCode, Json<HandoffResponse>)> {
    create_handoff_inner(&state, &principal, &headers, body).await
}

async fn create_handoff_from_openai(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    headers: HeaderMap,
    ApiJson(call): ApiJson<OpenAiFunctionCall>,
) -> AppResult<(StatusCode, Json<HandoffResponse>)> {
    let body = openai_create(call)?;
    create_handoff_inner(&state, &principal, &headers, body).await
}

async fn create_handoff_from_anthropic(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    headers: HeaderMap,
    ApiJson(call): ApiJson<AnthropicToolUse>,
) -> AppResult<(StatusCode, Json<HandoffResponse>)> {
    let body = anthropic_create(call)?;
    create_handoff_inner(&state, &principal, &headers, body).await
}

async fn create_handoff_inner(
    state: &AppState,
    principal: &PrincipalContext,
    headers: &HeaderMap,
    body: CreateHandoffRequest,
) -> AppResult<(StatusCode, Json<HandoffResponse>)> {
    if principal.kind != "agent" {
        return Err(AppError::forbidden());
    }
    validate_handoff_create(&body)?;
    if body.target_agent_id == principal.principal_id {
        return Err(AppError::invalid("source and target agents must differ"));
    }
    let key = idempotency_key(headers)?;
    let request_hash = canonical_hash(&body)?;
    let mut tx = begin_tenant(&state.pool, principal).await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!(
            "{}:{}:handoff.create:{}",
            principal.tenant_id, principal.principal_id, key
        ))
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM memshare.idempotency WHERE tenant_id=$1 AND principal_id=$2 AND operation='handoff.create' AND key=$3 AND expires_at<=now()")
        .bind(principal.tenant_id).bind(principal.principal_id).bind(&key)
        .execute(&mut *tx).await?;
    if let Some((old_hash, result_id)) = sqlx::query_as::<_, (String, Uuid)>(
        "SELECT input_hash,result_id FROM memshare.idempotency WHERE tenant_id=$1 AND principal_id=$2 AND operation='handoff.create' AND key=$3 AND expires_at>now()",
    )
    .bind(principal.tenant_id)
    .bind(principal.principal_id)
    .bind(&key)
    .fetch_optional(&mut *tx)
    .await?
    {
        if old_hash != request_hash {
            return Err(AppError::conflict(
                "idempotency_conflict",
                "Idempotency key was used with different input",
            ));
        }
        let record = fetch_handoff(&mut tx, principal, result_id).await?;
        let response = render_handoff(&mut tx, principal, record).await?;
        tx.commit().await?;
        return Ok((StatusCode::OK, Json(response)));
    }

    let target_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM memshare.principals p JOIN memshare.workspace_memberships wm ON wm.tenant_id=p.tenant_id AND wm.principal_id=p.id WHERE p.tenant_id=$1 AND p.id=$2 AND p.kind='agent' AND p.status='active' AND wm.workspace_id=$3 AND wm.status='active')",
    )
    .bind(principal.tenant_id)
    .bind(body.target_agent_id)
    .bind(body.workspace_id)
    .fetch_one(&mut *tx)
    .await?;
    let source_is_member = workspace_member(
        &mut tx,
        principal.tenant_id,
        body.workspace_id,
        principal.principal_id,
    )
    .await?;
    if !target_exists || !source_is_member {
        return Err(AppError::not_found());
    }
    if !has_grant(
        &mut tx,
        principal,
        body.workspace_id,
        "agent",
        body.target_agent_id,
        "handoff",
    )
    .await?
    {
        return Err(AppError::forbidden());
    }
    require_readable_memories(&mut tx, principal, &body.input_memory_ids).await?;

    let id = Uuid::new_v4();
    let source_metadata = verified_model_metadata(principal, &body.source);
    sqlx::query("INSERT INTO memshare.handoffs(tenant_id,id,workspace_id,source_agent_id,target_agent_id,status,task,input_memory_ids,source_metadata,expires_at) VALUES ($1,$2,$3,$4,$5,'queued',$6,$7,$8,$9)")
        .bind(principal.tenant_id)
        .bind(id)
        .bind(body.workspace_id)
        .bind(principal.principal_id)
        .bind(body.target_agent_id)
        .bind(serde_json::to_value(body.task).map_err(|_| AppError::invalid("task cannot be encoded"))?)
        .bind(body.input_memory_ids)
        .bind(source_metadata)
        .bind(body.expires_at)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO memshare.idempotency(tenant_id,principal_id,operation,key,input_hash,result_id,expires_at) VALUES ($1,$2,'handoff.create',$3,$4,$5,$6)")
        .bind(principal.tenant_id)
        .bind(principal.principal_id)
        .bind(key)
        .bind(request_hash)
        .bind(id)
        .bind(Utc::now() + Duration::hours(24))
        .execute(&mut *tx)
        .await?;
    audit(
        &mut tx,
        principal,
        "handoff.created",
        "handoff",
        Some(id),
        "allowed",
    )
    .await?;
    let record = fetch_handoff(&mut tx, principal, id).await?;
    let response = render_handoff(&mut tx, principal, record).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(response)))
}

#[derive(Deserialize)]
struct HandoffListQuery {
    status: Option<String>,
    direction: Option<String>,
    limit: Option<i64>,
    cursor: Option<String>,
}

async fn list_handoffs(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    ApiQuery(query): ApiQuery<HandoffListQuery>,
) -> AppResult<Json<HandoffListResponse>> {
    if principal.kind != "agent" {
        return Err(AppError::forbidden());
    }
    let limit = query.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(AppError::invalid("limit must be 1-100"));
    }
    let direction = query.direction.unwrap_or_else(|| "inbox".to_owned());
    if !matches!(direction.as_str(), "inbox" | "outbox" | "all") {
        return Err(AppError::invalid("direction must be inbox, outbox, or all"));
    }
    if query
        .status
        .as_deref()
        .is_some_and(|value| !matches!(value, "queued" | "claimed" | "completed" | "cancelled"))
    {
        return Err(AppError::invalid("invalid handoff status"));
    }
    let (cursor_time, cursor_id) = decode_handoff_cursor(query.cursor.as_deref())?;
    let mut tx = begin_tenant(&state.pool, &principal).await?;
    let records = sqlx::query_as::<_, HandoffRecord>(&format!(
        "SELECT {} FROM memshare.handoffs h WHERE h.tenant_id=$1 AND (h.expires_at IS NULL OR h.expires_at>now()) AND ($2::text IS NULL OR h.status=$2) AND ($3::timestamptz IS NULL OR (h.created_at,h.id)<($3,$4)) AND (($5='inbox' AND h.target_agent_id=$6) OR ($5='outbox' AND h.source_agent_id=$6) OR ($5='all' AND (h.source_agent_id=$6 OR h.target_agent_id=$6))) AND EXISTS(SELECT 1 FROM memshare.workspace_memberships wm WHERE wm.tenant_id=h.tenant_id AND wm.workspace_id=h.workspace_id AND wm.principal_id=$6 AND wm.status='active') ORDER BY h.created_at DESC,h.id DESC LIMIT $7",
        handoff_columns("h")
    ))
        .bind(principal.tenant_id).bind(query.status).bind(cursor_time).bind(cursor_id)
        .bind(direction).bind(principal.principal_id).bind(limit).fetch_all(&mut *tx).await?;
    let next_cursor = if records.len() == limit as usize {
        records
            .last()
            .map(|record| encode_handoff_cursor(record.created_at, record.id))
    } else {
        None
    };
    let mut items = Vec::with_capacity(records.len());
    for record in records {
        items.push(render_handoff(&mut tx, &principal, record).await?);
    }
    audit(
        &mut tx,
        &principal,
        "handoff.listed",
        "handoff",
        None,
        "allowed",
    )
    .await?;
    tx.commit().await?;
    Ok(Json(HandoffListResponse { items, next_cursor }))
}

fn encode_handoff_cursor(created_at: DateTime<Utc>, id: Uuid) -> String {
    URL_SAFE_NO_PAD.encode(format!("{}|{}", created_at.to_rfc3339(), id))
}

fn decode_handoff_cursor(value: Option<&str>) -> AppResult<(Option<DateTime<Utc>>, Option<Uuid>)> {
    let Some(value) = value else {
        return Ok((None, None));
    };
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| AppError::invalid("invalid cursor"))?;
    let decoded = std::str::from_utf8(&decoded).map_err(|_| AppError::invalid("invalid cursor"))?;
    let (timestamp, id) = decoded
        .split_once('|')
        .ok_or_else(|| AppError::invalid("invalid cursor"))?;
    let timestamp = DateTime::parse_from_rfc3339(timestamp)
        .map_err(|_| AppError::invalid("invalid cursor"))?
        .with_timezone(&Utc);
    let id = Uuid::parse_str(id).map_err(|_| AppError::invalid("invalid cursor"))?;
    Ok((Some(timestamp), Some(id)))
}

async fn get_handoff(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    ApiPath(id): ApiPath<Uuid>,
) -> AppResult<Json<HandoffResponse>> {
    let mut tx = begin_tenant(&state.pool, &principal).await?;
    let record = fetch_handoff(&mut tx, &principal, id).await?;
    let response = render_handoff(&mut tx, &principal, record).await?;
    audit(
        &mut tx,
        &principal,
        "handoff.read",
        "handoff",
        Some(id),
        "allowed",
    )
    .await?;
    tx.commit().await?;
    Ok(Json(response))
}

async fn claim_handoff(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(body): ApiJson<ClaimHandoffRequest>,
) -> AppResult<Json<HandoffResponse>> {
    claim_handoff_inner(&state, &principal, id, body).await
}

async fn claim_handoff_from_openai(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(call): ApiJson<OpenAiFunctionCall>,
) -> AppResult<Json<HandoffResponse>> {
    claim_handoff_inner(&state, &principal, id, openai_claim(call)?).await
}

async fn claim_handoff_from_anthropic(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(call): ApiJson<AnthropicToolUse>,
) -> AppResult<Json<HandoffResponse>> {
    claim_handoff_inner(&state, &principal, id, anthropic_claim(call)?).await
}

async fn claim_handoff_inner(
    state: &AppState,
    principal: &PrincipalContext,
    id: Uuid,
    body: ClaimHandoffRequest,
) -> AppResult<Json<HandoffResponse>> {
    if principal.kind != "agent" {
        return Err(AppError::forbidden());
    }
    if body.version < 1 {
        return Err(AppError::invalid("version must be positive"));
    }
    let lease_seconds = body.lease_seconds.unwrap_or(300);
    if !(30..=3600).contains(&lease_seconds) {
        return Err(AppError::invalid("lease_seconds must be 30-3600"));
    }
    let mut tx = begin_tenant(&state.pool, principal).await?;
    let record = fetch_handoff_for_update(&mut tx, principal, id).await?;
    if record.target_agent_id != principal.principal_id {
        return Err(AppError::not_found());
    }
    let claimable = record.status == "queued"
        || (record.status == "claimed"
            && record
                .lease_expires_at
                .is_some_and(|value| value <= Utc::now()));
    if record.version != body.version || !claimable {
        return Err(AppError::conflict(
            "handoff_conflict",
            "Handoff state or version has changed",
        ));
    }
    let updated = sqlx::query("UPDATE memshare.handoffs SET status='claimed',claimed_at=now(),lease_expires_at=now()+make_interval(secs=>$5),version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 AND target_agent_id=$3 AND version=$4 AND (status='queued' OR (status='claimed' AND lease_expires_at<=now()))")
        .bind(principal.tenant_id)
        .bind(id)
        .bind(principal.principal_id)
        .bind(body.version)
        .bind(i32::try_from(lease_seconds).map_err(|_| AppError::invalid("invalid lease"))?)
        .execute(&mut *tx)
        .await?;
    if updated.rows_affected() != 1 {
        return Err(AppError::conflict(
            "handoff_conflict",
            "Handoff state or version has changed",
        ));
    }
    audit(
        &mut tx,
        principal,
        "handoff.claimed",
        "handoff",
        Some(id),
        "allowed",
    )
    .await?;
    let record = fetch_handoff(&mut tx, principal, id).await?;
    let response = render_handoff(&mut tx, principal, record).await?;
    tx.commit().await?;
    Ok(Json(response))
}

async fn complete_handoff(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(body): ApiJson<CompleteHandoffRequest>,
) -> AppResult<Json<HandoffResponse>> {
    complete_handoff_inner(&state, &principal, id, body).await
}

async fn complete_handoff_from_openai(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(call): ApiJson<OpenAiFunctionCall>,
) -> AppResult<Json<HandoffResponse>> {
    complete_handoff_inner(&state, &principal, id, openai_complete(call)?).await
}

async fn complete_handoff_from_anthropic(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(call): ApiJson<AnthropicToolUse>,
) -> AppResult<Json<HandoffResponse>> {
    let body = anthropic_complete(call)?;
    complete_handoff_inner(&state, &principal, id, body).await
}

async fn complete_handoff_inner(
    state: &AppState,
    principal: &PrincipalContext,
    id: Uuid,
    body: CompleteHandoffRequest,
) -> AppResult<Json<HandoffResponse>> {
    if principal.kind != "agent" {
        return Err(AppError::forbidden());
    }
    validate_handoff_complete(&body)?;
    let mut tx = begin_tenant(&state.pool, principal).await?;
    let record = fetch_handoff_for_update(&mut tx, principal, id).await?;
    if record.target_agent_id != principal.principal_id {
        return Err(AppError::not_found());
    }
    if record.version != body.version
        || record.status != "claimed"
        || record
            .lease_expires_at
            .is_none_or(|value| value <= Utc::now())
    {
        return Err(AppError::conflict(
            "handoff_conflict",
            "Handoff state or version has changed",
        ));
    }
    require_readable_memories(&mut tx, principal, &body.result_memory_ids).await?;
    let result_metadata = verified_model_metadata(principal, &body.source);
    let updated = sqlx::query("UPDATE memshare.handoffs SET status='completed',result=$1,result_memory_ids=$2,result_metadata=$3,completed_at=now(),version=version+1,updated_at=now() WHERE tenant_id=$4 AND id=$5 AND target_agent_id=$6 AND status='claimed' AND version=$7")
        .bind(serde_json::to_value(body.result).map_err(|_| AppError::invalid("result cannot be encoded"))?)
        .bind(body.result_memory_ids)
        .bind(result_metadata)
        .bind(principal.tenant_id)
        .bind(id)
        .bind(principal.principal_id)
        .bind(body.version)
        .execute(&mut *tx)
        .await?;
    if updated.rows_affected() != 1 {
        return Err(AppError::conflict(
            "handoff_conflict",
            "Handoff state or version has changed",
        ));
    }
    audit(
        &mut tx,
        principal,
        "handoff.completed",
        "handoff",
        Some(id),
        "allowed",
    )
    .await?;
    let record = fetch_handoff(&mut tx, principal, id).await?;
    let response = render_handoff(&mut tx, principal, record).await?;
    tx.commit().await?;
    Ok(Json(response))
}

async fn release_handoff(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(body): ApiJson<TransitionHandoffRequest>,
) -> AppResult<Json<HandoffResponse>> {
    if principal.kind != "agent" {
        return Err(AppError::forbidden());
    }
    if body.version < 1 {
        return Err(AppError::invalid("version must be positive"));
    }
    let mut tx = begin_tenant(&state.pool, &principal).await?;
    let record = fetch_handoff_for_update(&mut tx, &principal, id).await?;
    if record.target_agent_id != principal.principal_id {
        return Err(AppError::not_found());
    }
    if record.version != body.version || record.status != "claimed" {
        return Err(AppError::conflict(
            "handoff_conflict",
            "Handoff state or version has changed",
        ));
    }
    sqlx::query("UPDATE memshare.handoffs SET status='queued',claimed_at=NULL,lease_expires_at=NULL,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 AND target_agent_id=$3 AND status='claimed' AND version=$4")
        .bind(principal.tenant_id).bind(id).bind(principal.principal_id).bind(body.version).execute(&mut *tx).await?;
    audit(
        &mut tx,
        &principal,
        "handoff.released",
        "handoff",
        Some(id),
        "allowed",
    )
    .await?;
    let record = fetch_handoff(&mut tx, &principal, id).await?;
    let response = render_handoff(&mut tx, &principal, record).await?;
    tx.commit().await?;
    Ok(Json(response))
}

async fn cancel_handoff(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(body): ApiJson<TransitionHandoffRequest>,
) -> AppResult<Json<HandoffResponse>> {
    if principal.kind != "agent" {
        return Err(AppError::forbidden());
    }
    if body.version < 1 {
        return Err(AppError::invalid("version must be positive"));
    }
    let mut tx = begin_tenant(&state.pool, &principal).await?;
    let record = fetch_handoff_for_update(&mut tx, &principal, id).await?;
    if record.source_agent_id != principal.principal_id {
        return Err(AppError::not_found());
    }
    if record.version != body.version || !matches!(record.status.as_str(), "queued" | "claimed") {
        return Err(AppError::conflict(
            "handoff_conflict",
            "Handoff state or version has changed",
        ));
    }
    sqlx::query("UPDATE memshare.handoffs SET status='cancelled',lease_expires_at=NULL,cancelled_at=now(),version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 AND source_agent_id=$3 AND status IN ('queued','claimed') AND version=$4")
        .bind(principal.tenant_id).bind(id).bind(principal.principal_id).bind(body.version).execute(&mut *tx).await?;
    audit(
        &mut tx,
        &principal,
        "handoff.cancelled",
        "handoff",
        Some(id),
        "allowed",
    )
    .await?;
    let record = fetch_handoff(&mut tx, &principal, id).await?;
    let response = render_handoff(&mut tx, &principal, record).await?;
    tx.commit().await?;
    Ok(Json(response))
}

async fn fetch_handoff(
    tx: &mut Transaction<'_, Postgres>,
    principal: &PrincipalContext,
    id: Uuid,
) -> AppResult<HandoffRecord> {
    sqlx::query_as::<_, HandoffRecord>(&format!("SELECT {} FROM memshare.handoffs h WHERE h.tenant_id=$1 AND h.id=$2 AND (h.source_agent_id=$3 OR h.target_agent_id=$3) AND (h.expires_at IS NULL OR h.expires_at>now()) AND EXISTS(SELECT 1 FROM memshare.workspace_memberships wm WHERE wm.tenant_id=h.tenant_id AND wm.workspace_id=h.workspace_id AND wm.principal_id=$3 AND wm.status='active')", handoff_columns("h")))
        .bind(principal.tenant_id)
        .bind(id)
        .bind(principal.principal_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(AppError::not_found)
}

async fn fetch_handoff_for_update(
    tx: &mut Transaction<'_, Postgres>,
    principal: &PrincipalContext,
    id: Uuid,
) -> AppResult<HandoffRecord> {
    sqlx::query_as::<_, HandoffRecord>(&format!("SELECT {} FROM memshare.handoffs h WHERE h.tenant_id=$1 AND h.id=$2 AND (h.source_agent_id=$3 OR h.target_agent_id=$3) AND (h.expires_at IS NULL OR h.expires_at>now()) AND EXISTS(SELECT 1 FROM memshare.workspace_memberships wm WHERE wm.tenant_id=h.tenant_id AND wm.workspace_id=h.workspace_id AND wm.principal_id=$3 AND wm.status='active') FOR UPDATE OF h", handoff_columns("h")))
        .bind(principal.tenant_id)
        .bind(id)
        .bind(principal.principal_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(AppError::not_found)
}

async fn render_handoff(
    tx: &mut Transaction<'_, Postgres>,
    principal: &PrincipalContext,
    record: HandoffRecord,
) -> AppResult<HandoffResponse> {
    let input_memory_ids = visible_memory_ids(tx, principal, &record.input_memory_ids).await?;
    let result_memory_ids = visible_memory_ids(tx, principal, &record.result_memory_ids).await?;
    Ok(HandoffResponse {
        id: record.id,
        workspace_id: record.workspace_id,
        source_agent_id: record.source_agent_id,
        target_agent_id: record.target_agent_id,
        status: record.status,
        task: record.task,
        omitted_input_references: record.input_memory_ids.len() - input_memory_ids.len(),
        input_memory_ids,
        result: record.result,
        omitted_result_references: record.result_memory_ids.len() - result_memory_ids.len(),
        result_memory_ids,
        source_metadata: record.source_metadata,
        result_metadata: record.result_metadata,
        version: record.version,
        expires_at: record.expires_at,
        claimed_at: record.claimed_at,
        lease_expires_at: record.lease_expires_at,
        completed_at: record.completed_at,
        cancelled_at: record.cancelled_at,
        created_at: record.created_at,
        updated_at: record.updated_at,
    })
}

async fn visible_memory_ids(
    tx: &mut Transaction<'_, Postgres>,
    principal: &PrincipalContext,
    ids: &[Uuid],
) -> AppResult<Vec<Uuid>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<Uuid> = sqlx::query_scalar(AUTHORIZED_MEMORY_IDS)
        .bind(principal.tenant_id)
        .bind(principal.principal_id)
        .bind(ids)
        .fetch_all(&mut **tx)
        .await?;
    let readable: std::collections::HashSet<Uuid> = rows.into_iter().collect();
    Ok(ids
        .iter()
        .copied()
        .filter(|id| readable.contains(id))
        .collect())
}

async fn require_readable_memories(
    tx: &mut Transaction<'_, Postgres>,
    principal: &PrincipalContext,
    ids: &[Uuid],
) -> AppResult<()> {
    let visible = visible_memory_ids(tx, principal, ids).await?;
    if visible.len() == ids.len() {
        Ok(())
    } else {
        Err(AppError::not_found())
    }
}

fn handoff_columns(alias: &str) -> String {
    [
        "id",
        "workspace_id",
        "source_agent_id",
        "target_agent_id",
        "status",
        "task",
        "input_memory_ids",
        "result",
        "result_memory_ids",
        "source_metadata",
        "result_metadata",
        "version",
        "expires_at",
        "claimed_at",
        "lease_expires_at",
        "completed_at",
        "cancelled_at",
        "created_at",
        "updated_at",
    ]
    .iter()
    .map(|column| format!("{alias}.{column}"))
    .collect::<Vec<_>>()
    .join(",")
}

const AUTHORIZED_MEMORY_IDS: &str = r#"
SELECT m.id FROM memshare.memories m
JOIN memshare.principals actor ON actor.tenant_id=m.tenant_id AND actor.id=$2
WHERE m.tenant_id=$1 AND m.id=ANY($3) AND m.deleted_at IS NULL
  AND (m.expires_at IS NULL OR m.expires_at>now()) AND (
    (actor.kind='user' AND m.scope='private' AND m.owner_principal_id=$2)
    OR (actor.kind='user' AND m.scope='workspace' AND EXISTS(
      SELECT 1 FROM memshare.workspace_memberships wm WHERE wm.tenant_id=m.tenant_id AND wm.workspace_id=m.workspace_id AND wm.principal_id=$2 AND wm.status='active'))
    OR (actor.kind='user' AND m.scope='project' AND EXISTS(
      SELECT 1 FROM memshare.project_memberships pm WHERE pm.tenant_id=m.tenant_id AND pm.workspace_id=m.workspace_id AND pm.project_id=m.project_id AND pm.principal_id=$2 AND pm.status='active'))
    OR (EXISTS(SELECT 1 FROM memshare.workspace_memberships wm WHERE wm.tenant_id=m.tenant_id AND wm.workspace_id=m.workspace_id AND wm.principal_id=$2 AND wm.status='active')
      AND EXISTS(SELECT 1 FROM memshare.grants g WHERE g.tenant_id=m.tenant_id AND g.subject_principal_id=$2 AND g.workspace_id=m.workspace_id AND g.scope=m.scope AND g.scope_id=m.scope_id AND 'read'=ANY(g.actions) AND g.revoked_at IS NULL AND (g.expires_at IS NULL OR g.expires_at>now()) AND (g.scope<>'agent' OR EXISTS(SELECT 1 FROM memshare.workspace_memberships target_wm WHERE target_wm.tenant_id=g.tenant_id AND target_wm.workspace_id=g.workspace_id AND target_wm.principal_id=g.scope_id AND target_wm.status='active'))))
  )"#;

fn verified_model_metadata(
    principal: &PrincipalContext,
    source: &ModelIdentity,
) -> serde_json::Value {
    json!({
        "verified": {"actor_principal_id": principal.principal_id, "method": "api"},
        "client": source
    })
}

fn validate_model_identity(identity: &ModelIdentity) -> AppResult<()> {
    let valid = |value: &str| !value.trim().is_empty() && value.len() <= 200;
    if !valid(&identity.provider)
        || !valid(&identity.model)
        || identity
            .framework
            .as_deref()
            .is_some_and(|value| !valid(value))
    {
        return Err(AppError::invalid(
            "provider, model, and framework must contain 1-200 characters",
        ));
    }
    Ok(())
}

fn validate_reference_ids(ids: &[Uuid]) -> AppResult<()> {
    let unique = ids
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    if ids.len() > 100 || unique.len() != ids.len() {
        return Err(AppError::invalid(
            "memory references must be unique and contain at most 100 entries",
        ));
    }
    Ok(())
}

fn validate_json_object(value: &serde_json::Value, field: &str) -> AppResult<()> {
    let size = serde_json::to_vec(value)
        .map_err(|_| AppError::invalid(format!("{field} cannot be encoded")))?
        .len();
    if !value.is_object() || size > 65_536 {
        return Err(AppError::invalid(format!(
            "{field} must be a JSON object no larger than 64 KiB"
        )));
    }
    Ok(())
}

fn validate_handoff_create(body: &CreateHandoffRequest) -> AppResult<()> {
    validate_protocol_text(&body.task.protocol_version, "task.protocol_version", 32)?;
    validate_handoff_protocol(&body.task.protocol_version, "task.protocol_version")?;
    validate_protocol_text(&body.task.summary, "task.summary", 2000)?;
    if body
        .task
        .instructions
        .as_deref()
        .is_some_and(|value| value.len() > 20_000)
    {
        return Err(AppError::invalid(
            "task.instructions must be at most 20000 characters",
        ));
    }
    validate_json_object(&body.task.data, "task.data")?;
    if serde_json::to_vec(&body.task)
        .map_err(|_| AppError::invalid("task cannot be encoded"))?
        .len()
        > 65_536
    {
        return Err(AppError::invalid("task must be no larger than 64 KiB"));
    }
    validate_reference_ids(&body.input_memory_ids)?;
    validate_model_identity(&body.source)?;
    if body.expires_at.is_some_and(|value| value <= Utc::now()) {
        return Err(AppError::invalid("expires_at must be in the future"));
    }
    Ok(())
}

fn validate_handoff_complete(body: &CompleteHandoffRequest) -> AppResult<()> {
    if body.version < 1 {
        return Err(AppError::invalid("version must be positive"));
    }
    validate_protocol_text(&body.result.protocol_version, "result.protocol_version", 32)?;
    validate_handoff_protocol(&body.result.protocol_version, "result.protocol_version")?;
    validate_protocol_text(&body.result.summary, "result.summary", 2000)?;
    if !matches!(body.result.outcome.as_str(), "succeeded" | "failed") {
        return Err(AppError::invalid(
            "result.outcome must be succeeded or failed",
        ));
    }
    validate_json_object(&body.result.data, "result.data")?;
    if serde_json::to_vec(&body.result)
        .map_err(|_| AppError::invalid("result cannot be encoded"))?
        .len()
        > 65_536
    {
        return Err(AppError::invalid("result must be no larger than 64 KiB"));
    }
    validate_reference_ids(&body.result_memory_ids)?;
    validate_model_identity(&body.source)
}

fn validate_protocol_text(value: &str, field: &str, max: usize) -> AppResult<()> {
    if value.trim().is_empty() || value.len() > max {
        Err(AppError::invalid(format!(
            "{field} must contain 1-{max} characters"
        )))
    } else {
        Ok(())
    }
}

fn validate_handoff_protocol(value: &str, field: &str) -> AppResult<()> {
    if value == "1.0" {
        Ok(())
    } else {
        Err(AppError::invalid(format!("{field} must be 1.0")))
    }
}

#[derive(Deserialize)]
struct ListQuery {
    q: Option<String>,
    limit: Option<i64>,
}

async fn list_memories(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    ApiQuery(query): ApiQuery<ListQuery>,
) -> AppResult<Json<ListResponse<MemoryResponse>>> {
    let limit = query.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(AppError::invalid("limit must be 1-100"));
    }
    let q = query.q.unwrap_or_default();
    if q.len() > 500 {
        return Err(AppError::invalid("q must be at most 500 characters"));
    }
    let mut tx = begin_tenant(&state.pool, &principal).await?;
    let items = sqlx::query_as::<_, MemoryResponse>(&format!(
        "{} ORDER BY m.updated_at DESC,m.id LIMIT $4",
        authorized_memory_select()
    ))
    .bind(principal.tenant_id)
    .bind(principal.principal_id)
    .bind(q)
    .bind(limit)
    .fetch_all(&mut *tx)
    .await?;
    audit(
        &mut tx,
        &principal,
        "memory.listed",
        "memory",
        None,
        "allowed",
    )
    .await?;
    tx.commit().await?;
    Ok(Json(ListResponse { items }))
}

async fn get_memory(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    ApiPath(id): ApiPath<Uuid>,
) -> AppResult<Json<MemoryResponse>> {
    let mut tx = begin_tenant(&state.pool, &principal).await?;
    let value = fetch_authorized_memory(&mut tx, &principal, id, "read").await?;
    audit(
        &mut tx,
        &principal,
        "memory.read",
        "memory",
        Some(id),
        "allowed",
    )
    .await?;
    tx.commit().await?;
    Ok(Json(value))
}

async fn update_memory(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(body): ApiJson<UpdateMemoryRequest>,
) -> AppResult<Json<MemoryResponse>> {
    if body.version < 1 || body.content.trim().is_empty() || body.content.len() > 50000 {
        return Err(AppError::invalid("invalid version or content"));
    }
    let mut tx = begin_tenant(&state.pool, &principal).await?;
    let current = fetch_authorized_memory_for_update(&mut tx, &principal, id, "write").await?;
    if current.version != body.version {
        return Err(AppError::conflict(
            "version_conflict",
            "Memory version has changed",
        ));
    }
    sqlx::query("INSERT INTO memshare.memory_versions(tenant_id,memory_id,version,content,changed_by) VALUES ($1,$2,$3,$4,$5)")
        .bind(principal.tenant_id).bind(id).bind(current.version).bind(&current.content).bind(principal.principal_id).execute(&mut *tx).await?;
    let updated = sqlx::query("UPDATE memshare.memories SET content=$1,version=version+1,updated_at=now() WHERE tenant_id=$2 AND id=$3 AND version=$4")
        .bind(body.content.trim()).bind(principal.tenant_id).bind(id).bind(body.version).execute(&mut *tx).await?;
    if updated.rows_affected() != 1 {
        return Err(AppError::conflict(
            "version_conflict",
            "Memory version has changed",
        ));
    }
    audit(
        &mut tx,
        &principal,
        "memory.updated",
        "memory",
        Some(id),
        "allowed",
    )
    .await?;
    let value = fetch_authorized_memory(&mut tx, &principal, id, "read").await?;
    tx.commit().await?;
    Ok(Json(value))
}

#[derive(Deserialize)]
struct DeleteQuery {
    version: i32,
}
async fn delete_memory(
    State(state): State<AppState>,
    Extension(principal): Extension<PrincipalContext>,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<DeleteQuery>,
) -> AppResult<Json<StatusResponse>> {
    let mut tx = begin_tenant(&state.pool, &principal).await?;
    let current = fetch_authorized_memory_for_update(&mut tx, &principal, id, "delete").await?;
    if current.version != query.version {
        return Err(AppError::conflict(
            "version_conflict",
            "Memory version has changed",
        ));
    }
    sqlx::query("INSERT INTO memshare.memory_versions(tenant_id,memory_id,version,content,changed_by) VALUES ($1,$2,$3,$4,$5)")
        .bind(principal.tenant_id).bind(id).bind(current.version).bind(current.content).bind(principal.principal_id).execute(&mut *tx).await?;
    let deleted = sqlx::query("UPDATE memshare.memories SET deleted_at=now(),version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 AND version=$3")
        .bind(principal.tenant_id).bind(id).bind(query.version).execute(&mut *tx).await?;
    if deleted.rows_affected() != 1 {
        return Err(AppError::conflict(
            "version_conflict",
            "Memory version has changed",
        ));
    }
    audit(
        &mut tx,
        &principal,
        "memory.deleted",
        "memory",
        Some(id),
        "allowed",
    )
    .await?;
    tx.commit().await?;
    Ok(Json(StatusResponse { status: "deleted" }))
}

fn authorized_memory_select() -> &'static str {
    r#"
SELECT m.id,m.workspace_id,m.project_id,m.scope,m.scope_id,m.owner_principal_id,
       m.creator_principal_id,m.kind,m.content,m.source,m.version,m.created_at,m.updated_at
FROM memshare.memories m
JOIN memshare.principals actor ON actor.tenant_id=m.tenant_id AND actor.id=$2
WHERE m.tenant_id=$1 AND m.deleted_at IS NULL AND (m.expires_at IS NULL OR m.expires_at>now())
  AND strpos(lower(m.content),lower($3))>0
  AND (
    (actor.kind='user' AND m.scope='private' AND m.owner_principal_id=$2)
    OR (actor.kind='user' AND m.scope='workspace' AND EXISTS(
      SELECT 1 FROM memshare.workspace_memberships wm WHERE wm.tenant_id=m.tenant_id AND wm.workspace_id=m.workspace_id AND wm.principal_id=$2 AND wm.status='active'))
    OR (actor.kind='user' AND m.scope='project' AND EXISTS(
      SELECT 1 FROM memshare.project_memberships pm WHERE pm.tenant_id=m.tenant_id AND pm.workspace_id=m.workspace_id AND pm.project_id=m.project_id AND pm.principal_id=$2 AND pm.status='active'))
    OR (EXISTS(SELECT 1 FROM memshare.workspace_memberships wm WHERE wm.tenant_id=m.tenant_id AND wm.workspace_id=m.workspace_id AND wm.principal_id=$2 AND wm.status='active')
      AND EXISTS(SELECT 1 FROM memshare.grants g WHERE g.tenant_id=m.tenant_id AND g.subject_principal_id=$2 AND g.workspace_id=m.workspace_id AND g.scope=m.scope AND g.scope_id=m.scope_id AND 'read'=ANY(g.actions) AND g.revoked_at IS NULL AND (g.expires_at IS NULL OR g.expires_at>now()) AND (g.scope<>'agent' OR EXISTS(SELECT 1 FROM memshare.workspace_memberships target_wm WHERE target_wm.tenant_id=g.tenant_id AND target_wm.workspace_id=g.workspace_id AND target_wm.principal_id=g.scope_id AND target_wm.status='active'))))
  )"#
}

async fn fetch_authorized_memory(
    tx: &mut Transaction<'_, Postgres>,
    p: &PrincipalContext,
    id: Uuid,
    action: &str,
) -> AppResult<MemoryResponse> {
    let value=sqlx::query_as::<_,MemoryResponse>(r#"SELECT m.id,m.workspace_id,m.project_id,m.scope,m.scope_id,m.owner_principal_id,m.creator_principal_id,m.kind,m.content,m.source,m.version,m.created_at,m.updated_at FROM memshare.memories m JOIN memshare.principals actor ON actor.tenant_id=m.tenant_id AND actor.id=$2 WHERE m.tenant_id=$1 AND m.id=$3 AND m.deleted_at IS NULL AND (m.expires_at IS NULL OR m.expires_at>now()) AND ((actor.kind='user' AND m.scope='private' AND m.owner_principal_id=$2) OR (actor.kind='user' AND m.scope='workspace' AND EXISTS(SELECT 1 FROM memshare.workspace_memberships wm WHERE wm.tenant_id=m.tenant_id AND wm.workspace_id=m.workspace_id AND wm.principal_id=$2 AND wm.status='active' AND ($4='read' OR wm.role='admin' OR m.creator_principal_id=$2))) OR (actor.kind='user' AND m.scope='project' AND EXISTS(SELECT 1 FROM memshare.project_memberships pm WHERE pm.tenant_id=m.tenant_id AND pm.workspace_id=m.workspace_id AND pm.project_id=m.project_id AND pm.principal_id=$2 AND pm.status='active' AND ($4='read' OR pm.role='admin' OR m.creator_principal_id=$2))) OR (EXISTS(SELECT 1 FROM memshare.workspace_memberships wm WHERE wm.tenant_id=m.tenant_id AND wm.workspace_id=m.workspace_id AND wm.principal_id=$2 AND wm.status='active') AND EXISTS(SELECT 1 FROM memshare.grants g WHERE g.tenant_id=m.tenant_id AND g.subject_principal_id=$2 AND g.workspace_id=m.workspace_id AND g.scope=m.scope AND g.scope_id=m.scope_id AND $4=ANY(g.actions) AND g.revoked_at IS NULL AND (g.expires_at IS NULL OR g.expires_at>now()) AND (g.scope<>'agent' OR EXISTS(SELECT 1 FROM memshare.workspace_memberships target_wm WHERE target_wm.tenant_id=g.tenant_id AND target_wm.workspace_id=g.workspace_id AND target_wm.principal_id=g.scope_id AND target_wm.status='active')))))"#)
        .bind(p.tenant_id).bind(p.principal_id).bind(id).bind(action).fetch_optional(&mut **tx).await?;
    value.ok_or_else(AppError::not_found)
}

async fn fetch_authorized_memory_for_update(
    tx: &mut Transaction<'_, Postgres>,
    p: &PrincipalContext,
    id: Uuid,
    action: &str,
) -> AppResult<MemoryResponse> {
    let value=sqlx::query_as::<_,MemoryResponse>(r#"SELECT m.id,m.workspace_id,m.project_id,m.scope,m.scope_id,m.owner_principal_id,m.creator_principal_id,m.kind,m.content,m.source,m.version,m.created_at,m.updated_at FROM memshare.memories m JOIN memshare.principals actor ON actor.tenant_id=m.tenant_id AND actor.id=$2 WHERE m.tenant_id=$1 AND m.id=$3 AND m.deleted_at IS NULL AND (m.expires_at IS NULL OR m.expires_at>now()) AND ((actor.kind='user' AND m.scope='private' AND m.owner_principal_id=$2) OR (actor.kind='user' AND m.scope='workspace' AND EXISTS(SELECT 1 FROM memshare.workspace_memberships wm WHERE wm.tenant_id=m.tenant_id AND wm.workspace_id=m.workspace_id AND wm.principal_id=$2 AND wm.status='active' AND ($4='read' OR wm.role='admin' OR m.creator_principal_id=$2))) OR (actor.kind='user' AND m.scope='project' AND EXISTS(SELECT 1 FROM memshare.project_memberships pm WHERE pm.tenant_id=m.tenant_id AND pm.workspace_id=m.workspace_id AND pm.project_id=m.project_id AND pm.principal_id=$2 AND pm.status='active' AND ($4='read' OR pm.role='admin' OR m.creator_principal_id=$2))) OR (EXISTS(SELECT 1 FROM memshare.workspace_memberships wm WHERE wm.tenant_id=m.tenant_id AND wm.workspace_id=m.workspace_id AND wm.principal_id=$2 AND wm.status='active') AND EXISTS(SELECT 1 FROM memshare.grants g WHERE g.tenant_id=m.tenant_id AND g.subject_principal_id=$2 AND g.workspace_id=m.workspace_id AND g.scope=m.scope AND g.scope_id=m.scope_id AND $4=ANY(g.actions) AND g.revoked_at IS NULL AND (g.expires_at IS NULL OR g.expires_at>now()) AND (g.scope<>'agent' OR EXISTS(SELECT 1 FROM memshare.workspace_memberships target_wm WHERE target_wm.tenant_id=g.tenant_id AND target_wm.workspace_id=g.workspace_id AND target_wm.principal_id=g.scope_id AND target_wm.status='active'))))) FOR UPDATE OF m"#)
        .bind(p.tenant_id).bind(p.principal_id).bind(id).bind(action).fetch_optional(&mut **tx).await?;
    value.ok_or_else(AppError::not_found)
}

async fn authorize_scope(
    tx: &mut Transaction<'_, Postgres>,
    p: &PrincipalContext,
    b: &CreateMemoryRequest,
    action: &str,
) -> AppResult<()> {
    let allowed = match b.scope.as_str() {
        "private" => {
            p.kind == "user"
                && b.scope_id == p.principal_id
                && workspace_member(tx, p.tenant_id, b.workspace_id, p.principal_id).await?
        }
        "workspace" if b.scope_id == b.workspace_id => {
            if p.kind == "user" {
                workspace_member(tx, p.tenant_id, b.workspace_id, p.principal_id).await?
            } else {
                has_grant(tx, p, b.workspace_id, &b.scope, b.scope_id, action).await?
            }
        }
        "project" if b.project_id == Some(b.scope_id) => {
            if p.kind == "user" {
                project_member(tx, p.tenant_id, b.workspace_id, b.scope_id, p.principal_id).await?
            } else {
                has_grant(tx, p, b.workspace_id, &b.scope, b.scope_id, action).await?
            }
        }
        "agent" if b.scope_id == p.principal_id => {
            has_grant(tx, p, b.workspace_id, &b.scope, b.scope_id, action).await?
        }
        _ => false,
    };
    if allowed {
        Ok(())
    } else {
        Err(AppError::forbidden())
    }
}

async fn authorize_grant_issuer(
    tx: &mut Transaction<'_, Postgres>,
    p: &PrincipalContext,
    workspace_id: Uuid,
    b: &CreateGrantRequest,
) -> AppResult<()> {
    let allowed = match b.scope.as_str() {
        "private" => b.scope_id == p.principal_id,
        "workspace" => {
            b.scope_id == workspace_id
                && workspace_admin(tx, p.tenant_id, workspace_id, p.principal_id).await?
        }
        "project" => {
            project_admin(tx, p.tenant_id, workspace_id, b.scope_id, p.principal_id).await?
        }
        "agent" => {
            let owner:Option<Uuid>=sqlx::query_scalar("SELECT owner_principal_id FROM memshare.principals WHERE tenant_id=$1 AND id=$2 AND kind='agent' AND status='active'")
                .bind(p.tenant_id).bind(b.scope_id).fetch_optional(&mut **tx).await?;
            owner == Some(p.principal_id)
        }
        _ => false,
    };
    if allowed {
        Ok(())
    } else {
        Err(AppError::forbidden())
    }
}

fn validate_display_name(value: &str) -> AppResult<()> {
    if value.trim().is_empty() || value.len() > 200 {
        Err(AppError::invalid("name must contain 1-200 characters"))
    } else {
        Ok(())
    }
}

fn validate_slug(value: &str) -> AppResult<()> {
    if value.is_empty()
        || value.len() > 100
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        Err(AppError::invalid(
            "slug must contain 1-100 ASCII letters, digits, dots, underscores, or hyphens",
        ))
    } else {
        Ok(())
    }
}

fn validate_role(value: &str) -> AppResult<()> {
    if matches!(value, "admin" | "member") {
        Ok(())
    } else {
        Err(AppError::invalid("role must be admin or member"))
    }
}

fn validate_future(value: Option<DateTime<Utc>>) -> AppResult<()> {
    if value.is_some_and(|expires_at| expires_at <= Utc::now()) {
        Err(AppError::invalid("expires_at must be in the future"))
    } else {
        Ok(())
    }
}

async fn require_tenant_admin(
    tx: &mut Transaction<'_, Postgres>,
    actor: &PrincipalContext,
) -> AppResult<()> {
    let allowed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memshare.principals WHERE tenant_id=$1 AND id=$2 AND kind='user' AND status='active' AND tenant_role='admin')")
        .bind(actor.tenant_id).bind(actor.principal_id).fetch_one(&mut **tx).await?;
    if allowed {
        Ok(())
    } else {
        Err(AppError::forbidden())
    }
}

async fn require_active_principal(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    principal_id: Uuid,
) -> AppResult<()> {
    let active: Option<bool> = sqlx::query_scalar(
        "SELECT status='active' FROM memshare.principals WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
    )
    .bind(tenant_id)
    .bind(principal_id)
    .fetch_optional(&mut **tx)
    .await?;
    if active == Some(true) {
        Ok(())
    } else {
        Err(AppError::not_found())
    }
}

async fn workspace_member(
    tx: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
    workspace: Uuid,
    principal: Uuid,
) -> AppResult<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memshare.workspace_memberships WHERE tenant_id=$1 AND workspace_id=$2 AND principal_id=$3 AND status='active')").bind(tenant).bind(workspace).bind(principal).fetch_one(&mut **tx).await?)
}
async fn workspace_admin(
    tx: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
    workspace: Uuid,
    principal: Uuid,
) -> AppResult<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memshare.workspace_memberships WHERE tenant_id=$1 AND workspace_id=$2 AND principal_id=$3 AND status='active' AND role='admin')").bind(tenant).bind(workspace).bind(principal).fetch_one(&mut **tx).await?)
}
async fn require_workspace_role(
    tx: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
    workspace: Uuid,
    principal: Uuid,
    role: &str,
) -> AppResult<()> {
    let ok:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memshare.workspace_memberships WHERE tenant_id=$1 AND workspace_id=$2 AND principal_id=$3 AND status='active' AND role=$4)").bind(tenant).bind(workspace).bind(principal).bind(role).fetch_one(&mut **tx).await?;
    if ok {
        Ok(())
    } else {
        Err(AppError::forbidden())
    }
}
async fn project_member(
    tx: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
    workspace: Uuid,
    project: Uuid,
    principal: Uuid,
) -> AppResult<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memshare.project_memberships WHERE tenant_id=$1 AND workspace_id=$2 AND project_id=$3 AND principal_id=$4 AND status='active')").bind(tenant).bind(workspace).bind(project).bind(principal).fetch_one(&mut **tx).await?)
}
async fn project_admin(
    tx: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
    workspace: Uuid,
    project: Uuid,
    principal: Uuid,
) -> AppResult<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memshare.project_memberships WHERE tenant_id=$1 AND workspace_id=$2 AND project_id=$3 AND principal_id=$4 AND status='active' AND role='admin')").bind(tenant).bind(workspace).bind(project).bind(principal).fetch_one(&mut **tx).await?)
}
async fn has_grant(
    tx: &mut Transaction<'_, Postgres>,
    p: &PrincipalContext,
    workspace: Uuid,
    scope: &str,
    scope_id: Uuid,
    action: &str,
) -> AppResult<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memshare.grants g WHERE g.tenant_id=$1 AND g.subject_principal_id=$2 AND g.workspace_id=$3 AND g.scope=$4 AND g.scope_id=$5 AND $6=ANY(g.actions) AND g.revoked_at IS NULL AND (g.expires_at IS NULL OR g.expires_at>now()) AND EXISTS(SELECT 1 FROM memshare.workspace_memberships wm WHERE wm.tenant_id=g.tenant_id AND wm.workspace_id=g.workspace_id AND wm.principal_id=g.subject_principal_id AND wm.status='active') AND (g.scope<>'agent' OR EXISTS(SELECT 1 FROM memshare.workspace_memberships target_wm WHERE target_wm.tenant_id=g.tenant_id AND target_wm.workspace_id=g.workspace_id AND target_wm.principal_id=g.scope_id AND target_wm.status='active')))").bind(p.tenant_id).bind(p.principal_id).bind(workspace).bind(scope).bind(scope_id).bind(action).fetch_one(&mut **tx).await?)
}

async fn audit(
    tx: &mut Transaction<'_, Postgres>,
    p: &PrincipalContext,
    action: &str,
    target_type: &str,
    target_id: Option<Uuid>,
    outcome: &str,
) -> AppResult<()> {
    sqlx::query("INSERT INTO memshare.audit_events(tenant_id,id,actor_principal_id,action,outcome,target_type,target_id) VALUES ($1,$2,$3,$4,$5,$6,$7)").bind(p.tenant_id).bind(Uuid::new_v4()).bind(p.principal_id).bind(action).bind(outcome).bind(target_type).bind(target_id).execute(&mut **tx).await?;
    Ok(())
}

fn validate_actions(actions: &[String]) -> AppResult<()> {
    if actions.is_empty()
        || actions.iter().any(|a| {
            !matches!(
                a.as_str(),
                "read" | "write" | "delete" | "review" | "handoff"
            )
        })
    {
        Err(AppError::invalid(
            "actions must contain read, write, delete, review, or handoff",
        ))
    } else {
        Ok(())
    }
}
fn validate_memory(b: &CreateMemoryRequest) -> AppResult<()> {
    if !matches!(
        b.scope.as_str(),
        "private" | "workspace" | "project" | "agent"
    ) || !matches!(
        b.kind.as_str(),
        "preference" | "fact" | "experience" | "task_state" | "unverified"
    ) || b.content.trim().is_empty()
        || b.content.len() > 50000
    {
        Err(AppError::invalid("invalid memory scope, kind, or content"))
    } else if serde_json::to_vec(&b.source)
        .map_err(|_| AppError::invalid("source cannot be encoded"))?
        .len()
        > 16_384
    {
        Err(AppError::invalid("source must be at most 16 KiB"))
    } else if b.expires_at.is_some_and(|v| v <= Utc::now()) {
        Err(AppError::invalid("expires_at must be in the future"))
    } else {
        Ok(())
    }
}
fn idempotency_key(headers: &HeaderMap) -> AppResult<String> {
    let value = headers
        .get("Idempotency-Key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::invalid("Idempotency-Key is required"))?;
    if value.trim().is_empty() || value.len() > 200 {
        Err(AppError::invalid(
            "Idempotency-Key must contain 1-200 characters",
        ))
    } else {
        Ok(value.to_owned())
    }
}
fn canonical_hash<T: serde::Serialize>(value: &T) -> AppResult<String> {
    let bytes =
        serde_json::to_vec(value).map_err(|_| AppError::invalid("request cannot be encoded"))?;
    let mut digest = Sha256::new();
    digest.update(bytes);
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actions_are_allowlisted() {
        assert!(validate_actions(&["read".into(), "write".into()]).is_ok());
        assert!(validate_actions(&[]).is_err());
        assert!(validate_actions(&["admin".into()]).is_err());
    }

    #[test]
    fn handoff_cursor_round_trips_and_rejects_garbage() {
        let timestamp = Utc::now();
        let id = Uuid::new_v4();
        let encoded = encode_handoff_cursor(timestamp, id);
        let (decoded_timestamp, decoded_id) = decode_handoff_cursor(Some(&encoded)).unwrap();
        assert_eq!(decoded_timestamp.unwrap(), timestamp);
        assert_eq!(decoded_id, Some(id));
        assert!(decode_handoff_cursor(Some("not-a-cursor")).is_err());
    }
}
