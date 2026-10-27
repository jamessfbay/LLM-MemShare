use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use llm_memshare::{
    auth::AppState,
    db::{issue_token, token_hash, verify_runtime_role},
    migrate, router,
};
use serde_json::{json, Value};
use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions},
    PgPool,
};
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    tenant_id: Uuid,
    workspace_id: Uuid,
    principal_id: Uuid,
    token: String,
}

async fn fixture(pool: &PgPool, prefix: &str) -> Fixture {
    let tenant_id = Uuid::new_v4();
    let workspace_id = Uuid::new_v4();
    let principal_id = Uuid::new_v4();
    let token = issue_token();
    let mut tx = pool.begin().await.expect("begin fixture");
    sqlx::query("SELECT set_config('memshare.tenant_id',$1,true),set_config('memshare.principal_id',$2,true)")
        .bind(tenant_id.to_string())
        .bind(principal_id.to_string())
        .execute(&mut *tx)
        .await
        .expect("set tenant context");
    sqlx::query("INSERT INTO memshare.tenants(id,slug,status) VALUES ($1,$2,'active')")
        .bind(tenant_id)
        .bind(format!("{prefix}-{tenant_id}"))
        .execute(&mut *tx)
        .await
        .expect("insert tenant");
    sqlx::query("INSERT INTO memshare.principals(tenant_id,id,kind,name,status,tenant_role) VALUES ($1,$2,'user','owner','active','admin')")
        .bind(tenant_id).bind(principal_id).execute(&mut *tx).await.expect("insert principal");
    sqlx::query(
        "INSERT INTO memshare.workspaces(tenant_id,id,slug,status) VALUES ($1,$2,'main','active')",
    )
    .bind(tenant_id)
    .bind(workspace_id)
    .execute(&mut *tx)
    .await
    .expect("insert workspace");
    sqlx::query("INSERT INTO memshare.workspace_memberships(tenant_id,workspace_id,principal_id,role,status) VALUES ($1,$2,$3,'admin','active')")
        .bind(tenant_id).bind(workspace_id).bind(principal_id).execute(&mut *tx).await.expect("insert membership");
    sqlx::query("INSERT INTO memshare.credentials(id,tenant_id,principal_id,token_hash,status) VALUES ($1,$2,$3,$4,'active')")
        .bind(Uuid::new_v4()).bind(tenant_id).bind(principal_id).bind(token_hash(&token)).execute(&mut *tx).await.expect("insert credential");
    tx.commit().await.expect("commit fixture");
    Fixture {
        tenant_id,
        workspace_id,
        principal_id,
        token,
    }
}

async fn workspace_member_fixture(pool: &PgPool, owner: &Fixture) -> Fixture {
    let principal_id = Uuid::new_v4();
    let token = issue_token();
    sqlx::query("INSERT INTO memshare.principals(tenant_id,id,kind,name,status,tenant_role) VALUES ($1,$2,'user','member','active','member')")
        .bind(owner.tenant_id)
        .bind(principal_id)
        .execute(pool)
        .await
        .expect("insert member principal");
    sqlx::query("INSERT INTO memshare.workspace_memberships(tenant_id,workspace_id,principal_id,role,status) VALUES ($1,$2,$3,'member','active')")
        .bind(owner.tenant_id)
        .bind(owner.workspace_id)
        .bind(principal_id)
        .execute(pool)
        .await
        .expect("insert member membership");
    sqlx::query("INSERT INTO memshare.credentials(id,tenant_id,principal_id,token_hash,status) VALUES ($1,$2,$3,$4,'active')")
        .bind(Uuid::new_v4())
        .bind(owner.tenant_id)
        .bind(principal_id)
        .bind(token_hash(&token))
        .execute(pool)
        .await
        .expect("insert member credential");
    Fixture {
        tenant_id: owner.tenant_id,
        workspace_id: owner.workspace_id,
        principal_id,
        token,
    }
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1_000_000)
        .await
        .expect("read response");
    serde_json::from_slice(&bytes).expect("json response")
}

async fn runtime_pool(options: PgConnectOptions, admin: &PgPool) -> PgPool {
    let role = format!("memshare_runtime_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE ROLE {role} NOLOGIN NOBYPASSRLS"))
        .execute(admin)
        .await
        .expect("create runtime role");
    sqlx::query(&format!("GRANT USAGE ON SCHEMA memshare TO {role}"))
        .execute(admin)
        .await
        .expect("grant schema use");
    sqlx::query(&format!(
        "GRANT SELECT,INSERT,UPDATE,DELETE ON ALL TABLES IN SCHEMA memshare TO {role}"
    ))
    .execute(admin)
    .await
    .expect("grant table DML");
    sqlx::query(&format!(
        "GRANT EXECUTE ON FUNCTION memshare.authenticate_credential(text) TO {role}"
    ))
    .execute(admin)
    .await
    .expect("grant authentication function");

    PgPoolOptions::new()
        .max_connections(10)
        .after_connect(move |connection, _| {
            let statement = format!("SET ROLE {role}");
            Box::pin(async move {
                sqlx::query(&statement).execute(connection).await?;
                Ok(())
            })
        })
        .connect_with(options)
        .await
        .expect("connect runtime pool")
}

fn request(method: &str, uri: String, token: &str, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"));
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .expect("request")
}

#[tokio::test]
async fn tenant_isolation_grant_and_revocation_flow() {
    let database_url = std::env::var("MEMSHARE_TEST_DATABASE_URL")
        .expect("MEMSHARE_TEST_DATABASE_URL is required for integration tests");
    let options = PgConnectOptions::from_str(&database_url).expect("valid test database URL");
    let database_name = format!("memshare_test_{}", Uuid::new_v4().simple());
    let control_pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options.clone().database("postgres"))
        .await
        .expect("connect database control pool");
    sqlx::query(&format!("CREATE DATABASE {database_name}"))
        .execute(&control_pool)
        .await
        .expect("create isolated test database");
    let test_options = options.database(&database_name);
    let admin_pool = PgPoolOptions::new()
        .max_connections(5)
        .connect_with(test_options.clone())
        .await
        .expect("connect test database");
    migrate(&admin_pool).await.expect("migrate test database");
    let a = fixture(&admin_pool, "tenant-a").await;
    let member = workspace_member_fixture(&admin_pool, &a).await;
    let b = fixture(&admin_pool, "tenant-b").await;
    let pool = runtime_pool(test_options, &admin_pool).await;
    assert!(verify_runtime_role(&pool).await.is_ok());
    assert!(verify_runtime_role(&admin_pool).await.is_err());
    let app = router(AppState { pool: pool.clone() });

    let delegated_admin = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/principals".to_owned(),
            &a.token,
            Some(json!({"kind":"user","name":"delegated-admin","tenant_role":"admin","expires_at":null})),
        ))
        .await
        .unwrap();
    assert_eq!(delegated_admin.status(), StatusCode::CREATED);
    let delegated_admin_token = response_json(delegated_admin).await["token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        app.clone()
            .oneshot(request(
                "POST",
                "/v1/workspaces".to_owned(),
                &delegated_admin_token,
                Some(json!({"slug":"delegated"})),
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::CREATED
    );

    let service = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/principals".to_owned(),
            &a.token,
            Some(json!({"kind":"service","name":"indexer","expires_at":null})),
        ))
        .await
        .unwrap();
    assert_eq!(service.status(), StatusCode::CREATED);
    let service = response_json(service).await;
    let service_id = service["principal_id"].as_str().unwrap();
    let old_service_token = service["token"].as_str().unwrap();
    let workspace = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/workspaces".to_owned(),
            &a.token,
            Some(json!({"slug":"operations"})),
        ))
        .await
        .unwrap();
    assert_eq!(workspace.status(), StatusCode::CREATED);
    let workspace = response_json(workspace).await;
    let managed_workspace_id = workspace["id"].as_str().unwrap();
    assert_eq!(
        app.clone()
            .oneshot(request(
                "PUT",
                format!("/v1/workspaces/{managed_workspace_id}/members/{service_id}"),
                &a.token,
                Some(json!({"role":"member"})),
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let project = app
        .clone()
        .oneshot(request(
            "POST",
            format!("/v1/workspaces/{managed_workspace_id}/projects"),
            &a.token,
            Some(json!({"slug":"launch"})),
        ))
        .await
        .unwrap();
    assert_eq!(project.status(), StatusCode::CREATED);
    let project_id = response_json(project).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        app.clone()
            .oneshot(request(
                "PUT",
                format!("/v1/workspaces/{managed_workspace_id}/projects/{project_id}/members/{service_id}"),
                &a.token,
                Some(json!({"role":"member"})),
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let rotated = app
        .clone()
        .oneshot(request(
            "POST",
            format!("/v1/principals/{service_id}/credentials"),
            &a.token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(rotated.status(), StatusCode::CREATED);
    let rotated = response_json(rotated).await;
    assert_ne!(rotated["token"], old_service_token);
    let rotated_service_token = rotated["token"].as_str().unwrap();
    assert_eq!(
        app.clone()
            .oneshot(request(
                "GET",
                "/v1/memories".to_owned(),
                old_service_token,
                None,
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.clone()
            .oneshot(request(
                "DELETE",
                format!("/v1/workspaces/{managed_workspace_id}/members/{service_id}"),
                &a.token,
                None,
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let project_membership_status: String = sqlx::query_scalar("SELECT status FROM memshare.project_memberships WHERE tenant_id=$1 AND workspace_id=$2 AND project_id=$3 AND principal_id=$4")
        .bind(a.tenant_id)
        .bind(Uuid::parse_str(managed_workspace_id).unwrap())
        .bind(Uuid::parse_str(&project_id).unwrap())
        .bind(Uuid::parse_str(service_id).unwrap())
        .fetch_one(&admin_pool)
        .await
        .unwrap();
    assert_eq!(project_membership_status, "removed");
    let disabled = app
        .clone()
        .oneshot(request(
            "PATCH",
            format!("/v1/principals/{service_id}"),
            &a.token,
            Some(json!({"status":"disabled"})),
        ))
        .await
        .unwrap();
    assert_eq!(disabled.status(), StatusCode::OK);
    assert_eq!(response_json(disabled).await["status"], "disabled");
    assert_eq!(
        app.clone()
            .oneshot(request(
                "GET",
                "/v1/memories".to_owned(),
                rotated_service_token,
                None,
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    let contract = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/tool-contracts/handoff")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(contract.status(), StatusCode::OK);
    assert_eq!(response_json(contract).await["version"], "1.0");

    let create = request(
        "POST",
        "/v1/memories".to_owned(),
        &a.token,
        Some(json!({
            "workspace_id":a.workspace_id,
            "project_id":null,
            "scope":"private",
            "scope_id":a.principal_id,
            "kind":"fact",
            "content":"Tenant A private memory",
            "source":{},
            "expires_at":null
        })),
    );
    let mut create = create;
    create
        .headers_mut()
        .insert("Idempotency-Key", "tenant-a-memory".parse().unwrap());
    let response = app.clone().oneshot(create).await.expect("create response");
    assert_eq!(response.status(), StatusCode::CREATED);
    let memory = response_json(response).await;
    let memory_id = memory["id"].as_str().expect("memory id");

    let replay_body = json!({
        "workspace_id":a.workspace_id,
        "project_id":null,
        "scope":"private",
        "scope_id":a.principal_id,
        "kind":"fact",
        "content":"Tenant A private memory",
        "source":{},
        "expires_at":null
    });
    let mut replay = request(
        "POST",
        "/v1/memories".to_owned(),
        &a.token,
        Some(replay_body.clone()),
    );
    replay
        .headers_mut()
        .insert("Idempotency-Key", "tenant-a-memory".parse().unwrap());
    let replay = app.clone().oneshot(replay).await.unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(response_json(replay).await["id"], memory["id"]);

    let mut collision_body = replay_body;
    collision_body["content"] = json!("different content");
    let mut collision = request(
        "POST",
        "/v1/memories".to_owned(),
        &a.token,
        Some(collision_body),
    );
    collision
        .headers_mut()
        .insert("Idempotency-Key", "tenant-a-memory".parse().unwrap());
    assert_eq!(
        app.clone().oneshot(collision).await.unwrap().status(),
        StatusCode::CONFLICT
    );

    let updated = app
        .clone()
        .oneshot(request(
            "PATCH",
            format!("/v1/memories/{memory_id}"),
            &a.token,
            Some(json!({"version":1,"content":"Updated tenant A memory"})),
        ))
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::OK);
    assert_eq!(response_json(updated).await["version"], 2);
    let stale = app
        .clone()
        .oneshot(request(
            "PATCH",
            format!("/v1/memories/{memory_id}"),
            &a.token,
            Some(json!({"version":1,"content":"Stale update"})),
        ))
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);

    let malformed = Request::builder()
        .method("POST")
        .uri("/v1/memories")
        .header("authorization", format!("Bearer {}", a.token))
        .header("content-type", "application/json")
        .body(Body::from("{"))
        .unwrap();
    let malformed = app.clone().oneshot(malformed).await.unwrap();
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(malformed).await["error"]["code"],
        "invalid_json"
    );
    let invalid_path = app
        .clone()
        .oneshot(request(
            "GET",
            "/v1/memories/not-a-uuid".to_owned(),
            &a.token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(invalid_path.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(invalid_path).await["error"]["code"],
        "invalid_path"
    );
    let invalid_query = app
        .clone()
        .oneshot(request(
            "GET",
            "/v1/memories?limit=nope".to_owned(),
            &a.token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(invalid_query.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(invalid_query).await["error"]["code"],
        "invalid_query"
    );

    let mut workspace_create = request(
        "POST",
        "/v1/memories".to_owned(),
        &a.token,
        Some(json!({
            "workspace_id":a.workspace_id,
            "project_id":null,
            "scope":"workspace",
            "scope_id":a.workspace_id,
            "kind":"fact",
            "content":"Shared but owner-controlled mutation",
            "source":{},
            "expires_at":null
        })),
    );
    workspace_create
        .headers_mut()
        .insert("Idempotency-Key", "workspace-memory".parse().unwrap());
    let workspace_memory = app.clone().oneshot(workspace_create).await.unwrap();
    assert_eq!(workspace_memory.status(), StatusCode::CREATED);
    let workspace_memory = response_json(workspace_memory).await;
    let workspace_memory_id = workspace_memory["id"].as_str().unwrap();
    assert_eq!(
        app.clone()
            .oneshot(request(
                "GET",
                format!("/v1/memories/{workspace_memory_id}"),
                &member.token,
                None,
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.clone()
            .oneshot(request(
                "PATCH",
                format!("/v1/memories/{workspace_memory_id}"),
                &member.token,
                Some(json!({"version":1,"content":"Unauthorized overwrite"})),
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.clone()
            .oneshot(request(
                "DELETE",
                format!("/v1/memories/{workspace_memory_id}?version=1"),
                &member.token,
                None,
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );

    let cross_tenant = app
        .clone()
        .oneshot(request(
            "GET",
            format!("/v1/memories/{memory_id}"),
            &b.token,
            None,
        ))
        .await
        .expect("cross-tenant response");
    assert_eq!(cross_tenant.status(), StatusCode::NOT_FOUND);

    let create_agent = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/agents".to_owned(),
            &a.token,
            Some(json!({"name":"reader","workspace_id":a.workspace_id,"expires_at":null})),
        ))
        .await
        .expect("agent response");
    assert_eq!(create_agent.status(), StatusCode::CREATED);
    let agent = response_json(create_agent).await;
    let agent_id = agent["principal_id"].as_str().unwrap();
    let agent_credential_id = agent["credential_id"].as_str().unwrap();
    let agent_token = agent["token"].as_str().unwrap();

    let before_grant = app
        .clone()
        .oneshot(request(
            "GET",
            format!("/v1/memories/{memory_id}"),
            agent_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(before_grant.status(), StatusCode::NOT_FOUND);

    let grant = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/grants".to_owned(),
            &a.token,
            Some(json!({
                "subject_principal_id":agent_id,
                "workspace_id":a.workspace_id,
                "scope":"private",
                "scope_id":a.principal_id,
                "actions":["read"],
                "expires_at":null
            })),
        ))
        .await
        .unwrap();
    assert_eq!(grant.status(), StatusCode::CREATED);
    let grant = response_json(grant).await;
    let grant_id = grant["id"].as_str().unwrap();

    let allowed = app
        .clone()
        .oneshot(request(
            "GET",
            format!("/v1/memories/{memory_id}"),
            agent_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(allowed.status(), StatusCode::OK);

    let revoked = app
        .clone()
        .oneshot(request(
            "DELETE",
            format!("/v1/grants/{grant_id}"),
            &a.token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::OK);
    let after_revoke = app
        .clone()
        .oneshot(request(
            "GET",
            format!("/v1/memories/{memory_id}"),
            agent_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(after_revoke.status(), StatusCode::NOT_FOUND);

    let source_agent_response = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/agents".to_owned(),
            &a.token,
            Some(json!({"name":"openai-source","workspace_id":a.workspace_id,"expires_at":null})),
        ))
        .await
        .unwrap();
    assert_eq!(source_agent_response.status(), StatusCode::CREATED);
    let source_agent = response_json(source_agent_response).await;
    let source_agent_id = source_agent["principal_id"].as_str().unwrap();
    let source_agent_token = source_agent["token"].as_str().unwrap();

    let target_agent_response = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/agents".to_owned(),
            &a.token,
            Some(
                json!({"name":"anthropic-target","workspace_id":a.workspace_id,"expires_at":null}),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(target_agent_response.status(), StatusCode::CREATED);
    let target_agent = response_json(target_agent_response).await;
    let target_agent_id = target_agent["principal_id"].as_str().unwrap();
    let target_agent_token = target_agent["token"].as_str().unwrap();

    let create_handoff_body = json!({
        "workspace_id": a.workspace_id,
        "target_agent_id": target_agent_id,
        "task": {"summary":"Review the deployment window","protocol_version":"1.0","instructions":null,"data":{}},
        "input_memory_ids": [memory_id],
        "source": {"provider":"openai","model":"test-openai-model","framework":"responses"},
        "expires_at": null
    });
    let mut self_handoff = request(
        "POST",
        "/v1/handoffs".to_owned(),
        source_agent_token,
        Some(json!({
            "workspace_id":a.workspace_id,
            "target_agent_id":source_agent_id,
            "task":{"protocol_version":"1.0","summary":"invalid","instructions":null,"data":{}},
            "input_memory_ids":[],
            "source":{"provider":"test","model":"test","framework":null},
            "expires_at":null
        })),
    );
    self_handoff
        .headers_mut()
        .insert("Idempotency-Key", "self-handoff".parse().unwrap());
    assert_eq!(
        app.clone().oneshot(self_handoff).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    let mut unauthorized_handoff = request(
        "POST",
        "/v1/handoffs".to_owned(),
        source_agent_token,
        Some(create_handoff_body.clone()),
    );
    unauthorized_handoff
        .headers_mut()
        .insert("Idempotency-Key", "handoff-before-grant".parse().unwrap());
    assert_eq!(
        app.clone()
            .oneshot(unauthorized_handoff)
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );

    for grant_body in [
        json!({
            "subject_principal_id":source_agent_id,
            "workspace_id":a.workspace_id,
            "scope":"private",
            "scope_id":a.principal_id,
            "actions":["read"],
            "expires_at":null
        }),
        json!({
            "subject_principal_id":source_agent_id,
            "workspace_id":a.workspace_id,
            "scope":"agent",
            "scope_id":target_agent_id,
            "actions":["handoff"],
            "expires_at":null
        }),
    ] {
        assert_eq!(
            app.clone()
                .oneshot(request(
                    "POST",
                    "/v1/grants".to_owned(),
                    &a.token,
                    Some(grant_body),
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::CREATED
        );
    }

    let openai_call = json!({
        "type":"function_call",
        "name":"memshare_create_handoff",
        "arguments":create_handoff_body.to_string(),
        "call_id":"call_cross_llm_1"
    });
    let mut create_handoff = request(
        "POST",
        "/v1/adapters/openai/handoffs".to_owned(),
        source_agent_token,
        Some(openai_call),
    );
    create_handoff
        .headers_mut()
        .insert("Idempotency-Key", "cross-llm-handoff".parse().unwrap());
    let created_handoff = app.clone().oneshot(create_handoff).await.unwrap();
    assert_eq!(created_handoff.status(), StatusCode::CREATED);
    let created_handoff = response_json(created_handoff).await;
    let handoff_id = created_handoff["id"].as_str().unwrap();
    assert_eq!(created_handoff["status"], "queued");
    assert_eq!(created_handoff["input_memory_ids"][0], memory_id);
    assert_eq!(
        created_handoff["source_metadata"]["verified"]["actor_principal_id"],
        source_agent_id
    );

    let inbox = app
        .clone()
        .oneshot(request(
            "GET",
            "/v1/handoffs?direction=inbox&status=queued&limit=10".to_owned(),
            target_agent_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(inbox.status(), StatusCode::OK);
    assert_eq!(response_json(inbox).await["items"][0]["id"], handoff_id);

    let target_view = app
        .clone()
        .oneshot(request(
            "GET",
            format!("/v1/handoffs/{handoff_id}"),
            target_agent_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(target_view.status(), StatusCode::OK);
    let target_view = response_json(target_view).await;
    assert_eq!(target_view["input_memory_ids"], json!([]));
    assert_eq!(target_view["omitted_input_references"], 1);

    assert_eq!(
        app.clone()
            .oneshot(request(
                "GET",
                format!("/v1/handoffs/{handoff_id}"),
                agent_token,
                None,
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.clone()
            .oneshot(request(
                "GET",
                format!("/v1/handoffs/{handoff_id}"),
                &b.token,
                None,
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );

    let claimed = app
        .clone()
        .oneshot(request(
            "POST",
            format!("/v1/handoffs/{handoff_id}/claim"),
            target_agent_token,
            Some(json!({"version":1})),
        ))
        .await
        .unwrap();
    assert_eq!(claimed.status(), StatusCode::OK);
    assert_eq!(response_json(claimed).await["version"], 2);
    assert_eq!(
        app.clone()
            .oneshot(request(
                "POST",
                format!("/v1/handoffs/{handoff_id}/claim"),
                target_agent_token,
                Some(json!({"version":1})),
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );

    let released = app
        .clone()
        .oneshot(request(
            "POST",
            format!("/v1/handoffs/{handoff_id}/release"),
            target_agent_token,
            Some(json!({"version":2})),
        ))
        .await
        .unwrap();
    assert_eq!(released.status(), StatusCode::OK);
    assert_eq!(response_json(released).await["version"], 3);

    let openai_claim = json!({
        "type":"function_call",
        "name":"memshare_claim_handoff",
        "arguments":json!({"version":3,"lease_seconds":300}).to_string(),
        "call_id":"call_claim_1"
    });
    let reclaimed = app
        .clone()
        .oneshot(request(
            "POST",
            format!("/v1/adapters/openai/handoffs/{handoff_id}/claim"),
            target_agent_token,
            Some(openai_claim),
        ))
        .await
        .unwrap();
    assert_eq!(reclaimed.status(), StatusCode::OK);
    assert_eq!(response_json(reclaimed).await["version"], 4);

    let anthropic_call = json!({
        "type":"tool_use",
        "name":"memshare_complete_handoff",
        "input":{
            "version":4,
            "result":{"protocol_version":"1.0","summary":"Deployment window confirmed","outcome":"succeeded","data":{"accepted":true}},
            "result_memory_ids":[],
            "source":{"provider":"anthropic","model":"test-anthropic-model","framework":"messages"}
        },
        "id":"toolu_cross_llm_1"
    });
    let completed = app
        .clone()
        .oneshot(request(
            "POST",
            format!("/v1/adapters/anthropic/handoffs/{handoff_id}/complete"),
            target_agent_token,
            Some(anthropic_call),
        ))
        .await
        .unwrap();
    assert_eq!(completed.status(), StatusCode::OK);
    let completed = response_json(completed).await;
    assert_eq!(completed["status"], "completed");
    assert_eq!(completed["version"], 5);
    assert_eq!(completed["result"]["data"]["accepted"], true);
    assert_eq!(
        completed["result_metadata"]["verified"]["actor_principal_id"],
        target_agent_id
    );

    let source_result = app
        .clone()
        .oneshot(request(
            "GET",
            format!("/v1/handoffs/{handoff_id}"),
            source_agent_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(source_result.status(), StatusCode::OK);
    assert_eq!(response_json(source_result).await["status"], "completed");

    let anthropic_create = json!({
        "type":"tool_use",
        "name":"memshare_create_handoff",
        "input":create_handoff_body,
        "id":"toolu_create_cancel_1"
    });
    let mut cancellable = request(
        "POST",
        "/v1/adapters/anthropic/handoffs".to_owned(),
        source_agent_token,
        Some(anthropic_create),
    );
    cancellable
        .headers_mut()
        .insert("Idempotency-Key", "cancel-handoff".parse().unwrap());
    let cancellable = app.clone().oneshot(cancellable).await.unwrap();
    assert_eq!(cancellable.status(), StatusCode::CREATED);
    let cancellable = response_json(cancellable).await;
    let cancellable_id = cancellable["id"].as_str().unwrap();
    let cancelled = app
        .clone()
        .oneshot(request(
            "POST",
            format!("/v1/handoffs/{cancellable_id}/cancel"),
            source_agent_token,
            Some(json!({"version":1})),
        ))
        .await
        .unwrap();
    assert_eq!(cancelled.status(), StatusCode::OK);
    assert_eq!(response_json(cancelled).await["status"], "cancelled");

    assert_eq!(
        app.clone()
            .oneshot(request(
                "DELETE",
                format!(
                    "/v1/workspaces/{}/members/{target_agent_id}",
                    a.workspace_id
                ),
                &a.token,
                None,
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.clone()
            .oneshot(request(
                "GET",
                format!("/v1/handoffs/{handoff_id}"),
                target_agent_token,
                None,
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );

    let credential_revoked = app
        .clone()
        .oneshot(request(
            "DELETE",
            format!("/v1/credentials/{agent_credential_id}"),
            &a.token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(credential_revoked.status(), StatusCode::OK);
    let revoked_credential = app
        .clone()
        .oneshot(request(
            "GET",
            format!("/v1/memories/{memory_id}"),
            agent_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(revoked_credential.status(), StatusCode::UNAUTHORIZED);

    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('memshare.tenant_id',$1,true),set_config('memshare.principal_id',$2,true)")
        .bind(b.tenant_id.to_string()).bind(b.principal_id.to_string()).execute(&mut *tx).await.unwrap();
    let visible: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memshare.memories WHERE id=$1)")
            .bind(Uuid::parse_str(memory_id).unwrap())
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert!(
        !visible,
        "RLS must hide another tenant's row even without an application predicate"
    );
    tx.rollback().await.unwrap();

    let deleted = app
        .clone()
        .oneshot(request(
            "DELETE",
            format!("/v1/memories/{memory_id}?version=2"),
            &a.token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    let after_delete = app
        .oneshot(request(
            "GET",
            format!("/v1/memories/{memory_id}"),
            &a.token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(after_delete.status(), StatusCode::NOT_FOUND);
    pool.close().await;
    admin_pool.close().await;
    sqlx::query(&format!("DROP DATABASE {database_name} WITH (FORCE)"))
        .execute(&control_pool)
        .await
        .expect("drop isolated test database");
    control_pool.close().await;
}
use std::str::FromStr;
