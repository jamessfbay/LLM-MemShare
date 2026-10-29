use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use llm_memshare::{
    auth::AppState,
    db::{begin_tenant, issue_token, token_hash, verify_runtime_role},
    migrate,
    model::PrincipalContext,
    router,
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

async fn runtime_pool(options: PgConnectOptions, admin: &PgPool) -> (PgPool, String) {
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

    let pool = PgPoolOptions::new()
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
        .connect_with(options)
        .await
        .expect("connect runtime pool");
    (pool, role)
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
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::ERROR)
        .with_test_writer()
        .try_init();
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
    let (pool, runtime_role) = runtime_pool(test_options, &admin_pool).await;
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
    let delegated_admin = response_json(delegated_admin).await;
    let delegated_admin_id = delegated_admin["principal_id"].as_str().unwrap().to_owned();
    let delegated_admin_token = delegated_admin["token"].as_str().unwrap().to_owned();
    let delegated_workspace = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/workspaces".to_owned(),
            &delegated_admin_token,
            Some(json!({"slug":"delegated"})),
        ))
        .await
        .unwrap();
    assert_eq!(delegated_workspace.status(), StatusCode::CREATED);
    let delegated_workspace_id = response_json(delegated_workspace).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let delegated_agent = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/agents".to_owned(),
            &delegated_admin_token,
            Some(json!({"name":"delegated-agent","workspace_id":delegated_workspace_id,"expires_at":null})),
        ))
        .await
        .unwrap();
    assert_eq!(delegated_agent.status(), StatusCode::CREATED);
    let delegated_agent = response_json(delegated_agent).await;
    let delegated_agent_id = delegated_agent["principal_id"].as_str().unwrap();
    let delegated_agent_token = delegated_agent["token"].as_str().unwrap();
    let delegated_grant = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/grants".to_owned(),
            &delegated_admin_token,
            Some(json!({
                "subject_principal_id":delegated_agent_id,
                "workspace_id":delegated_workspace_id,
                "scope":"agent",
                "scope_id":delegated_agent_id,
                "actions":["read","write","delete"],
                "expires_at":null
            })),
        ))
        .await
        .unwrap();
    assert_eq!(delegated_grant.status(), StatusCode::CREATED);
    let delegated_grant_id = response_json(delegated_grant).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let disable_owner = app
        .clone()
        .oneshot(request(
            "PATCH",
            format!("/v1/principals/{delegated_admin_id}"),
            &a.token,
            Some(json!({"status":"disabled"})),
        ))
        .await
        .unwrap();
    assert_eq!(disable_owner.status(), StatusCode::OK);
    assert_eq!(
        app.clone()
            .oneshot(request(
                "GET",
                "/v1/memories".to_owned(),
                delegated_agent_token,
                None,
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let delegated_agent_status: String =
        sqlx::query_scalar("SELECT status FROM memshare.principals WHERE tenant_id=$1 AND id=$2")
            .bind(a.tenant_id)
            .bind(Uuid::parse_str(delegated_agent_id).unwrap())
            .fetch_one(&admin_pool)
            .await
            .unwrap();
    assert_eq!(delegated_agent_status, "disabled");
    let delegated_grant_revoked: bool = sqlx::query_scalar(
        "SELECT revoked_at IS NOT NULL FROM memshare.grants WHERE tenant_id=$1 AND id=$2",
    )
    .bind(a.tenant_id)
    .bind(Uuid::parse_str(&delegated_grant_id).unwrap())
    .fetch_one(&admin_pool)
    .await
    .unwrap();
    assert!(delegated_grant_revoked);
    let blocked_reactivation = app
        .clone()
        .oneshot(request(
            "PATCH",
            format!("/v1/principals/{delegated_agent_id}"),
            &a.token,
            Some(json!({"status":"active"})),
        ))
        .await
        .unwrap();
    assert_eq!(blocked_reactivation.status(), StatusCode::BAD_REQUEST);
    let restored_agent_token = issue_token();
    let restored_agent_credential_id = Uuid::new_v4();
    let mut restore_agent = admin_pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('memshare.tenant_id',$1,true),set_config('memshare.principal_id',$2,true)")
        .bind(a.tenant_id.to_string())
        .bind(a.principal_id.to_string())
        .execute(&mut *restore_agent)
        .await
        .unwrap();
    sqlx::query("UPDATE memshare.principals SET status='active' WHERE tenant_id=$1 AND id=$2")
        .bind(a.tenant_id)
        .bind(Uuid::parse_str(delegated_agent_id).unwrap())
        .execute(&mut *restore_agent)
        .await
        .unwrap();
    sqlx::query("INSERT INTO memshare.credentials(id,tenant_id,principal_id,token_hash,status) VALUES ($1,$2,$3,$4,'active')")
        .bind(restored_agent_credential_id)
        .bind(a.tenant_id)
        .bind(Uuid::parse_str(delegated_agent_id).unwrap())
        .bind(token_hash(&restored_agent_token))
        .execute(&mut *restore_agent)
        .await
        .unwrap();
    restore_agent.commit().await.unwrap();
    let restored_agent_context = PrincipalContext {
        credential_id: restored_agent_credential_id,
        tenant_id: a.tenant_id,
        principal_id: Uuid::parse_str(delegated_agent_id).unwrap(),
        kind: "agent".to_owned(),
    };
    assert!(begin_tenant(&pool, &restored_agent_context).await.is_err());
    assert_eq!(
        app.clone()
            .oneshot(request(
                "GET",
                "/v1/memories".to_owned(),
                &restored_agent_token,
                None,
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    let service = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/principals".to_owned(),
            &a.token,
            Some(json!({"kind":"service","name":"indexer","expires_at":"2099-01-01T00:00:00Z"})),
        ))
        .await
        .unwrap();
    assert_eq!(service.status(), StatusCode::CREATED);
    let service = response_json(service).await;
    let service_id = service["principal_id"].as_str().unwrap();
    let service_credential_id = service["credential_id"].as_str().unwrap();
    let old_service_token = service["token"].as_str().unwrap();
    let original_service_expiry: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT expires_at FROM memshare.credentials WHERE id=$1")
            .bind(Uuid::parse_str(service_credential_id).unwrap())
            .fetch_one(&admin_pool)
            .await
            .unwrap();
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
    let rotated_credential_id = rotated["credential_id"].as_str().unwrap();
    let rotated_service_expiry: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT expires_at FROM memshare.credentials WHERE id=$1")
            .bind(Uuid::parse_str(rotated_credential_id).unwrap())
            .fetch_one(&admin_pool)
            .await
            .unwrap();
    assert_eq!(rotated_service_expiry, original_service_expiry);
    assert!(rotated_service_expiry.is_some());
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
    let permanent_service = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/principals".to_owned(),
            &a.token,
            Some(json!({"kind":"service","name":"permanent-indexer","expires_at":null})),
        ))
        .await
        .unwrap();
    assert_eq!(permanent_service.status(), StatusCode::CREATED);
    let permanent_service = response_json(permanent_service).await;
    let permanent_service_id = permanent_service["principal_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let permanent_rotation = app
        .clone()
        .oneshot(request(
            "POST",
            format!("/v1/principals/{permanent_service_id}/credentials"),
            &a.token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(permanent_rotation.status(), StatusCode::CREATED);
    let permanent_rotation = response_json(permanent_rotation).await;
    let permanent_expiry: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT expires_at FROM memshare.credentials WHERE id=$1")
            .bind(Uuid::parse_str(permanent_rotation["credential_id"].as_str().unwrap()).unwrap())
            .fetch_one(&admin_pool)
            .await
            .unwrap();
    assert_eq!(permanent_expiry, None);
    let first_rotation = request(
        "POST",
        format!("/v1/principals/{permanent_service_id}/credentials"),
        &a.token,
        None,
    );
    let second_rotation = request(
        "POST",
        format!("/v1/principals/{permanent_service_id}/credentials"),
        &a.token,
        None,
    );
    let (first_rotation, second_rotation) = tokio::join!(
        app.clone().oneshot(first_rotation),
        app.clone().oneshot(second_rotation)
    );
    assert_eq!(first_rotation.unwrap().status(), StatusCode::CREATED);
    assert_eq!(second_rotation.unwrap().status(), StatusCode::CREATED);
    let active_permanent_credentials: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM memshare.credentials WHERE tenant_id=$1 AND principal_id=$2 AND status='active'",
    )
    .bind(a.tenant_id)
    .bind(Uuid::parse_str(&permanent_service_id).unwrap())
    .fetch_one(&admin_pool)
    .await
    .unwrap();
    assert_eq!(active_permanent_credentials, 1);
    let recovery_service = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/principals".to_owned(),
            &a.token,
            Some(json!({"kind":"service","name":"expired-recovery","expires_at":"2099-01-01T00:00:00Z"})),
        ))
        .await
        .unwrap();
    assert_eq!(recovery_service.status(), StatusCode::CREATED);
    let recovery_service = response_json(recovery_service).await;
    let recovery_service_id = recovery_service["principal_id"]
        .as_str()
        .unwrap()
        .to_owned();
    sqlx::query("UPDATE memshare.credentials SET expires_at=now()-interval '1 second' WHERE id=$1")
        .bind(Uuid::parse_str(recovery_service["credential_id"].as_str().unwrap()).unwrap())
        .execute(&admin_pool)
        .await
        .unwrap();
    let recovered_credential = app
        .clone()
        .oneshot(request(
            "POST",
            format!("/v1/principals/{recovery_service_id}/credentials"),
            &a.token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(recovered_credential.status(), StatusCode::CREATED);
    let recovered_credential = response_json(recovered_credential).await;
    let recovered_expiry: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT expires_at FROM memshare.credentials WHERE id=$1")
            .bind(Uuid::parse_str(recovered_credential["credential_id"].as_str().unwrap()).unwrap())
            .fetch_one(&admin_pool)
            .await
            .unwrap();
    assert_eq!(recovered_expiry, None);
    let mut managed_memory = request(
        "POST",
        "/v1/memories".to_owned(),
        &a.token,
        Some(json!({
            "workspace_id":managed_workspace_id,
            "project_id":null,
            "scope":"workspace",
            "scope_id":managed_workspace_id,
            "kind":"fact",
            "content":"Membership removal regression",
            "source":{},
            "expires_at":null
        })),
    );
    managed_memory.headers_mut().insert(
        "Idempotency-Key",
        "managed-workspace-memory".parse().unwrap(),
    );
    let managed_memory = app.clone().oneshot(managed_memory).await.unwrap();
    assert_eq!(managed_memory.status(), StatusCode::CREATED);
    let managed_memory_id = response_json(managed_memory).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let managed_grant = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/grants".to_owned(),
            &a.token,
            Some(json!({
                "subject_principal_id":service_id,
                "workspace_id":managed_workspace_id,
                "scope":"workspace",
                "scope_id":managed_workspace_id,
                "actions":["read","write","delete"],
                "expires_at":null
            })),
        ))
        .await
        .unwrap();
    assert_eq!(managed_grant.status(), StatusCode::CREATED);
    let managed_grant_id = response_json(managed_grant).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        app.clone()
            .oneshot(request(
                "GET",
                format!("/v1/memories/{managed_memory_id}"),
                rotated_service_token,
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
    assert_eq!(
        app.clone()
            .oneshot(request(
                "GET",
                format!("/v1/memories/{managed_memory_id}"),
                rotated_service_token,
                None,
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let managed_grant_revoked: bool = sqlx::query_scalar(
        "SELECT revoked_at IS NOT NULL FROM memshare.grants WHERE tenant_id=$1 AND id=$2",
    )
    .bind(a.tenant_id)
    .bind(Uuid::parse_str(&managed_grant_id).unwrap())
    .fetch_one(&admin_pool)
    .await
    .unwrap();
    assert!(managed_grant_revoked);
    let mut restore_stale_grant = admin_pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('memshare.tenant_id',$1,true),set_config('memshare.principal_id',$2,true)")
        .bind(a.tenant_id.to_string())
        .bind(a.principal_id.to_string())
        .execute(&mut *restore_stale_grant)
        .await
        .unwrap();
    sqlx::query("UPDATE memshare.grants SET revoked_at=NULL WHERE tenant_id=$1 AND id=$2")
        .bind(a.tenant_id)
        .bind(Uuid::parse_str(&managed_grant_id).unwrap())
        .execute(&mut *restore_stale_grant)
        .await
        .unwrap();
    restore_stale_grant.commit().await.unwrap();
    assert_eq!(
        app.clone()
            .oneshot(request(
                "GET",
                format!("/v1/memories/{managed_memory_id}"),
                rotated_service_token,
                None,
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let stale_grant_list = app
        .clone()
        .oneshot(request(
            "GET",
            "/v1/memories?q=Membership&limit=100".to_owned(),
            rotated_service_token,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(stale_grant_list.status(), StatusCode::OK);
    assert_eq!(response_json(stale_grant_list).await["items"], json!([]));
    assert_eq!(
        app.clone()
            .oneshot(request(
                "PATCH",
                format!("/v1/memories/{managed_memory_id}"),
                rotated_service_token,
                Some(json!({"version":1,"content":"stale grant mutation"})),
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let mut stale_grant_create = request(
        "POST",
        "/v1/memories".to_owned(),
        rotated_service_token,
        Some(json!({
            "workspace_id":managed_workspace_id,
            "project_id":null,
            "scope":"workspace",
            "scope_id":managed_workspace_id,
            "kind":"fact",
            "content":"stale grant create",
            "source":{},
            "expires_at":null
        })),
    );
    stale_grant_create
        .headers_mut()
        .insert("Idempotency-Key", "stale-grant-create".parse().unwrap());
    let stale_grant_create = app.clone().oneshot(stale_grant_create).await.unwrap();
    let stale_grant_create_status = stale_grant_create.status();
    let stale_grant_create_body = response_json(stale_grant_create).await;
    assert_eq!(
        stale_grant_create_status,
        StatusCode::FORBIDDEN,
        "unexpected response: {stale_grant_create_body}"
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
                "POST",
                "/v1/grants".to_owned(),
                &a.token,
                Some(json!({
                    "subject_principal_id":service_id,
                    "workspace_id":managed_workspace_id,
                    "scope":"workspace",
                    "scope_id":managed_workspace_id,
                    "actions":["read"],
                    "expires_at":null
                })),
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

    let mut collision_body = replay_body.clone();
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

    let mut expire_memory_key = admin_pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('memshare.tenant_id',$1,true),set_config('memshare.principal_id',$2,true)")
        .bind(a.tenant_id.to_string())
        .bind(a.principal_id.to_string())
        .execute(&mut *expire_memory_key)
        .await
        .unwrap();
    sqlx::query("UPDATE memshare.idempotency SET expires_at=now()-interval '1 second' WHERE tenant_id=$1 AND principal_id=$2 AND operation='memory.create' AND key='tenant-a-memory'")
        .bind(a.tenant_id)
        .bind(a.principal_id)
        .execute(&mut *expire_memory_key)
        .await
        .unwrap();
    expire_memory_key.commit().await.unwrap();
    let mut expired_memory_reuse = request(
        "POST",
        "/v1/memories".to_owned(),
        &a.token,
        Some(replay_body),
    );
    expired_memory_reuse
        .headers_mut()
        .insert("Idempotency-Key", "tenant-a-memory".parse().unwrap());
    let expired_memory_reuse = app.clone().oneshot(expired_memory_reuse).await.unwrap();
    assert_eq!(expired_memory_reuse.status(), StatusCode::CREATED);
    assert_ne!(
        response_json(expired_memory_reuse).await["id"],
        memory["id"]
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
        Some(openai_call.clone()),
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

    let mut expire_handoff_key = admin_pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('memshare.tenant_id',$1,true),set_config('memshare.principal_id',$2,true)")
        .bind(a.tenant_id.to_string())
        .bind(Uuid::parse_str(source_agent_id).unwrap().to_string())
        .execute(&mut *expire_handoff_key)
        .await
        .unwrap();
    sqlx::query("UPDATE memshare.idempotency SET expires_at=now()-interval '1 second' WHERE tenant_id=$1 AND principal_id=$2 AND operation='handoff.create' AND key='cross-llm-handoff'")
        .bind(a.tenant_id)
        .bind(Uuid::parse_str(source_agent_id).unwrap())
        .execute(&mut *expire_handoff_key)
        .await
        .unwrap();
    expire_handoff_key.commit().await.unwrap();
    let mut expired_handoff_reuse = request(
        "POST",
        "/v1/adapters/openai/handoffs".to_owned(),
        source_agent_token,
        Some(openai_call),
    );
    expired_handoff_reuse
        .headers_mut()
        .insert("Idempotency-Key", "cross-llm-handoff".parse().unwrap());
    let expired_handoff_reuse = app.clone().oneshot(expired_handoff_reuse).await.unwrap();
    assert_eq!(expired_handoff_reuse.status(), StatusCode::CREATED);
    assert_ne!(response_json(expired_handoff_reuse).await["id"], handoff_id);

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
    let target_grants_active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM memshare.grants WHERE tenant_id=$1 AND workspace_id=$2 AND scope='agent' AND scope_id=$3 AND revoked_at IS NULL",
    )
    .bind(a.tenant_id)
    .bind(a.workspace_id)
    .bind(Uuid::parse_str(target_agent_id).unwrap())
    .fetch_one(&admin_pool)
    .await
    .unwrap();
    assert_eq!(target_grants_active, 0);

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
    sqlx::query(&format!("DROP ROLE {runtime_role}"))
        .execute(&control_pool)
        .await
        .expect("drop isolated runtime role");
    control_pool.close().await;
}
use std::str::FromStr;
