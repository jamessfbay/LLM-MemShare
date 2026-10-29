use axum::{
    extract::{Request, State},
    http::{HeaderName, HeaderValue, StatusCode},
    middleware::Next,
    response::Response,
};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use uuid::Uuid;

use crate::{
    db::{record_request_audit, token_hash},
    error::AppError,
    model::PrincipalContext,
};

type AuthenticatedRequest = (Uuid, Uuid, Uuid, String, bool, i64, i64, DateTime<Utc>);

#[derive(Default)]
pub struct Metrics {
    requests: AtomicU64,
    responses_2xx: AtomicU64,
    responses_4xx: AtomicU64,
    responses_5xx: AtomicU64,
    authentication_failures: AtomicU64,
    rate_limited: AtomicU64,
}

impl Metrics {
    pub fn record_request(&self) {
        self.requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_response(&self, status: StatusCode) {
        let counter = if status.is_success() {
            &self.responses_2xx
        } else if status.is_client_error() {
            &self.responses_4xx
        } else if status.is_server_error() {
            &self.responses_5xx
        } else {
            return;
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_authentication_failure(&self) {
        self.authentication_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_rate_limited(&self) {
        self.rate_limited.fetch_add(1, Ordering::Relaxed);
    }

    pub fn render(&self) -> String {
        format!(
            concat!(
                "# TYPE memshare_http_requests_total counter\n",
                "memshare_http_requests_total {}\n",
                "# TYPE memshare_http_responses_total counter\n",
                "memshare_http_responses_total{{class=\"2xx\"}} {}\n",
                "memshare_http_responses_total{{class=\"4xx\"}} {}\n",
                "memshare_http_responses_total{{class=\"5xx\"}} {}\n",
                "# TYPE memshare_authentication_failures_total counter\n",
                "memshare_authentication_failures_total {}\n",
                "# TYPE memshare_rate_limited_total counter\n",
                "memshare_rate_limited_total {}\n"
            ),
            self.requests.load(Ordering::Relaxed),
            self.responses_2xx.load(Ordering::Relaxed),
            self.responses_4xx.load(Ordering::Relaxed),
            self.responses_5xx.load(Ordering::Relaxed),
            self.authentication_failures.load(Ordering::Relaxed),
            self.rate_limited.load(Ordering::Relaxed),
        )
    }
}

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub metrics: Arc<Metrics>,
    pub public_base_url: Arc<str>,
}

impl AppState {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            metrics: Arc::new(Metrics::default()),
            public_base_url: Arc::from("http://127.0.0.1:8080"),
        }
    }

    pub fn with_public_base_url(mut self, public_base_url: String) -> Self {
        self.public_base_url = Arc::from(public_base_url);
        self
    }
}

pub async fn authenticate(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let method = request.method().to_string();
    let path = request.uri().path().to_owned();
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let header = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            state.metrics.record_authentication_failure();
            AppError::unauthorized()
        })?;
    let token = header
        .strip_prefix("Bearer ")
        .filter(|value| (32..=512).contains(&value.len()))
        .ok_or_else(|| {
            state.metrics.record_authentication_failure();
            AppError::unauthorized()
        })?;
    let row: Option<AuthenticatedRequest> = sqlx::query_as(
        "SELECT credential_id,tenant_id,principal_id,kind,rate_allowed,rate_limit,rate_remaining,rate_reset_at FROM memshare.authenticate_request($1)",
    )
    .bind(token_hash(token))
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::database)?;
    let (credential_id, tenant_id, principal_id, kind, allowed, limit, remaining, reset_at) =
        row.ok_or_else(|| {
            state.metrics.record_authentication_failure();
            AppError::unauthorized()
        })?;
    let principal = PrincipalContext {
        credential_id,
        tenant_id,
        principal_id,
        kind,
        request_id,
    };
    if !allowed {
        state.metrics.record_rate_limited();
        if let Err(error) = record_request_audit(
            &state.pool,
            &principal,
            "http.rate_limited",
            &path,
            "denied",
        )
        .await
        {
            tracing::warn!(error = ?error, "failed to record rate-limit audit event");
        }
        return Err(AppError::rate_limited());
    }
    request.extensions_mut().insert(principal.clone());
    let mut response = next.run(request).await;
    if response.status().is_client_error() || response.status().is_server_error() {
        let outcome = if response.status().is_server_error() {
            "failed"
        } else {
            "denied"
        };
        if let Err(error) = record_request_audit(
            &state.pool,
            &principal,
            &format!("http.{}", method.to_ascii_lowercase()),
            &path,
            outcome,
        )
        .await
        {
            tracing::warn!(error = ?error, "failed to record request audit event");
        }
    }
    let headers = response.headers_mut();
    headers.insert(
        HeaderName::from_static("x-ratelimit-limit"),
        HeaderValue::from_str(&limit.to_string()).expect("integer header"),
    );
    headers.insert(
        HeaderName::from_static("x-ratelimit-remaining"),
        HeaderValue::from_str(&remaining.to_string()).expect("integer header"),
    );
    headers.insert(
        HeaderName::from_static("x-ratelimit-reset"),
        HeaderValue::from_str(&reset_at.timestamp().to_string()).expect("timestamp header"),
    );
    Ok(response)
}

pub async fn observe(State(state): State<AppState>, request: Request, next: Next) -> Response {
    state.metrics.record_request();
    let response = next.run(request).await;
    state.metrics.record_response(response.status());
    response
}
