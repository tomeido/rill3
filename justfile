set dotenv-load := true

default:
    @just --list

fmt:
    cargo fmt --all -- --check

lint:
    cargo clippy --workspace --all-targets --locked -- -D warnings

test:
    cargo test --workspace --all-targets --locked

check: fmt lint test sqlx-check compose-config budgets

server:
    cargo run --locked -p rill3 -- server

worker:
    cargo run --locked -p rill3 -- worker

db-up:
    docker compose -f deploy/compose.yml up -d postgres

db-migrate:
    cargo sqlx migrate run --source migrations

sqlx-prepare:
    cargo sqlx prepare --workspace -- --all-targets

sqlx-check:
    cargo sqlx prepare --workspace --check -- --all-targets
    SQLX_OFFLINE=true cargo check --workspace --all-targets --locked

compose-config:
    docker compose -f deploy/compose.yml config --quiet

budgets:
    bash tests/check-budgets.sh

up:
    docker compose -f deploy/compose.yml up --build

down:
    docker compose -f deploy/compose.yml down

logs:
    docker compose -f deploy/compose.yml logs -f server worker

