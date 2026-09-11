# Shared-host deployment

Public URL: <https://domeido.asuscomm.com/rill3>.

This M0/M1 preview uses the existing host Caddy for HTTPS. It runs a separate
PostgreSQL volume, server, and worker in the `rill3-selfhost` Compose project.
Only the server joins Caddy's `domeido-edge` network. No additional host ports
are published. Restart policies bring the services back after a host restart.

## Configuration

Store deployment settings in the ignored, owner-readable `.env.production`:

```dotenv
RILL3_ENVIRONMENT=production
RILL3_PUBLIC_ORIGIN=https://domeido.asuscomm.com
RILL3_BASE_PATH=/rill3
RILL3_TWITCH_PARENT=domeido.asuscomm.com
RILL3_DB_MAX_CONNECTIONS=10
RUST_LOG=info,tower_http=info
RILL3_IMAGE_TAG=release-YYYYMMDD-HHMMSS
POSTGRES_DB=rill3
POSTGRES_USER=rill3
POSTGRES_PASSWORD=REPLACE_WITH_A_RANDOM_HEX_PASSWORD
```

Use a generated password (for example `openssl rand -hex 32`), not the example
placeholder. Set file permissions to `600`. Provider credentials may be added
to the same file using the variable names in `.env.example`. Without credentials
or registered channels, the site correctly shows an empty live list. Do not
insert development demo streams into the public database.

## Build and start

```sh
docker compose --env-file .env.production -f deploy/compose.selfhost.yml config --quiet
docker compose --env-file .env.production -f deploy/compose.selfhost.yml build server
docker compose --env-file .env.production -f deploy/compose.selfhost.yml up -d --wait
```

Server and worker apply embedded migrations on startup. The database persists
in `rill3-selfhost_postgres-data`. Never use `down --volumes` for this project.
Validate formatting, clippy, workspace tests, PostgreSQL integration tests,
SQLx metadata, and browser budgets before replacing an image.

## Connect the existing Caddy

Merge the snippet in `deploy/Caddyfile.selfhost` into the existing
`domeido.asuscomm.com` block in `/home/tomeido/docker/caddy/Caddyfile`.
Back up that file first, preserve all other routes, and keep the `/rill3`
prefix when forwarding requests. Then validate and reload without restarting:

```sh
docker exec caddy caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
docker exec caddy caddy reload --config /etc/caddy/Caddyfile --adapter caddyfile
```

The existing shared proxy has no access log enabled. If enabling access logs,
exclude `/rill3/webhooks/youtube` because WebSub verification tokens appear in
its query string, as described in the standalone `deploy/Caddyfile`.

## Verify and operate

```sh
curl -fsS https://domeido.asuscomm.com/rill3
curl -fsS https://domeido.asuscomm.com/rill3/health/live
curl -fsS https://domeido.asuscomm.com/rill3/health/ready
curl -fsS https://domeido.asuscomm.com/rill3/live.json
docker compose --env-file .env.production -f deploy/compose.selfhost.yml ps
docker compose --env-file .env.production -f deploy/compose.selfhost.yml logs --tail 50 server worker
```

Both root health routes remain available inside the container for Docker and
Caddy. Public CSS, links, JSON, OpenAPI and callbacks use the `/rill3` prefix.
Callback URLs are `/rill3/webhooks/twitch` and `/rill3/webhooks/youtube` on the
public origin. `/rill3/` redirects to the canonical `/rill3` URL.

For an application rollback, select the previous `RILL3_IMAGE_TAG` and run
`up -d --no-build --wait`; first confirm schema compatibility. To remove this
initial route, restore the saved Caddy configuration and validate/reload it,
then stop only the `rill3-selfhost` project without deleting volumes.

The initial deployment has no provider credentials or registered user data.
The backup/restore and public-beta requirements in [OPERATIONS.md](OPERATIONS.md)
remain outstanding; a local Docker volume is not an off-host backup.

## Verified deployment — 2026-09-06

- Image: `rill3:release-20260906-140148`.
- Image ID: `sha256:bcb4e250b02fece5e0360a8cc836e10cd2f36e280d642727fd2529bad2ffaf8c`.
- Caddy backup: `/home/tomeido/docker/caddy/Caddyfile.bak.rill3-20260906T141433Z`.
- Validation: 68 unit tests, 5 tests against a fresh PostgreSQL database,
  SQLx metadata check, workspace clippy, formatting, and browser budgets passed.
- Live HTTPS checks passed for the home page, CSS, JSON, OpenAPI, liveness,
  readiness, missing creator pages, ETag responses, and canonical redirects.
- Existing `/`, `/corporate/`, `/mantis/`, and `/splendor/` services still respond.
- Migration `20260902000000` applied; the public database contains no demo creators.

## Verified deployment — 2026-09-07

- Image: `rill3:release-20260907-061611`.
- Image ID: `sha256:5b394dda247218e06bc0e2e8154a18f4d47865075a0596f18743658c3c156054`.
- Adds permanent platform shortcuts, direct broadcast links on cards and creator
  pages, validated `watch_url` values in the public snapshot, and clearer empty
  and offline states. The stylesheet URL now includes its content digest to
  refresh browser caches when CSS changes.
- Validation: 80 unit tests and 5 fresh PostgreSQL integration tests, workspace
  clippy, formatting, SQLx metadata/offline compilation, Compose validation,
  browser budgets, and a release-image server/worker smoke check passed.
- A pre-release database dump was restored into an isolated database and its
  migration/row counts verified. The owner-readable release archive, prior
  environment, and dump are in
  `/home/tomeido/.local/state/rill3/releases/release-20260907-061611/`.
  This remains a local recovery copy, not an off-host backup.
- Public HTTPS checks passed for home, versioned CSS, liveness/readiness, JSON
  ETag, OpenAPI `watch_url`, missing creator pages, and canonical redirects.
- Actual production-browser checks passed at 1440px and 390px for the four
  shortcuts, CSS digest, layout, and keyboard focus. Screenshots and the browser
  report are in `/tmp/rill3-release-artifacts/`.
- Server and worker run the same image with zero restarts after replacement.
  Existing `/`, `/corporate/`, `/mantis/`, and `/splendor/` still return HTTP 200.
- Migration `20260902000000` is unchanged. Production still has zero creators
  and channels; no demo data was inserted.
- Application rollback target: `release-20260906-140148`. Set that
  `RILL3_IMAGE_TAG` and run `up -d --no-deps --no-build --wait server worker` with
  the same environment and Compose file.

Remaining discovery improvements are recorded in [IMPROVEMENTS.md](IMPROVEMENTS.md).

## Verified discovery deployment — 2026-09-07 09:56 UTC

- Image: `rill3:release-20260907-094909`.
- Image ID: `sha256:219e19fe810ab0142ad71933c7f8eadd0f6377615a94faa3d9ac114154a16529`.
- Adds operator channel registration/listing, offline discovery at `/channels`,
  exact channel selection and switching, bounded manual LIVE state with immediate
  expiry checks, shared provider credential checks, and a compact mobile home.
  See [CHANNELS.md](CHANNELS.md) for registration and renewal commands.
- Validation passed: 110 unit tests, 15 fresh PostgreSQL integration tests,
  workspace clippy, formatting, refreshed SQLx metadata, offline compilation,
  Compose checks, and source/rendered browser budgets. PostgreSQL integration
  tests run serially because provider-wide assertions share their database.
- Actual CLI checks covered default Unknown registration, idempotent identity,
  ownership-conflict rollback, unsafe URL errors without secret echoing, cursor
  pages, and Online/Offline transitions. The exact release image passed server
  readiness and one-shot worker checks with incomplete Twitch credentials.
- Browser checks covered empty and populated homes at 320/390/1440px, all four
  simultaneous platforms for one creator, exact channel selection, player
  fallbacks, keyboard focus, and 25 registered channels across two pages.
  At 390px the first broadcast begins at about 304px. A manual YouTube expiry
  removed its iframe, home card, and API entry on reload without a worker update.
- Production browser checks passed for home and `/channels` at 390/1440px,
  navigation, four platform shortcuts, empty-state copy, keyboard focus, and
  stylesheet digest `4d19032cf53c9da9`. Common CSS is 12,816 bytes; no JavaScript
  or home-page iframe was added.
- Public checks passed for health, JSON/ETag, OpenAPI, invalid cursors/selectors,
  missing creator pages, canonical redirects, and versioned CSS. Existing `/`,
  `/corporate/`, `/mantis/`, and `/splendor/` continue to return HTTP 200.
- Server and worker were replaced together and have zero restarts. Production
  still has zero creators/channels; test registrations used isolated databases.
  Migration `20260902000000` is unchanged.
- Local backup restore, prior environment, source archive, and verification
  artifacts are stored in
  `/home/tomeido/.local/state/rill3/releases/release-20260907-094909/`.
  This is a local recovery copy; off-host backup remains follow-up work.
- Rollback target: `release-20260907-061611`. Set `RILL3_IMAGE_TAG` to that value
  and run `up -d --no-deps --no-build --wait server worker` using the existing
  production environment and Compose file. The database and proxy stay in place.
