# M0/M1 decisions

This log complements the durable ADRs.

## 2026-09-02

- Pin Rust 1.96.1 and direct dependency versions. Keep `Cargo.lock` committed for the application workspace.
- Start as one binary with `server`, `worker`, and an `indexer` boundary; no process-level microservice split is required to develop or deploy it.
- Treat PostgreSQL observations as the public source of truth. Public request handlers cannot hold a provider client and therefore cannot accidentally make outbound provider calls.
- Use text columns plus application/check-constraint validation for provider and status values instead of PostgreSQL native enums.
- Persist provider payload hashes, delivery identity, normalized state, and observation time—not raw webhook/API bodies.
- Refuse to start production server/worker roles without `DATABASE_URL`; tests cover health routing with a deliberately unavailable pool.
- Require a trusted absolute public HTTPS origin in production. Derive YouTube `origin` from it and validate Twitch `parent` as a hostname-only configuration value.
- Render CHZZK and generic providers as link-only until an official documented embed contract is approved.
- Ship zero base application JavaScript in M0/M1.

