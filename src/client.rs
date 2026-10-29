use reqwest::{Method, RequestBuilder, StatusCode, Url};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    adapters::{AnthropicToolUse, OpenAiFunctionCall},
    model::{
        ClaimHandoffRequest, CompleteHandoffRequest, CreateHandoffRequest, CreateMemoryRequest,
        CursorListResponse, HandoffListResponse, HandoffResponse, MemoryResponse, StatusResponse,
        TransitionHandoffRequest, UpdateMemoryRequest,
    },
};

#[derive(Clone)]
pub struct MemShareClient {
    http: reqwest::Client,
    base_url: Url,
    token: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("invalid MemShare URL: {0}")]
    InvalidUrl(String),
    #[error("MemShare transport error: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("MemShare API error {status}: {code}: {message}")]
    Api {
        status: StatusCode,
        code: String,
        message: String,
    },
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    code: String,
    message: String,
}

#[derive(Serialize)]
struct MemoryListQuery<'a> {
    q: Option<&'a str>,
    limit: Option<i64>,
    cursor: Option<&'a str>,
}

#[derive(Serialize)]
struct HandoffListQuery<'a> {
    status: Option<&'a str>,
    direction: Option<&'a str>,
    limit: Option<i64>,
    cursor: Option<&'a str>,
}

impl MemShareClient {
    pub fn new(base_url: &str, token: impl Into<String>) -> Result<Self, ClientError> {
        let mut base_url =
            Url::parse(base_url).map_err(|error| ClientError::InvalidUrl(error.to_string()))?;
        if !base_url.path().ends_with('/') {
            base_url.set_path(&format!("{}/", base_url.path()));
        }
        Ok(Self {
            http: reqwest::Client::new(),
            base_url,
            token: token.into(),
        })
    }

    pub fn with_http_client(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    pub async fn create_memory(
        &self,
        idempotency_key: &str,
        request: &CreateMemoryRequest,
    ) -> Result<MemoryResponse, ClientError> {
        self.send(
            self.request(Method::POST, "v1/memories")?
                .header("Idempotency-Key", idempotency_key)
                .json(request),
        )
        .await
    }

    pub async fn list_memories(
        &self,
        query: Option<&str>,
        limit: Option<i64>,
        cursor: Option<&str>,
    ) -> Result<CursorListResponse<MemoryResponse>, ClientError> {
        self.send(
            self.request(Method::GET, "v1/memories")?
                .query(&MemoryListQuery {
                    q: query,
                    limit,
                    cursor,
                }),
        )
        .await
    }

    pub async fn get_memory(&self, id: Uuid) -> Result<MemoryResponse, ClientError> {
        self.send(self.request(Method::GET, &format!("v1/memories/{id}"))?)
            .await
    }

    pub async fn update_memory(
        &self,
        id: Uuid,
        request: &UpdateMemoryRequest,
    ) -> Result<MemoryResponse, ClientError> {
        self.send(
            self.request(Method::PATCH, &format!("v1/memories/{id}"))?
                .json(request),
        )
        .await
    }

    pub async fn delete_memory(
        &self,
        id: Uuid,
        version: i32,
    ) -> Result<StatusResponse, ClientError> {
        self.send(
            self.request(Method::DELETE, &format!("v1/memories/{id}"))?
                .query(&[("version", version)]),
        )
        .await
    }

    pub async fn create_handoff(
        &self,
        idempotency_key: &str,
        request: &CreateHandoffRequest,
    ) -> Result<HandoffResponse, ClientError> {
        self.send(
            self.request(Method::POST, "v1/handoffs")?
                .header("Idempotency-Key", idempotency_key)
                .json(request),
        )
        .await
    }

    pub async fn create_handoff_openai(
        &self,
        idempotency_key: &str,
        call: &OpenAiFunctionCall,
    ) -> Result<HandoffResponse, ClientError> {
        self.send(
            self.request(Method::POST, "v1/adapters/openai/handoffs")?
                .header("Idempotency-Key", idempotency_key)
                .json(call),
        )
        .await
    }

    pub async fn create_handoff_anthropic(
        &self,
        idempotency_key: &str,
        call: &AnthropicToolUse,
    ) -> Result<HandoffResponse, ClientError> {
        self.send(
            self.request(Method::POST, "v1/adapters/anthropic/handoffs")?
                .header("Idempotency-Key", idempotency_key)
                .json(call),
        )
        .await
    }

    pub async fn list_handoffs(
        &self,
        status: Option<&str>,
        direction: Option<&str>,
        limit: Option<i64>,
        cursor: Option<&str>,
    ) -> Result<HandoffListResponse, ClientError> {
        self.send(
            self.request(Method::GET, "v1/handoffs")?
                .query(&HandoffListQuery {
                    status,
                    direction,
                    limit,
                    cursor,
                }),
        )
        .await
    }

    pub async fn get_handoff(&self, id: Uuid) -> Result<HandoffResponse, ClientError> {
        self.send(self.request(Method::GET, &format!("v1/handoffs/{id}"))?)
            .await
    }

    pub async fn claim_handoff(
        &self,
        id: Uuid,
        request: &ClaimHandoffRequest,
    ) -> Result<HandoffResponse, ClientError> {
        self.send(
            self.request(Method::POST, &format!("v1/handoffs/{id}/claim"))?
                .json(request),
        )
        .await
    }

    pub async fn complete_handoff(
        &self,
        id: Uuid,
        request: &CompleteHandoffRequest,
    ) -> Result<HandoffResponse, ClientError> {
        self.send(
            self.request(Method::POST, &format!("v1/handoffs/{id}/complete"))?
                .json(request),
        )
        .await
    }

    pub async fn release_handoff(
        &self,
        id: Uuid,
        version: i32,
    ) -> Result<HandoffResponse, ClientError> {
        self.transition(id, "release", version).await
    }

    pub async fn cancel_handoff(
        &self,
        id: Uuid,
        version: i32,
    ) -> Result<HandoffResponse, ClientError> {
        self.transition(id, "cancel", version).await
    }

    async fn transition(
        &self,
        id: Uuid,
        action: &str,
        version: i32,
    ) -> Result<HandoffResponse, ClientError> {
        self.send(
            self.request(Method::POST, &format!("v1/handoffs/{id}/{action}"))?
                .json(&TransitionHandoffRequest { version }),
        )
        .await
    }

    fn request(&self, method: Method, path: &str) -> Result<RequestBuilder, ClientError> {
        let url = self
            .base_url
            .join(path)
            .map_err(|error| ClientError::InvalidUrl(error.to_string()))?;
        Ok(self.http.request(method, url).bearer_auth(&self.token))
    }

    async fn send<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T, ClientError> {
        let response = request.send().await?;
        let status = response.status();
        if status.is_success() {
            return Ok(response.json().await?);
        }
        let error = response
            .json::<ErrorEnvelope>()
            .await
            .unwrap_or(ErrorEnvelope {
                error: ErrorBody {
                    code: "invalid_error_response".to_owned(),
                    message: "MemShare returned an unreadable error response".to_owned(),
                },
            });
        Err(ClientError::Api {
            status,
            code: error.error.code,
            message: error.error.message,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::to_bytes, extract::Request, http::StatusCode as HttpStatusCode,
        response::IntoResponse, routing::any, Json, Router,
    };
    use chrono::Utc;
    use serde_json::{json, Value};

    #[test]
    fn normalizes_base_url() {
        let client = MemShareClient::new("https://example.test/prefix", "token").unwrap();
        assert_eq!(client.base_url.as_str(), "https://example.test/prefix/");
    }

    async fn mock_memory_api(request: Request) -> axum::response::Response {
        assert_eq!(request.method(), axum::http::Method::POST);
        assert_eq!(request.uri().path(), "/prefix/v1/memories");
        assert_eq!(
            request.headers().get("authorization").unwrap(),
            "Bearer test-token"
        );
        let idempotency_key = request
            .headers()
            .get("idempotency-key")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let body: Value =
            serde_json::from_slice(&to_bytes(request.into_body(), 100_000).await.unwrap()).unwrap();
        assert_eq!(body["content"], "client contract");
        match idempotency_key.as_str() {
            "limited" => (
                HttpStatusCode::TOO_MANY_REQUESTS,
                Json(json!({"error":{"code":"rate_limited","message":"slow down"}})),
            )
                .into_response(),
            "broken" => (HttpStatusCode::INTERNAL_SERVER_ERROR, "not json").into_response(),
            _ => (
                HttpStatusCode::CREATED,
                Json(json!({
                    "id": Uuid::nil(),
                    "workspace_id": body["workspace_id"],
                    "project_id": null,
                    "scope": body["scope"],
                    "scope_id": body["scope_id"],
                    "owner_principal_id": null,
                    "creator_principal_id": Uuid::nil(),
                    "kind": body["kind"],
                    "content": body["content"],
                    "source": {},
                    "version": 1,
                    "created_at": Utc::now(),
                    "updated_at": Utc::now()
                })),
            )
                .into_response(),
        }
    }

    #[tokio::test]
    async fn sends_authenticated_requests_and_decodes_success_and_errors() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().fallback(any(mock_memory_api)))
                .await
                .unwrap();
        });
        let client =
            MemShareClient::new(&format!("http://{address}/prefix"), "test-token").unwrap();
        let workspace_id = Uuid::new_v4();
        let body = CreateMemoryRequest {
            workspace_id,
            project_id: None,
            scope: "workspace".to_owned(),
            scope_id: workspace_id,
            kind: "fact".to_owned(),
            content: "client contract".to_owned(),
            source: json!({}),
            expires_at: None,
        };

        let created = client.create_memory("created", &body).await.unwrap();
        assert_eq!(created.content, "client contract");
        match client.create_memory("limited", &body).await.unwrap_err() {
            ClientError::Api {
                status,
                code,
                message,
            } => {
                assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
                assert_eq!(code, "rate_limited");
                assert_eq!(message, "slow down");
            }
            error => panic!("unexpected error: {error}"),
        }
        match client.create_memory("broken", &body).await.unwrap_err() {
            ClientError::Api { code, .. } => assert_eq!(code, "invalid_error_response"),
            error => panic!("unexpected error: {error}"),
        }
        server.abort();
    }

    #[tokio::test]
    async fn reports_transport_failures() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let client = MemShareClient::new(&format!("http://{address}"), "test-token").unwrap();
        assert!(matches!(
            client.list_memories(None, None, None).await,
            Err(ClientError::Transport(_))
        ));
    }
}
