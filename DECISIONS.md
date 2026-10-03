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


## 2026-09-29 — Broadcaster registration and claimable donations

- Extend the original discovery-only scope with public channel registration and a separately configured Web3 payment path, as requested by the product owner.
- Use one immutable ETH vault per canonical provider/channel identity. This explicitly changes the original direct-forwarding/no-escrow plan: donations remain in the channel vault until its verified owner withdraws.
- Prove channel ownership with official OAuth, separately from legacy provider validation flags. Bind a browser session, one-use wallet signature challenge, deployment, channel and permanent beneficiary.
- The backend attestor is trusted only for initial ownership assignment. It has no withdrawal or owner-change mechanism after a claim. No donor or broadcaster spending key is held by the service.
- Enable only configured, bytecode-pinned standard-EVM deployments. Ethereum/Sepolia, Base/Base Sepolia and local Anvil are supported; Abstract-specific deployment and smart-wallet signatures require a separate adapter and validation.
- Keep wallet scripts on registration/support pages; discovery pages retain their existing no-JavaScript rendering.
