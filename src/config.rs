use std::{env, net::SocketAddr};

use crate::error::{AppError, AppResult};

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub bind: SocketAddr,
    pub max_connections: u32,
}

impl Config {
    pub fn from_env() -> AppResult<Self> {
        let database_url = env::var("MEMSHARE_DATABASE_URL")
            .map_err(|_| AppError::config("MEMSHARE_DATABASE_URL is required"))?;
        let bind = env::var("MEMSHARE_BIND")
            .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
            .parse()
            .map_err(|_| AppError::config("MEMSHARE_BIND must be a socket address"))?;
        let max_connections: u32 = env::var("MEMSHARE_DATABASE_MAX_CONNECTIONS")
            .unwrap_or_else(|_| "20".to_owned())
            .parse()
            .map_err(|_| {
                AppError::config("MEMSHARE_DATABASE_MAX_CONNECTIONS must be an integer")
            })?;
        if !(1..=200).contains(&max_connections) {
            return Err(AppError::config(
                "MEMSHARE_DATABASE_MAX_CONNECTIONS must be between 1 and 200",
            ));
        }

        Ok(Self {
            database_url,
            bind,
            max_connections,
        })
    }
}
