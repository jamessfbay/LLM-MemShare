use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::Duration,
};

use clap::{Parser, Subcommand};
use llm_memshare::{auth::AppState, db, router, Config};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser)]
#[command(name = "memshare", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(long, env = "MEMSHARE_BIND")]
        bind: Option<SocketAddr>,
    },
    Migrate,
    Bootstrap {
        #[arg(long)]
        tenant: String,
        #[arg(long)]
        workspace: String,
        #[arg(long)]
        user: String,
    },
    ProvisionTenant {
        #[arg(long)]
        tenant: String,
        #[arg(long)]
        workspace: String,
        #[arg(long)]
        user: String,
    },
    Healthcheck {
        #[arg(long, default_value = "127.0.0.1:8080")]
        address: SocketAddr,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "llm_memshare=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer().json())
        .init();
    let cli = Cli::parse();
    if let Command::Healthcheck { address } = &cli.command {
        healthcheck(*address)?;
        return Ok(());
    }
    let config = Config::from_env().map_err(anyhow::Error::msg)?;
    let pool = db::connect(&config.database_url, config.max_connections)
        .await
        .map_err(anyhow::Error::msg)?;
    match cli.command {
        Command::Migrate => db::migrate(&pool).await.map_err(anyhow::Error::msg)?,
        Command::Bootstrap {
            tenant,
            workspace,
            user,
        } => {
            let token = std::env::var("MEMSHARE_BOOTSTRAP_TOKEN")
                .map_err(|_| anyhow::anyhow!("MEMSHARE_BOOTSTRAP_TOKEN is required"))?;
            let (tenant_id, workspace_id, principal_id) =
                db::bootstrap(&pool, &tenant, &workspace, &user, &token)
                    .await
                    .map_err(anyhow::Error::msg)?;
            println!(
                "tenant_id={tenant_id}\nworkspace_id={workspace_id}\nprincipal_id={principal_id}"
            );
        }
        Command::ProvisionTenant {
            tenant,
            workspace,
            user,
        } => {
            let token = db::issue_token();
            let (tenant_id, workspace_id, principal_id) =
                db::provision_tenant(&pool, &tenant, &workspace, &user, &token, false)
                    .await
                    .map_err(anyhow::Error::msg)?;
            println!("tenant_id={tenant_id}\nworkspace_id={workspace_id}\nprincipal_id={principal_id}\ntoken={token}");
        }
        Command::Serve { bind } => {
            db::verify_runtime_role(&pool)
                .await
                .map_err(anyhow::Error::msg)?;
            let bind = bind.unwrap_or(config.bind);
            let listener = tokio::net::TcpListener::bind(bind).await?;
            tracing::info!(%bind,"memshare listening");
            let public_base_url = config
                .public_base_url
                .unwrap_or_else(|| format!("http://{bind}"));
            let state = AppState::new(pool).with_public_base_url(public_base_url);
            axum::serve(listener, router(state))
                .with_graceful_shutdown(shutdown())
                .await?;
        }
        Command::Healthcheck { .. } => unreachable!("healthcheck exits before database setup"),
    }
    Ok(())
}

fn healthcheck(address: SocketAddr) -> anyhow::Result<()> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream
        .write_all(b"GET /health/ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut response = [0_u8; 128];
    let size = stream.read(&mut response)?;
    let status = std::str::from_utf8(&response[..size])?;
    if !status.starts_with("HTTP/1.1 200") {
        anyhow::bail!("readiness endpoint did not return HTTP 200");
    }
    Ok(())
}

async fn shutdown() {
    let ctrl_c = async { tokio::signal::ctrl_c().await.expect("ctrl-c handler") };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {_ = ctrl_c=>{},_ = terminate=>{}}
}
