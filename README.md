# RILL3 — 릴쓰리

> 방송은 어디서 하든, 후원은 직접·투명하게.  
> Stream anywhere. Tip directly.

RILL3 is a Rust-first discovery layer for live streams hosted by Twitch, YouTube, CHZZK, and link-only providers. M0/M1 discovers registered streams through documented provider interfaces, renders lightweight server-side pages, and never proxies video bytes. Wallets, tips, TTS, and OBS alerts deliberately remain out of scope until later milestones.

## What works in M0/M1

- One `rill3` binary with `server`, `worker`, operator `channels register` / `channels list`, `indexer` (stub), and development-only `seed-demo` commands
- PostgreSQL-backed creators, external channels, normalized live sessions, and idempotent provider deliveries
- Twitch Helix reconciliation and signed EventSub ingress
- YouTube and link-only manual broadcasts with explicit status, bounded expiry, and a visible manual-source label; YouTube OAuth/liveBroadcasts remains an explicit TODO
- Best-effort polling of the official CHZZK global live directory in link-only display mode
- Strict HTTPS link-only channels that are stored but never fetched
- Askama-rendered `/`, paginated `/channels`, and `/c/{slug}?channel=<uuid>`, plus `/live.json` with ETag support
- Always-visible CHZZK, YouTube, Twitch, and SOOP shortcuts, plus direct broadcast links on cards and creator pages
- Channel-specific detail links and connected-channel navigation; offline and unknown channels remain discoverable
- Compact populated home pages, relative broadcast times, and a refresh link
- Liveness/readiness, request IDs, structured logs, security headers, graceful shutdown, Compose/Caddy, and CI

## Prerequisites

- Rust 1.96.1 (the repository toolchain file selects it)
- Docker with Compose for the normal local stack
- Optional: `just`, `cargo-sqlx`, `oha`, and Brotli CLI for convenience and extended checks

## Run locally with Compose

```sh
cp .env.example .env
docker compose -f deploy/compose.yml up --build --wait
docker compose -f deploy/compose.yml exec server /usr/local/bin/rill3 seed-demo
curl -fsS http://localhost:8080/health/live
curl -fsS http://localhost:8080/health/ready
```

Open <http://localhost:8080>. The seed command refuses to run when `RILL3_ENVIRONMENT=production`.

Stop the stack without deleting its database:

```sh
docker compose -f deploy/compose.yml down
```

## Run from Cargo

Start PostgreSQL, then export the development connection string:

```sh
export DATABASE_URL=postgres://rill3:rill3@127.0.0.1:5432/rill3
cargo run -p rill3 -- seed-demo
cargo run -p rill3 -- server
```

In another terminal:

```sh
export DATABASE_URL=postgres://rill3:rill3@127.0.0.1:5432/rill3
cargo run -p rill3 -- worker
```

`server` and `worker` run migrations before serving. `worker --once` performs one bounded reconciliation pass and exits. Missing, incomplete, or invalid provider credential sets disable the affected provider without preventing other providers from reconciling. Server connection labels use the same credential checks as the worker.

## Register real channels

Use `channels register` with a creator slug, display name, provider, and provider
channel ID. `channels list --json` returns a bounded page of registrations and a
`next_cursor`; continue with `--after <uuid>`. Both commands accept the database
configuration through the environment. Registration validates the selected
provider before atomically saving the creator and channel. Re-registration
preserves IDs, the existing creator profile, and channel ownership.

YouTube and link-only registrations default to unknown. To mark one online,
provide both `--live-status online` and `--live-url`. Manual online/offline state
expires after 120 minutes by default; `--live-ttl-minutes` accepts 1–1440.
Renew by repeating the full registration command while the broadcast is still
live, or end it with `--live-status offline`. A stored video URL alone never
implies LIVE. YouTube registration validates channel and video URL syntax;
live existence and creator OAuth ownership are not verified.

See [the operator channel guide](docs/CHANNELS.md) for production Compose
commands, provider credentials, renewal, and pagination. All example IDs there
are placeholders. The current production database has zero creators and
channels; actual channel information has not been supplied for registration.
Development fixtures must not be inserted into production.

## Configuration

All configuration is supplied through CLI flags or environment variables. Secrets are not accepted in tracked configuration files.

| Variable | Role | Default |
| --- | --- | --- |
| `DATABASE_URL` | PostgreSQL URL | required |
| `RILL3_BIND_ADDR` | server listen address | `127.0.0.1:3000` |
| `RILL3_PUBLIC_ORIGIN` | trusted YouTube origin / canonical origin | `http://localhost:3000` |
| `RILL3_BASE_PATH` | optional public route prefix, e.g. `/rill3`; proxy must preserve it | empty |
| `RILL3_TWITCH_PARENT` | trusted Twitch embed parent hostname | `localhost` |
| `RILL3_DB_MAX_CONNECTIONS` | pool size, hard maximum 20 | `10` |
| `RILL3_ENVIRONMENT` | `development`, `test`, or `production` | `development` |
| `RILL3_WORKER_INTERVAL_SECONDS` | base reconciliation interval (minimum 30) | `60` |
| `RILL3_PROVIDER_TIMEOUT_SECONDS` | timeout for one provider batch (CHZZK has a 60s safety floor) | `12` |
| `TWITCH_CLIENT_ID` / `TWITCH_CLIENT_SECRET` | official Twitch app credentials | unset / disabled |
| `TWITCH_EVENTSUB_SECRET` | EventSub HMAC secret; required by the current Twitch provider setup | unset / Twitch disabled |
| `CHZZK_CLIENT_ID` / `CHZZK_CLIENT_SECRET` | official CHZZK developer credentials | unset / disabled |
| `YOUTUBE_WEBSUB_TOKEN` | callback verification token | unset / endpoint unavailable |
| `YOUTUBE_WEBSUB_SECRET` | raw-body WebSub HMAC secret | unset / notification endpoint unavailable |

Production configuration requires an HTTPS public origin. Put TLS/Caddy/CDN in front of the app and use distinct, generated database and webhook secrets.

For the shared server at <https://domeido.asuscomm.com/rill3>, see
[the self-hosted deployment runbook](docs/SELFHOST.md). Keep
`RILL3_PUBLIC_ORIGIN=https://domeido.asuscomm.com` and set
`RILL3_BASE_PATH=/rill3`; the origin used by video embeds must not contain a path.

## Routes

| Method/path | Purpose |
| --- | --- |
| `GET /health/live` | process liveness; independent of PostgreSQL |
| `GET /health/ready` | PostgreSQL readiness only |
| `GET /` | up to 60 live cards, a registered-channel preview, and distinct empty/offline messages; zero iframe |
| `GET /channels?after=<uuid>` | paginated active registered channels, including offline and unknown states |
| `GET /c/{slug}?channel=<uuid>` | the selected enabled channel's official embed or external link, plus connected-channel navigation |
| `GET /live.json` | normalized online snapshot with ETag, status source, and manual expiry |
| `POST /webhooks/twitch` | signed Twitch EventSub deliveries |
| `GET/POST /webhooks/youtube` | validated YouTube WebSub callback/challenge |
| `GET /openapi.json` | machine-readable HTTP surface |

There is no public channel-management write API. Registration is an operator
CLI command; local fixture insertion remains a separate development command.
Omitting `channel` on a creator page selects its preferred channel. Supplying
it selects only an enabled channel belonging to that creator; another creator's
or a disabled channel returns 404, and a malformed UUID returns 400.

Platform shortcuts remain available when the live list is empty or provider credentials
are absent. SOOP is a directory shortcut only; it does not add an automatic provider
integration. Broadcast links prefer the current YouTube video, CHZZK live player, or
registered link-only live URL while online, and fall back to the channel otherwise.
The same validated destination appears as the nullable `watch_url` field in each
`/live.json` item. Unsafe stored URLs are omitted. Each item also includes
`channel_id`, `status_source` (`provider` or `manual`), and nullable
`manual_state_expires_at` as an RFC 3339 timestamp. Expired manual broadcasts are
excluded when rendering a new page or snapshot, even before the worker updates
its cached session.

Responses whose queried data includes YouTube or link-only channels use
`Cache-Control: public, max-age=0, must-revalidate` so caches cannot extend a
manual LIVE state through stale reuse. Other home/directory/snapshot responses
use `public, max-age=10, stale-while-revalidate=60`; other creator pages use
`public, max-age=30, stale-while-revalidate=120`. Snapshot ETags are computed from
the rendered body, and matching requests support 304 responses. Pages are
server-rendered; the refresh link requests the latest state rather than polling
automatically in the browser.

## Verification

Fast checks:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
docker compose -f deploy/compose.yml config --quiet
./tests/check-budgets.sh
```

Fresh-database and SQLx checks:

```sh
cargo install sqlx-cli --version 0.9.0 --no-default-features --features rustls,postgres --locked
sqlx database create
cargo sqlx migrate run --source migrations
RILL3_TEST_DATABASE_URL="$DATABASE_URL" cargo test -p rill3-db --test postgres --locked -- --test-threads=1
cargo sqlx prepare --workspace --check -- --all-targets
```

Use a disposable database for these integration tests. Run them serially because
provider-wide stale marking and directory assertions share the test database.

The integration tests use deterministic mock/local HTTP behavior and do not require real provider credentials. Real Twitch/CHZZK delivery and policy behavior must still be smoke-tested in staging with registered applications.

The documented CHZZK API exposes a viewer-count-ordered global live directory rather than a registered-channel live-status filter. RILL3 scans that directory once per provider cycle for at most 45 seconds/100 pages, applies channels it positively sees, and never calls a missing channel offline unless the scan reaches its natural end. Channels outside a partial scan age into stale state; creator pages remain link-only. Broadcaster-scoped session events can replace this best-effort path after authenticated creator connections exist in M2.

## Load check

With the Compose service running and `oha` installed:

```sh
oha -z 30s -c 64 -q 500 --no-tui --latency-correction http://127.0.0.1:8080/live.json
```

Record host CPU allocation, cache warmup, RPS, p95, error rate, and application RSS together; the product target is 500 RPS with p95 below 250 ms on 2 vCPU for cacheable endpoints. This target is a completion budget, not a production guarantee. `tests/check-budgets.sh` separately verifies static CSS/JS sizes, webfont absence, and that the home template contains no iframe.

## Repository map

```text
apps/rill3/          process wiring, SSR/API routes, worker and CLI
crates/domain/       provider-neutral values and ordering rules
crates/db/           PostgreSQL repository and migrations
crates/providers/    official provider adapters and webhook verification
crates/telemetry/    structured tracing setup
crates/{auth,payments,alerts,moderation}/
                     intentionally small later-milestone boundaries
templates/           Askama HTML
static/              hand-written CSS and intentionally empty base JS
migrations/          forward-only PostgreSQL schema
deploy/              container and Caddy configuration
docs/                architecture, provider/security policy, and ADRs
```

Read [PLAN.md](PLAN.md) for the exact current scope and [docs/SECURITY.md](docs/SECURITY.md) before changing an input or embed boundary. The latest discovery review and remaining priorities are in [docs/IMPROVEMENTS.md](docs/IMPROVEMENTS.md).

## License

No distribution license has been selected for this bootstrap. Choose and add one before public distribution. Provider names and marks remain the property of their respective owners.
