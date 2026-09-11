# Operations

This runbook covers the M0/M1 deployment only. RILL3 currently runs one image in two active roles: `rill3 server` serves SSR/API/webhook traffic, and `rill3 worker` reconciles provider state. The `indexer` command is a compiling boundary and must not be deployed as a production service before M3.

## Public-release gate

The current Compose volumes provide local persistence, not backups. Automated, encrypted, off-host PostgreSQL backups and a successfully timed restore drill are mandatory before entering M6 stabilization work or accepting public production traffic. Until that work exists, this stack is suitable for development and staging only.

Before public beta, operators must define and verify an RPO, RTO, retention period, backup encryption and access policy, failure alerting, and an isolated restore procedure. A backup is not considered valid until it has been restored into a clean environment and application-level checks have passed. Future object-store content must be added to the same recovery plan before alerts or media are enabled.

Never use `docker compose down --volumes` on an environment containing data that must survive. The PostgreSQL named volume, a filesystem snapshot on the same host, and Caddy's certificate volume are not substitutes for an off-host database backup.

## Runtime topology

| Component | Responsibility | Availability signal |
| --- | --- | --- |
| Caddy | TLS termination, security headers, compression, reverse proxy | External requests reach server health routes |
| `server` | SSR, public JSON, provider webhook ingress | `/health/live` and `/health/ready` |
| `worker` | Bounded provider reconciliation and stale-state updates | Structured logs plus successful reconciliation timestamps |
| PostgreSQL | Source of truth and advisory lock | `pg_isready`, then server readiness |

The worker intentionally has no HTTP health port. Do not add a public worker endpoint solely for orchestration. Use its structured completion/error events and a one-shot staging run to prove provider access.

## Configuration and secrets

Start from `.env.example`, but replace every development credential before a shared deployment. Keep the real environment file outside version control with owner-only permissions, or inject values from the deployment platform's secret store. Do not paste the output of `docker compose config` into tickets or logs because it expands the database URL and provider credentials.

Production configuration must use:

- an HTTPS `RILL3_PUBLIC_ORIGIN` matching the public site exactly;
- a hostname-only `RILL3_TWITCH_PARENT` matching the Twitch embed parent;
- a unique, high-entropy PostgreSQL password and provider webhook tokens;
- separate provider applications and credentials for staging and production;
- `RILL3_DB_MAX_CONNECTIONS` at or below the application hard limit of 20.

Provider secrets, database URLs, authorization headers, webhook signatures, raw webhook bodies, OAuth material, and future donation messages must never be logging fields. If any appears in logs or CI output, restrict access to the affected logs and rotate the credential immediately.

## Deploying server and worker

Run releases from a reviewed commit with a committed `Cargo.lock`. Do not deploy a working tree containing unreviewed changes.

1. Verify CI passed formatting, clippy, tests, fresh-database migrations, SQLx offline compilation, Compose validation, and browser budgets.
2. Validate staging configuration without publishing its expanded output:

   ```sh
   docker compose --env-file /secure/path/rill3.env -f deploy/compose.yml config --quiet
   ```

3. Take and verify the required pre-deployment database backup. Until the pre-M6 backup workflow exists, this step cannot be satisfied for public production.
4. Start PostgreSQL and wait for its health check:

   ```sh
   docker compose --env-file /secure/path/rill3.env -f deploy/compose.yml up -d --wait postgres
   ```

5. Apply migrations once from a trusted release checkout or migration job using the target database URL:

   ```sh
   DATABASE_URL='postgres://…' cargo sqlx migrate run --source migrations
   ```

   The current server and worker also run embedded pending migrations during startup. The explicit migration step is still preferred in a controlled release so schema failure happens before traffic is switched.

6. Build and start the application roles, then Caddy:

   ```sh
   docker compose --env-file /secure/path/rill3.env -f deploy/compose.yml up -d --build --wait server worker caddy
   ```

7. Check health, one public page, `/live.json`, provider state, and the structured logs before completing the release.

For repeatable production releases, build the image once in CI, identify it by an immutable digest, and deploy that same digest to staging and production. The local `--build` flow above is the current single-host M0/M1 path, not the final supply-chain design.

## Health and shutdown

`GET /health/live` proves that the server process and HTTP runtime are alive. It does not query PostgreSQL. `GET /health/ready` queries PostgreSQL and returns failure when the server cannot safely serve database-backed traffic.

```sh
curl -fsS https://staging.example/health/live
curl -fsS https://staging.example/health/ready
```

Treat these signals differently:

- liveness failure: inspect the process/container and recent panic or startup logs;
- readiness-only failure: inspect PostgreSQL health, connection limits, DNS, credentials, migration status, and disk capacity before restarting the app;
- a healthy server with stale providers: follow the provider procedure below rather than restarting PostgreSQL.

Compose uses an init process, sends `SIGTERM`, and allows 30 seconds before forced termination. The server stops accepting new work and drains through Axum's graceful-shutdown path; the worker receives the same cancellation signal and exits its bounded loop. Use a normal stop and watch for the structured `shutdown signal received` event:

```sh
docker compose --env-file /secure/path/rill3.env -f deploy/compose.yml stop -t 30 worker server
```

Avoid `SIGKILL` except when the grace period has demonstrably failed. During a full shutdown, stop application roles before PostgreSQL.

## Provider-disabled and stale states

A missing Twitch or CHZZK credential deliberately disables only that provider. A missing YouTube WebSub token disables authenticated callback handling while manual video mode remains separate. Disabled providers must be visible as disabled; they must not make server liveness or database readiness fail.

When a provider times out, returns 429/5xx, or cannot be reached, RILL3 preserves its last-known observation and marks it stale instead of fabricating an offline transition. Operators must not manually rewrite those sessions to offline.

For a disabled or stale provider:

1. Confirm whether the state is `disabled` or `stale` in the UI/API and logs.
2. Check the provider's official status page, credential validity and scopes, callback registration, rate-limit response, DNS/TLS reachability, and host clock.
3. Ensure another provider continues reconciling; one adapter failure must not stop the worker.
4. Correct the secret or upstream condition, restart only the worker if configuration changed, and run a bounded reconciliation check:

   ```sh
   DATABASE_URL='postgres://…' cargo run --locked -p rill3 -- worker --once
   ```

5. Verify a fresh observation clears the stale presentation. Do not suppress repeated 401/403, webhook-signature, or replay errors by widening validation.

## Migration and rollback policy

Migrations are forward-only and immutable after application to any shared environment. Never edit, reorder, or delete an applied migration. CI must continue testing both a completely fresh database and SQLx offline compilation.

There is no automatic down-migration path. If a migration or release is faulty:

- prefer a new timestamped forward-fix migration;
- roll the application image back only when the deployed schema is explicitly backward-compatible;
- restore the database from the verified pre-deployment backup only as an incident decision, with downtime and the expected data-loss window communicated;
- test the correction against both a fresh database and a copy upgraded from the previous release.

Potentially destructive changes require an expand/backfill/verify/contract sequence across releases. Dropping a table or column, rewriting a large table, or changing a constraint in place is not an acceptable one-step deployment.

## Backup and restore preparation

Before M6 begins, the implementation must automate at least the following controls:

- encrypted off-host PostgreSQL backups, with alerting on missed or failed jobs;
- retention and deletion rules matching the documented data-retention policy;
- a method appropriate to the chosen RPO, including WAL/PITR if snapshots or logical dumps cannot meet it;
- backup credentials isolated from normal application credentials;
- scheduled restoration into a clean, isolated environment;
- verification of migrations, row counts/invariants, readiness, representative pages, and provider state after restore;
- recorded restore duration and operator sign-off against the RTO.

A recovery drill should restore into a new PostgreSQL instance, point a staging server and worker at it, verify data before enabling external callbacks, and only then exercise a controlled cutover. If recovery follows a credential compromise, rotate database and provider credentials before reconnecting ingress.

## Logs and incident evidence

RILL3 and Caddy write structured JSON logs to stdout. Preserve timestamps, service name, request ID, provider kind, normalized error category, retry/backoff information, and deployment revision where available. Use bounded queries such as:

```sh
docker compose --env-file /secure/path/rill3.env -f deploy/compose.yml logs --since 30m server worker caddy
```

Do not enable raw request/body logging to debug webhooks. Capture provider delivery IDs, hashes, HTTP status classes, and validation outcomes instead. Restrict operational log access and define retention before public beta. Exporting logs or traces to a third party requires a privacy and secret-redaction review.

## Staging credential smoke-test checklist

Complete this checklist with staging-only accounts after every credential, callback, provider SDK, or ingress-policy change.

### Common

- [ ] Staging uses a separate HTTPS hostname, provider applications, database, and secrets.
- [ ] DNS, certificate chain, `RILL3_PUBLIC_ORIGIN`, and `RILL3_TWITCH_PARENT` match exactly.
- [ ] Host time is synchronized; webhook timestamp/replay checks are not relaxed.
- [ ] A clean database migrates successfully and both health endpoints have the expected semantics.
- [ ] Startup logs identify enabled and disabled providers without printing any credential.
- [ ] Restarting server and worker preserves normalized state and does not duplicate a session/event.
- [ ] Invalid credentials affect only their provider; public pages and other providers remain available.

### Twitch

- [ ] Helix verifies a registered staging channel and a bounded reconciliation completes.
- [ ] EventSub challenge, valid signed online/offline delivery, duplicate delivery, bad signature, and expired timestamp behave as designed.
- [ ] The official player loads with the staging hostname as `parent`; no untrusted parent value is accepted.
- [ ] A staged 429/5xx or credential revocation produces backoff/stale behavior without stopping the worker.

### YouTube

- [ ] The callback accepts only the configured staging token and expected topic/challenge.
- [ ] A notification acts only as a reconciliation signal and its raw body is not retained.
- [ ] A validated manual video ID embeds with the exact staging `origin`.
- [ ] No public-search polling, arbitrary URL fetch, or redirect following occurs.

### CHZZK and link-only

- [ ] Official CHZZK credentials reconcile a registered channel; missing/revoked credentials produce disabled or stale status.
- [ ] CHZZK renders link-only and never produces an iframe.
- [ ] Link-only input rejects non-HTTPS, credential-bearing, localhost, private, link-local, and unspecified literal-IP URLs.
- [ ] Stored link-only URLs are never fetched by server or worker.

Record the release revision, test time, operator, provider delivery identifiers, and sanitized results. Revoke or rotate staging credentials immediately if they were exposed during testing.
