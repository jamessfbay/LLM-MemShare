use std::{str::FromStr, time::Duration};

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use llm_memshare::{
    auth::AppState,
    db::{issue_token, token_hash},
    migrate, router,
};
use serde_json::{json, Value};
use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions},
    PgPool,
};
use tower::ServiceExt;
use uuid::Uuid;

struct Actor {
    id: Uuid,
    credential_id: Uuid,
    token: String,
}

fn request(method: &str, uri: &str, token: &str, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"));
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .unwrap()
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn insert_actor(
    pool: &PgPool,
    tenant_id: Uuid,
    workspace_id: Uuid,
    kind: &str,
    owner_id: Option<Uuid>,
    name: &str,
) -> Actor {
    let id = Uuid::new_v4();
    let credential_id = Uuid::new_v4();
    let token = issue_token();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('memshare.tenant_id',$1,true),set_config('memshare.principal_id',$2,true)")
        .bind(tenant_id.to_string())
        .bind(id.to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO memshare.principals(tenant_id,id,kind,name,status,tenant_role,owner_principal_id) VALUES ($1,$2,$3,$4,'active',$5,$6)")
        .bind(tenant_id)
        .bind(id)
        .bind(kind)
        .bind(name)
        .bind(if kind == "user" { "admin" } else { "member" })
        .bind(owner_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO memshare.workspace_memberships(tenant_id,workspace_id,principal_id,role,status) VALUES ($1,$2,$3,$4,'active')")
        .bind(tenant_id)
        .bind(workspace_id)
        .bind(id)
        .bind(if kind == "user" { "admin" } else { "member" })
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO memshare.credentials(id,tenant_id,principal_id,token_hash,status) VALUES ($1,$2,$3,$4,'active')")
        .bind(credential_id)
        .bind(tenant_id)
        .bind(id)
        .bind(token_hash(&token))
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    Actor {
        id,
        credential_id,
        token,
    }
}

#[tokio::test]
async fn sorted_grant_locks_database_clock_leases_and_pool_reuse_are_safe() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::ERROR)
        .with_test_writer()
        .try_init();
    let database_url = std::env::var("MEMSHARE_TEST_DATABASE_URL")
        .expect("MEMSHARE_TEST_DATABASE_URL is required for integration tests");
    let options = PgConnectOptions::from_str(&database_url).unwrap();
    let database_name = format!("memshare_concurrency_{}", Uuid::new_v4().simple());
    let control = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options.clone().database("postgres"))
        .await
        .unwrap();
    sqlx::query(&format!("CREATE DATABASE {database_name}"))
        .execute(&control)
        .await
        .unwrap();
    let test_options = options.database(&database_name);
    let owner_pool = PgPoolOptions::new()
        .max_connections(8)
        .connect_with(test_options.clone())
        .await
        .unwrap();
    migrate(&owner_pool).await.unwrap();

    let tenant_id = Uuid::new_v4();
    let workspace_id = Uuid::new_v4();
    sqlx::query("INSERT INTO memshare.tenants(id,slug,status) VALUES ($1,$2,'active')")
        .bind(tenant_id)
        .bind(format!("concurrency-{tenant_id}"))
        .execute(&owner_pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO memshare.tenant_limits(tenant_id,requests_per_minute) VALUES ($1,100000)",
    )
    .bind(tenant_id)
    .execute(&owner_pool)
    .await
    .unwrap();
    let setup_principal_id = Uuid::new_v4();
    let mut setup_tx = owner_pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('memshare.tenant_id',$1,true),set_config('memshare.principal_id',$2,true)")
        .bind(tenant_id.to_string())
        .bind(setup_principal_id.to_string())
        .execute(&mut *setup_tx)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO memshare.workspaces(tenant_id,id,slug,status) VALUES ($1,$2,'main','active')",
    )
    .bind(tenant_id)
    .bind(workspace_id)
    .execute(&mut *setup_tx)
    .await
    .unwrap();
    setup_tx.commit().await.unwrap();
    let user_a = insert_actor(&owner_pool, tenant_id, workspace_id, "user", None, "user-a").await;
    let user_b = insert_actor(&owner_pool, tenant_id, workspace_id, "user", None, "user-b").await;

    let role = format!("memshare_runtime_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE ROLE {role} NOLOGIN NOBYPASSRLS"))
        .execute(&owner_pool)
        .await
        .unwrap();
    sqlx::query(&format!("GRANT USAGE ON SCHEMA memshare TO {role}"))
        .execute(&owner_pool)
        .await
        .unwrap();
    sqlx::query(&format!(
        "GRANT SELECT,INSERT,UPDATE,DELETE ON ALL TABLES IN SCHEMA memshare TO {role}"
    ))
    .execute(&owner_pool)
    .await
    .unwrap();
    sqlx::query(&format!(
        "GRANT EXECUTE ON FUNCTION memshare.authenticate_request(text) TO {role}"
    ))
    .execute(&owner_pool)
    .await
    .unwrap();
    let runtime_pool = PgPoolOptions::new()
        .max_connections(10)
        .after_connect({
            let role = role.clone();
            move |connection, _| {
                let statement = format!("SET ROLE {role}");
                Box::pin(async move {
                    sqlx::query(&statement).execute(connection).await?;
                    Ok(())
                })
            }
        })
        .connect_with(test_options.clone())
        .await
        .unwrap();
    let app = router(AppState::new(runtime_pool.clone()));

    let ready = app
        .clone()
        .oneshot(request("GET", "/health/ready", "", None))
        .await
        .unwrap();
    assert_eq!(ready.status(), StatusCode::OK);
    let card = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/.well-known/agent-card.json")
                .header("host", "attacker.example")
                .header("x-forwarded-host", "attacker.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response_json(card).await["url"],
        "http://127.0.0.1:8080/v1/a2a"
    );
    sqlx::query(&format!(
        "REVOKE EXECUTE ON FUNCTION memshare.authenticate_request(text) FROM {role}"
    ))
    .execute(&owner_pool)
    .await
    .unwrap();
    let missing_grant = app
        .clone()
        .oneshot(request("GET", "/health/ready", "", None))
        .await
        .unwrap();
    assert_eq!(missing_grant.status(), StatusCode::SERVICE_UNAVAILABLE);
    sqlx::query(&format!(
        "GRANT EXECUTE ON FUNCTION memshare.authenticate_request(text) TO {role}"
    ))
    .execute(&owner_pool)
    .await
    .unwrap();

    let fresh_admin = insert_actor(
        &owner_pool,
        tenant_id,
        workspace_id,
        "user",
        None,
        "rate-limit-user",
    )
    .await;
    let limits = app
        .clone()
        .oneshot(request("GET", "/v1/tenant/limits", &user_a.token, None))
        .await
        .unwrap();
    assert_eq!(limits.status(), StatusCode::OK);
    let quota_update = app
        .clone()
        .oneshot(request(
            "PATCH",
            "/v1/tenant/limits",
            &user_a.token,
            Some(json!({"max_principals":3})),
        ))
        .await
        .unwrap();
    assert_eq!(quota_update.status(), StatusCode::OK);
    let quota_denied = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/principals",
            &user_a.token,
            Some(json!({"kind":"service","name":"over-quota","tenant_role":"member","expires_at":null})),
        ))
        .await
        .unwrap();
    assert_eq!(quota_denied.status(), StatusCode::TOO_MANY_REQUESTS);
    let limits_restored = app
        .clone()
        .oneshot(request(
            "PATCH",
            "/v1/tenant/limits",
            &user_a.token,
            Some(json!({"max_principals":1000,"requests_per_minute":1})),
        ))
        .await
        .unwrap();
    assert_eq!(limits_restored.status(), StatusCode::OK);

    let first_request = app
        .clone()
        .oneshot(request("GET", "/v1/memories", &fresh_admin.token, None))
        .await
        .unwrap();
    assert_eq!(first_request.status(), StatusCode::OK);
    let limited_request = app
        .clone()
        .oneshot(request("GET", "/v1/memories", &fresh_admin.token, None))
        .await
        .unwrap();
    assert_eq!(limited_request.status(), StatusCode::TOO_MANY_REQUESTS);
    sqlx::query("UPDATE memshare.tenant_limits SET requests_per_minute=100000 WHERE tenant_id=$1")
        .bind(tenant_id)
        .execute(&owner_pool)
        .await
        .unwrap();

    let credentials = app
        .clone()
        .oneshot(request(
            "GET",
            "/v1/admin/credentials?limit=100",
            &user_a.token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(credentials.status(), StatusCode::OK);
    let credentials = response_json(credentials).await;
    assert!(credentials["items"].as_array().unwrap().iter().any(|item| {
        item["id"] == fresh_admin.credential_id.to_string() && !item["last_used_at"].is_null()
    }));

    let invalid_request = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/grants",
            &fresh_admin.token,
            Some(json!({"invalid":true})),
        ))
        .await
        .unwrap();
    assert!(invalid_request.status().is_client_error());
    let audit_events = app
        .clone()
        .oneshot(request(
            "GET",
            "/v1/admin/audit-events?limit=100",
            &user_a.token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(audit_events.status(), StatusCode::OK);
    let audit_events = response_json(audit_events).await;
    let audit_items = audit_events["items"].as_array().unwrap();
    assert!(audit_items.iter().any(|item| {
        item["outcome"] == "denied" && item["request_id"].as_str().is_some_and(|id| !id.is_empty())
    }));
    assert!(audit_items.iter().all(|item| {
        item["previous_hash"]
            .as_str()
            .is_some_and(|hash| hash.len() == 64)
            && item["event_hash"]
                .as_str()
                .is_some_and(|hash| hash.len() == 64)
    }));
    for pair in audit_items.windows(2) {
        assert_eq!(pair[0]["previous_hash"], pair[1]["event_hash"]);
    }
    let verification = app
        .clone()
        .oneshot(request(
            "GET",
            "/v1/admin/audit-events/verify",
            &user_a.token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(verification.status(), StatusCode::OK);
    let verification = response_json(verification).await;
    assert_eq!(verification["valid"], true);
    assert!(verification["event_count"].as_i64().unwrap() >= 1);

    let older_transaction_event = Uuid::new_v4();
    let newer_transaction_event = Uuid::new_v4();
    let mut older_transaction = owner_pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('memshare.tenant_id',$1,true),set_config('memshare.principal_id',$2,true)")
        .bind(tenant_id.to_string())
        .bind(user_a.id.to_string())
        .execute(&mut *older_transaction)
        .await
        .unwrap();
    let mut newer_transaction = owner_pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('memshare.tenant_id',$1,true),set_config('memshare.principal_id',$2,true)")
        .bind(tenant_id.to_string())
        .bind(user_a.id.to_string())
        .execute(&mut *newer_transaction)
        .await
        .unwrap();
    sqlx::query("INSERT INTO memshare.audit_events(tenant_id,id,actor_principal_id,action,outcome,target_type) VALUES ($1,$2,$3,'concurrent-newer','allowed','test')")
        .bind(tenant_id)
        .bind(newer_transaction_event)
        .bind(user_a.id)
        .execute(&mut *newer_transaction)
        .await
        .unwrap();
    newer_transaction.commit().await.unwrap();
    sqlx::query("INSERT INTO memshare.audit_events(tenant_id,id,actor_principal_id,action,outcome,target_type) VALUES ($1,$2,$3,'concurrent-older','allowed','test')")
        .bind(tenant_id)
        .bind(older_transaction_event)
        .bind(user_a.id)
        .execute(&mut *older_transaction)
        .await
        .unwrap();
    older_transaction.commit().await.unwrap();
    let (newer_position, older_position, timestamps_inverted): (i64, i64, bool) =
        sqlx::query_as(
            "SELECT newer.chain_position,older.chain_position,older.occurred_at<newer.occurred_at FROM memshare.audit_events newer JOIN memshare.audit_events older ON newer.tenant_id=older.tenant_id WHERE newer.tenant_id=$1 AND newer.id=$2 AND older.id=$3",
        )
        .bind(tenant_id)
        .bind(newer_transaction_event)
        .bind(older_transaction_event)
        .fetch_one(&owner_pool)
        .await
        .unwrap();
    assert!(timestamps_inverted);
    assert!(newer_position < older_position);
    let concurrent_verification = app
        .clone()
        .oneshot(request(
            "GET",
            "/v1/admin/audit-events/verify",
            &user_a.token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(concurrent_verification.status(), StatusCode::OK);
    assert_eq!(response_json(concurrent_verification).await["valid"], true);

    for sequence in 0..20 {
        let body_a = json!({
            "subject_principal_id": user_b.id,
            "workspace_id": workspace_id,
            "scope": "workspace",
            "scope_id": workspace_id,
            "actions": ["read"],
            "expires_at": null
        });
        let body_b = json!({
            "subject_principal_id": user_a.id,
            "workspace_id": workspace_id,
            "scope": "workspace",
            "scope_id": workspace_id,
            "actions": ["read"],
            "expires_at": null
        });
        let left = app
            .clone()
            .oneshot(request("POST", "/v1/grants", &user_a.token, Some(body_a)));
        let right = app
            .clone()
            .oneshot(request("POST", "/v1/grants", &user_b.token, Some(body_b)));
        let (left, right) = tokio::join!(left, right);
        let left = left.unwrap();
        let right = right.unwrap();
        if left.status() != StatusCode::CREATED {
            panic!("left round {sequence}: {}", response_json(left).await);
        }
        if right.status() != StatusCode::CREATED {
            panic!("right round {sequence}: {}", response_json(right).await);
        }
    }

    let single_connection_pool = PgPoolOptions::new()
        .max_connections(1)
        .after_connect({
            let role = role.clone();
            move |connection, _| {
                let statement = format!("SET ROLE {role}");
                Box::pin(async move {
                    sqlx::query(&statement).execute(connection).await?;
                    Ok(())
                })
            }
        })
        .connect_with(test_options.clone())
        .await
        .unwrap();
    let mut scoped = single_connection_pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('memshare.tenant_id',$1,true),set_config('memshare.principal_id',$2,true)")
        .bind(tenant_id.to_string())
        .bind(user_a.id.to_string())
        .execute(&mut *scoped)
        .await
        .unwrap();
    let visible: i64 = sqlx::query_scalar("SELECT count(*) FROM memshare.principals")
        .fetch_one(&mut *scoped)
        .await
        .unwrap();
    assert!(visible >= 2);
    scoped.commit().await.unwrap();
    let mut reused = single_connection_pool.begin().await.unwrap();
    let unscoped: i64 = sqlx::query_scalar("SELECT count(*) FROM memshare.principals")
        .fetch_one(&mut *reused)
        .await
        .unwrap();
    assert_eq!(unscoped, 0, "transaction-local tenant context leaked");
    reused.rollback().await.unwrap();

    let source = insert_actor(
        &owner_pool,
        tenant_id,
        workspace_id,
        "agent",
        Some(user_a.id),
        "source-agent",
    )
    .await;
    let target = insert_actor(
        &owner_pool,
        tenant_id,
        workspace_id,
        "agent",
        Some(user_b.id),
        "target-agent",
    )
    .await;
    let lifecycle_target = insert_actor(
        &owner_pool,
        tenant_id,
        workspace_id,
        "agent",
        Some(user_b.id),
        "lifecycle-target-agent",
    )
    .await;
    for sequence in 0..20 {
        let grant = app.clone().oneshot(request(
            "POST",
            "/v1/grants",
            &user_b.token,
            Some(json!({
                "subject_principal_id": source.id,
                "workspace_id": workspace_id,
                "scope": "agent",
                "scope_id": lifecycle_target.id,
                "actions": ["handoff"],
                "expires_at": null
            })),
        ));
        let disable = app.clone().oneshot(request(
            "PATCH",
            &format!("/v1/principals/{}", lifecycle_target.id),
            &user_a.token,
            Some(json!({"status":"disabled"})),
        ));
        let (grant, disable) = tokio::join!(grant, disable);
        let grant = grant.unwrap();
        let disable = disable.unwrap();
        assert!(
            matches!(grant.status(), StatusCode::CREATED | StatusCode::NOT_FOUND),
            "grant/lifecycle round {sequence} returned {}",
            grant.status()
        );
        assert_eq!(disable.status(), StatusCode::OK, "disable round {sequence}");
        let mut invariant_tx = owner_pool.begin().await.unwrap();
        sqlx::query("SELECT set_config('memshare.tenant_id',$1,true),set_config('memshare.principal_id',$2,true)")
            .bind(tenant_id.to_string())
            .bind(user_a.id.to_string())
            .execute(&mut *invariant_tx)
            .await
            .unwrap();
        let surviving_grants: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM memshare.grants WHERE tenant_id=$1 AND scope='agent' AND scope_id=$2 AND revoked_at IS NULL",
        )
        .bind(tenant_id)
        .bind(lifecycle_target.id)
        .fetch_one(&mut *invariant_tx)
        .await
        .unwrap();
        invariant_tx.commit().await.unwrap();
        assert_eq!(
            surviving_grants, 0,
            "grant survived disable round {sequence}"
        );
        let reactivate = app
            .clone()
            .oneshot(request(
                "PATCH",
                &format!("/v1/principals/{}", lifecycle_target.id),
                &user_a.token,
                Some(json!({"status":"active"})),
            ))
            .await
            .unwrap();
        assert_eq!(
            reactivate.status(),
            StatusCode::OK,
            "reactivate round {sequence}"
        );
    }
    let rotate = app.clone().oneshot(request(
        "POST",
        &format!("/v1/principals/{}/credentials", lifecycle_target.id),
        &user_a.token,
        None,
    ));
    let disable = app.clone().oneshot(request(
        "PATCH",
        &format!("/v1/principals/{}", lifecycle_target.id),
        &user_a.token,
        Some(json!({"status":"disabled"})),
    ));
    let (rotate, disable) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(rotate, disable)
    })
    .await
    .expect("credential rotation and lifecycle update deadlocked");
    assert!(matches!(
        rotate.unwrap().status(),
        StatusCode::CREATED | StatusCode::NOT_FOUND
    ));
    assert_eq!(disable.unwrap().status(), StatusCode::OK);
    let mut credential_invariant_tx = owner_pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('memshare.tenant_id',$1,true),set_config('memshare.principal_id',$2,true)")
        .bind(tenant_id.to_string())
        .bind(user_a.id.to_string())
        .execute(&mut *credential_invariant_tx)
        .await
        .unwrap();
    let active_credentials: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM memshare.credentials WHERE tenant_id=$1 AND principal_id=$2 AND status='active'",
    )
    .bind(tenant_id)
    .bind(lifecycle_target.id)
    .fetch_one(&mut *credential_invariant_tx)
    .await
    .unwrap();
    credential_invariant_tx.commit().await.unwrap();
    assert_eq!(active_credentials, 0);
    let mut grant_tx = owner_pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('memshare.tenant_id',$1,true),set_config('memshare.principal_id',$2,true)")
        .bind(tenant_id.to_string())
        .bind(user_b.id.to_string())
        .execute(&mut *grant_tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO memshare.grants(tenant_id,id,issuer_principal_id,subject_principal_id,workspace_id,scope,scope_id,actions) VALUES ($1,$2,$3,$4,$5,'agent',$6,ARRAY['handoff'])")
        .bind(tenant_id)
        .bind(Uuid::new_v4())
        .bind(user_b.id)
        .bind(source.id)
        .bind(workspace_id)
        .bind(target.id)
        .execute(&mut *grant_tx)
        .await
        .unwrap();
    grant_tx.commit().await.unwrap();

    let handoff_request = json!({
        "workspace_id": workspace_id,
        "target_agent_id": target.id,
        "task": {"protocol_version":"1.0","summary":"protocol test","instructions":null,"data":{}},
        "input_memory_ids": [],
        "source": {"provider":"test","model":"source","framework":"integration"},
        "expires_at": null
    });
    let mcp_create = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/mcp",
            &source.token,
            Some(json!({
                "jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":"memshare_create_handoff","arguments":handoff_request.clone()}
            })),
        ))
        .await
        .unwrap();
    assert_eq!(mcp_create.status(), StatusCode::OK);
    let mcp_create = response_json(mcp_create).await;
    let mcp_handoff_id = mcp_create["result"]["structuredContent"]["id"]
        .as_str()
        .unwrap();
    let mcp_claim = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/mcp",
            &target.token,
            Some(json!({
                "jsonrpc":"2.0","id":2,"method":"tools/call",
                "params":{"name":"memshare_claim_handoff","arguments":{"handoff_id":mcp_handoff_id,"version":1,"lease_seconds":30}}
            })),
        ))
        .await
        .unwrap();
    assert_eq!(
        response_json(mcp_claim).await["result"]["structuredContent"]["version"],
        2
    );
    let mcp_complete = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/mcp",
            &target.token,
            Some(json!({
                "jsonrpc":"2.0","id":3,"method":"tools/call",
                "params":{"name":"memshare_complete_handoff","arguments":{
                    "handoff_id":mcp_handoff_id,"version":2,
                    "result":{"protocol_version":"1.0","summary":"done","outcome":"succeeded","data":{}},
                    "result_memory_ids":[],
                    "source":{"provider":"test","model":"target","framework":"integration"}
                }}
            })),
        ))
        .await
        .unwrap();
    assert_eq!(
        response_json(mcp_complete).await["result"]["structuredContent"]["status"],
        "completed"
    );
    let a2a_get = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/a2a",
            &source.token,
            Some(
                json!({"jsonrpc":"2.0","id":4,"method":"tasks/get","params":{"id":mcp_handoff_id}}),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(
        response_json(a2a_get).await["result"]["status"]["state"],
        "completed"
    );
    let a2a_create = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/a2a",
            &source.token,
            Some(json!({
                "jsonrpc":"2.0","id":5,"method":"message/send","params":{"message":{"parts":[{"kind":"data","data":{
                    "operation":"create_handoff","idempotency_key":"a2a-integration","request":handoff_request
                }}]}}
            })),
        ))
        .await
        .unwrap();
    assert_eq!(
        response_json(a2a_create).await["result"]["status"]["state"],
        "submitted"
    );
    let malformed_mcp = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/mcp",
            &source.token,
            Some(json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{}})),
        ))
        .await
        .unwrap();
    assert_eq!(response_json(malformed_mcp).await["error"]["code"], -32602);

    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/handoffs")
                .header("authorization", format!("Bearer {}", source.token))
                .header("content-type", "application/json")
                .header("idempotency-key", "database-clock-lease")
                .body(Body::from(
                    json!({
                        "workspace_id": workspace_id,
                        "target_agent_id": target.id,
                        "task": {"protocol_version":"1.0","summary":"clock test","instructions":null,"data":{}},
                        "input_memory_ids": [],
                        "source": {"provider":"test","model":"source","framework":"integration"},
                        "expires_at": null
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::CREATED);
    let handoff_id = response_json(create).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let claim = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/handoffs/{handoff_id}/claim"),
            &target.token,
            Some(json!({"version":1,"lease_seconds":30})),
        ))
        .await
        .unwrap();
    assert_eq!(claim.status(), StatusCode::OK);

    let mut expire_tx = owner_pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('memshare.tenant_id',$1,true),set_config('memshare.principal_id',$2,true)")
        .bind(tenant_id.to_string())
        .bind(target.id.to_string())
        .execute(&mut *expire_tx)
        .await
        .unwrap();
    sqlx::query("UPDATE memshare.handoffs SET lease_expires_at=clock_timestamp()-interval '1 second' WHERE tenant_id=$1 AND id=$2")
        .bind(tenant_id)
        .bind(Uuid::parse_str(&handoff_id).unwrap())
        .execute(&mut *expire_tx)
        .await
        .unwrap();
    expire_tx.commit().await.unwrap();

    let expired_completion = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/handoffs/{handoff_id}/complete"),
            &target.token,
            Some(json!({
                "version":2,
                "result":{"protocol_version":"1.0","summary":"late","outcome":"succeeded","data":{}},
                "result_memory_ids":[],
                "source":{"provider":"test","model":"target","framework":"integration"}
            })),
        ))
        .await
        .unwrap();
    assert_eq!(expired_completion.status(), StatusCode::CONFLICT);
    let reclaim = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/handoffs/{handoff_id}/claim"),
            &target.token,
            Some(json!({"version":2,"lease_seconds":30})),
        ))
        .await
        .unwrap();
    assert_eq!(reclaim.status(), StatusCode::OK);
    assert_eq!(response_json(reclaim).await["version"], 3);

    sqlx::query("UPDATE memshare.credentials SET status='revoked',revoked_at=clock_timestamp() WHERE token_hash=$1")
        .bind(token_hash(&target.token))
        .execute(&owner_pool)
        .await
        .unwrap();
    let revoked = app
        .oneshot(request(
            "GET",
            &format!("/v1/handoffs/{handoff_id}"),
            &target.token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);

    runtime_pool.close().await;
    single_connection_pool.close().await;
    owner_pool.close().await;
    sqlx::query(&format!("DROP DATABASE {database_name}"))
        .execute(&control)
        .await
        .unwrap();
    sqlx::query(&format!("DROP ROLE {role}"))
        .execute(&control)
        .await
        .unwrap();
    control.close().await;
}
