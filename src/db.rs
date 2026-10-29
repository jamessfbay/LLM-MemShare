use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};
use sqlx::{migrate::Migrator, postgres::PgPoolOptions, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    error::{AppError, AppResult},
    model::PrincipalContext,
};

static MIGRATOR: Migrator = sqlx::migrate!();

pub async fn connect(database_url: &str, max_connections: u32) -> AppResult<PgPool> {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(10))
        .connect(database_url)
        .await
        .map_err(AppError::database)
}

pub async fn migrate(pool: &PgPool) -> AppResult<()> {
    MIGRATOR
        .run(pool)
        .await
        .map_err(|error| AppError::config(format!("migration failed: {error}")))
}

pub async fn verify_runtime_role(pool: &PgPool) -> AppResult<()> {
    let (role, superuser, bypass_rls, create_role, create_db, owns_tables, can_authenticate): (
        String,
        bool,
        bool,
        bool,
        bool,
        bool,
        bool,
    ) = sqlx::query_as(
        r#"
SELECT current_user::text,
       r.rolsuper,
       r.rolbypassrls,
       r.rolcreaterole,
       r.rolcreatedb,
       EXISTS (
         SELECT 1
         FROM pg_class c
         JOIN pg_namespace n ON n.oid=c.relnamespace
         WHERE n.nspname='memshare' AND c.relkind IN ('r','p') AND c.relowner=r.oid
       ),
       COALESCE(has_function_privilege(current_user,to_regprocedure('memshare.authenticate_request(text)'),'EXECUTE'),false)
FROM pg_roles r
WHERE r.rolname=current_user
        "#,
    )
    .fetch_one(pool)
    .await?;
    if superuser || bypass_rls || create_role || create_db || owns_tables {
        return Err(AppError::config(format!(
            "unsafe runtime database role {role}: serve requires a non-owner, NOSUPERUSER, NOBYPASSRLS, NOCREATEROLE, NOCREATEDB role"
        )));
    }
    if !can_authenticate {
        return Err(AppError::config(format!(
            "runtime database role {role} cannot execute memshare.authenticate_request(text); apply migration 0005 and its runtime grant"
        )));
    }
    Ok(())
}

pub fn token_hash(token: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(token.as_bytes());
    format!("{:x}", digest.finalize())
}

pub fn issue_token() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub async fn bootstrap(
    pool: &PgPool,
    tenant_slug: &str,
    workspace_slug: &str,
    user_name: &str,
    token: &str,
) -> AppResult<(Uuid, Uuid, Uuid)> {
    provision_tenant(pool, tenant_slug, workspace_slug, user_name, token, true).await
}

pub async fn provision_tenant(
    pool: &PgPool,
    tenant_slug: &str,
    workspace_slug: &str,
    user_name: &str,
    token: &str,
    first_only: bool,
) -> AppResult<(Uuid, Uuid, Uuid)> {
    if token.len() < 32 {
        return Err(AppError::invalid(
            "MEMSHARE_BOOTSTRAP_TOKEN must contain at least 32 bytes",
        ));
    }
    validate_name("tenant", tenant_slug)?;
    validate_name("workspace", workspace_slug)?;
    validate_name("user", user_name)?;

    let tenant_id = Uuid::new_v4();
    let workspace_id = Uuid::new_v4();
    let principal_id = Uuid::new_v4();
    let credential_id = Uuid::new_v4();
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(716092331)")
        .execute(&mut *tx)
        .await?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memshare.credentials)")
        .fetch_one(&mut *tx)
        .await?;
    if first_only && exists {
        return Err(AppError::conflict(
            "already_initialized",
            "A tenant already exists; use tenant provisioning",
        ));
    }
    set_tenant_context_raw(&mut tx, tenant_id, principal_id).await?;
    sqlx::query("INSERT INTO memshare.tenants(id, slug, status) VALUES ($1,$2,'active')")
        .bind(tenant_id)
        .bind(tenant_slug)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO memshare.tenant_limits(tenant_id) VALUES ($1)")
        .bind(tenant_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO memshare.principals(tenant_id,id,kind,name,status,tenant_role) VALUES ($1,$2,'user',$3,'active','admin')")
        .bind(tenant_id)
        .bind(principal_id)
        .bind(user_name)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO memshare.workspaces(tenant_id,id,slug,status) VALUES ($1,$2,$3,'active')",
    )
    .bind(tenant_id)
    .bind(workspace_id)
    .bind(workspace_slug)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO memshare.workspace_memberships(tenant_id,workspace_id,principal_id,role,status) VALUES ($1,$2,$3,'admin','active')")
        .bind(tenant_id)
        .bind(workspace_id)
        .bind(principal_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO memshare.credentials(id,tenant_id,principal_id,token_hash,status) VALUES ($1,$2,$3,$4,'active')")
        .bind(credential_id)
        .bind(tenant_id)
        .bind(principal_id)
        .bind(token_hash(token))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok((tenant_id, workspace_id, principal_id))
}

pub async fn begin_tenant(
    pool: &PgPool,
    principal: &PrincipalContext,
) -> AppResult<Transaction<'static, Postgres>> {
    let mut tx = pool.begin().await?;
    set_tenant_context_raw(&mut tx, principal.tenant_id, principal.principal_id).await?;
    let active: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM memshare.principals p JOIN memshare.tenants t ON t.id=p.tenant_id LEFT JOIN memshare.principals owner ON owner.tenant_id=p.tenant_id AND owner.id=p.owner_principal_id WHERE p.tenant_id=$1 AND p.id=$2 AND p.status='active' AND t.status='active' AND (p.kind<>'agent' OR owner.status='active'))",
    )
    .bind(principal.tenant_id)
    .bind(principal.principal_id)
    .fetch_one(&mut *tx)
    .await?;
    if !active {
        return Err(AppError::unauthorized());
    }
    Ok(tx)
}

pub async fn record_request_audit(
    pool: &PgPool,
    principal: &PrincipalContext,
    action: &str,
    target_type: &str,
    outcome: &str,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    set_tenant_context_raw(&mut tx, principal.tenant_id, principal.principal_id).await?;
    sqlx::query("INSERT INTO memshare.audit_events(tenant_id,id,actor_principal_id,credential_id,action,outcome,target_type,request_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)")
        .bind(principal.tenant_id)
        .bind(Uuid::new_v4())
        .bind(principal.principal_id)
        .bind(principal.credential_id)
        .bind(action)
        .bind(outcome)
        .bind(target_type)
        .bind(&principal.request_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

async fn set_tenant_context_raw(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    principal_id: Uuid,
) -> AppResult<()> {
    sqlx::query("SELECT set_config('memshare.tenant_id', $1, true), set_config('memshare.principal_id', $2, true)")
        .bind(tenant_id.to_string())
        .bind(principal_id.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

fn validate_name(field: &str, value: &str) -> AppResult<()> {
    if value.is_empty()
        || value.len() > 100
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(AppError::invalid(format!(
            "{field} must be 1-100 ASCII letters, digits, dots, underscores, or hyphens"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_random_url_safe_and_long_enough() {
        let a = issue_token();
        let b = issue_token();
        assert_ne!(a, b);
        assert!(a.len() >= 43);
        assert!(a
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn bootstrap_names_are_bounded() {
        assert!(validate_name("tenant", "acme-prod_1").is_ok());
        assert!(validate_name("tenant", "bad space").is_err());
    }
}
