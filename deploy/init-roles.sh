#!/bin/sh
set -eu

: "${MEMSHARE_RUNTIME_PASSWORD:?MEMSHARE_RUNTIME_PASSWORD is required}"
: "${MEMSHARE_OWNER_PASSWORD:?MEMSHARE_OWNER_PASSWORD is required}"

psql --set=ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" \
  --set=owner_password="$MEMSHARE_OWNER_PASSWORD" \
  --set=runtime_password="$MEMSHARE_RUNTIME_PASSWORD" <<'SQL'
CREATE ROLE memshare_owner LOGIN NOBYPASSRLS NOSUPERUSER NOCREATEDB NOCREATEROLE
  PASSWORD :'owner_password';
CREATE ROLE memshare_runtime LOGIN NOBYPASSRLS NOSUPERUSER NOCREATEDB NOCREATEROLE
  PASSWORD :'runtime_password';
CREATE SCHEMA IF NOT EXISTS memshare AUTHORIZATION memshare_owner;
ALTER ROLE memshare_owner SET search_path TO memshare, pg_catalog;
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
GRANT CONNECT ON DATABASE memshare TO memshare_owner;
GRANT CREATE ON DATABASE memshare TO memshare_owner;
GRANT CONNECT ON DATABASE memshare TO memshare_runtime;
GRANT USAGE ON SCHEMA memshare TO memshare_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE memshare_owner IN SCHEMA memshare
  GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO memshare_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE memshare_owner IN SCHEMA memshare
  GRANT EXECUTE ON FUNCTIONS TO memshare_runtime;
SQL
