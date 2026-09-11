# Architecture

RILL3 M0/M1 is one Rust workspace deployed as one binary with three roles:

- `rill3 server`: cached public reads, SSR, health endpoints, and authenticated-provider webhook ingress.
- `rill3 worker`: periodically reconciles registered channels and persists normalized state.
- `rill3 indexer`: a compiling boundary only; chain work starts in M3.

PostgreSQL is the source of truth. Viewer requests only read normalized rows; they never wait on Twitch, YouTube, CHZZK, or an arbitrary creator URL. Provider traffic never scales with viewer requests: Twitch is batched by registered channel, while one bounded official global-directory scan is shared by all registered CHZZK channels. A CDN can cache `/` and `/live.json` in front of the server.

## Crate boundaries

- `domain`: provider-neutral values and state transition rules; no network or database access.
- `db`: migrations and repositories; the only crate that knows PostgreSQL persistence details.
- `providers`: official API adapters, webhook authentication, safe embed descriptors, retries, and the mock implementation.
- `telemetry`: structured logging and request correlation.
- `auth`, `payments`, `alerts`, `moderation`: compiling seams for later milestones, without product behavior.
- `apps/rill3`: configuration, routing, rendering, process lifecycle, and dependency wiring.

Dependencies point inward: the application depends on adapters; adapters depend on the domain. `domain` never depends on Axum, SQLx, or Reqwest.

## Read path

`GET /`, `GET /c/{slug}`, and `GET /live.json` issue bounded PostgreSQL queries. The home page renders thumbnails only. Creator pages build at most one iframe from a typed provider descriptor whose Twitch parent or YouTube origin came from process configuration.

## Write path

Twitch EventSub is verified before parsing or persistence. YouTube WebSub verifies its challenge/lease subscription and treats notifications as a reconciliation signal. The worker groups active channels by provider, takes one PostgreSQL advisory lock, applies a provider timeout, and records only normalized changes. Provider delivery identifiers and event timestamps make retries and out-of-order deliveries safe.

`live_sessions.observed_at` is the ordering authority. A state received at or before the stored observation cannot overwrite it. Provider events are uniquely identified by `(provider, external_event_id)`, and applying an event plus updating live state occurs in one transaction.

## Caching and limits

- `/live.json`: `public, max-age=10, stale-while-revalidate=60` plus a content-derived ETag.
- Home and creator pages: bounded lists, no unbounded scans, no provider calls.
- Database pool: configurable but hard-capped at 20.
- Webhook bodies and total request duration are capped.
- The browser loads no application JavaScript on the home page and no external webfont.
