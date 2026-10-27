use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{db::token_hash, error::AppError, model::PrincipalContext};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
}

pub async fn authenticate(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let header = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(AppError::unauthorized)?;
    let token = header
        .strip_prefix("Bearer ")
        .filter(|value| (32..=512).contains(&value.len()))
        .ok_or_else(AppError::unauthorized)?;
    let row: Option<(Uuid, Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT credential_id,tenant_id,principal_id,kind FROM memshare.authenticate_credential($1)",
    )
    .bind(token_hash(token))
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::database)?;
    let (credential_id, tenant_id, principal_id, kind) = row.ok_or_else(AppError::unauthorized)?;
    request.extensions_mut().insert(PrincipalContext {
        credential_id,
        tenant_id,
        principal_id,
        kind,
    });
    Ok(next.run(request).await)
}
