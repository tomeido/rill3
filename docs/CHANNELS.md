# Operator channel registration

The `channels` CLI registers external channels and lists their stored state. It
requires access to the application database through the deployment environment.
There is no public registration or channel-management write API, and these
commands do not establish creator OAuth ownership.

Run the following commands from the repository root, using the deployed image
and the existing PostgreSQL service. The command creates a temporary application
container and removes it afterward; `--no-deps` leaves the running services in
place. Deployment setup is described in [SELFHOST.md](SELFHOST.md).

All values in angle brackets are placeholders. Replace them with the intended
creator's actual identifiers and public URLs before running a registration.
The examples do not designate any real channel as live.

## Configuration and provider inputs

Keep credentials in the owner-readable, ignored `.env.production` file. Compose
supplies `DATABASE_URL`, `RILL3_ENVIRONMENT`, `RILL3_PUBLIC_ORIGIN`, and provider
credentials to the temporary container. The production origin must use HTTPS
and contain no path, query, fragment, or credentials; `/rill3` belongs in
`RILL3_BASE_PATH`.

| Provider | `--provider-channel-id` | Required credentials | Verification performed |
| --- | --- | --- | --- |
| `twitch` | Numeric Twitch user ID or Twitch login | `TWITCH_CLIENT_ID`, `TWITCH_CLIENT_SECRET`, `TWITCH_EVENTSUB_SECRET` | Looks up the user through Twitch Helix and stores the returned numeric ID and login. |
| `chzzk` | Canonical 32-character hexadecimal channel ID | `CHZZK_CLIENT_ID`, `CHZZK_CLIENT_SECRET` | Looks up the channel through the official CHZZK API. |
| `youtube` | Canonical 24-character channel ID beginning with `UC` | None | Validates channel-ID syntax and official video-URL forms; it does not fetch a channel or verify that a video belongs to it or is live. |
| `link_only` | A stable operator-chosen ID, unique across link-only registrations | None | Requires `--url` and validates public HTTPS links without fetching them. |

Twitch's current provider constructor also requires the EventSub secret for
channel lookup. It must be 10–100 ASCII characters. `--handle` is optional; for
Twitch, supplying it makes lookup use that login in preference to
`--provider-channel-id`, although the latter flag remains required. Prefer
passing the numeric ID or login directly as `--provider-channel-id`.

Only the selected provider's credentials are required. Link-only registration
can represent a SOOP channel, but it does not add automatic SOOP live detection.
Choose stable identifiers that distinguish platforms and accounts when using
multiple link-only services.

Every registration requires `--slug`, `--display-name`, `--provider`, and
`--provider-channel-id`. Slugs contain 1–63 lowercase ASCII letters, digits, or
hyphens, with no leading or trailing hyphen. A trimmed display name contains
1–120 characters. Optional `--bio` supports at most 2000 characters, and
`--locale` defaults to `ko-KR`.

## Register a channel

Register a YouTube channel with its status initially unknown:

```sh
docker compose --env-file .env.production -f deploy/compose.selfhost.yml \
  run --rm --no-deps server channels register \
  --slug '<creator-slug>' \
  --display-name '<creator-display-name>' \
  --provider youtube \
  --provider-channel-id '<UC-channel-id>'
```

The canonical channel URL is derived from the ID. Do not supply `--url` for
YouTube, Twitch, or CHZZK. Adding a YouTube `--live-url` without
`--live-status online` still registers the channel as unknown.

For a link-only channel, supply its public channel URL:

```sh
docker compose --env-file .env.production -f deploy/compose.selfhost.yml \
  run --rm --no-deps server channels register \
  --slug '<creator-slug>' \
  --display-name '<creator-display-name>' \
  --provider link_only \
  --provider-channel-id '<stable-link-only-id>' \
  --url '<public-https-channel-url>'
```

This also starts as unknown. Public links must use HTTPS and cannot contain
embedded credentials, literal private IP addresses, or local host names. Supplied links
are stored for viewers to open; the application does not fetch them.

Twitch and CHZZK registration use the same required flags, with
`--provider twitch` or `--provider chzzk` and the matching identifier from the table. Their
live status comes from provider reconciliation; `--live-status` and `--live-url`
are rejected for those providers.

Successful registration prints JSON with the persisted channel UUID, creator
slug, provider identity, enabled state, manual expiry, and status. A new
Twitch/CHZZK registration reports unknown until the worker observes it.

## Set, renew, and end a manual broadcast

YouTube and link-only channels require an explicit manual online state. A
broadcast URL alone never proves that a broadcast is live. Online registration
requires `--live-url`, and its state expires after 120 minutes by default.
`--live-ttl-minutes` accepts 1–1440 minutes; there is no indefinite online option.

Register or refresh a YouTube broadcast that the operator has confirmed is live:

```sh
docker compose --env-file .env.production -f deploy/compose.selfhost.yml \
  run --rm --no-deps server channels register \
  --slug '<creator-slug>' \
  --display-name '<creator-display-name>' \
  --provider youtube \
  --provider-channel-id '<UC-channel-id>' \
  --live-url 'https://www.youtube.com/watch?v=<video-id>' \
  --live-status online \
  --live-ttl-minutes 120
```

The video ID is the actual 11-character YouTube video ID. Official HTTPS
`youtu.be/<video-id>`, YouTube `/live/<video-id>`, and `/embed/<video-id>` forms
are also accepted and normalized to the canonical watch URL.

For a confirmed link-only broadcast, repeat both its channel URL and live URL:

```sh
docker compose --env-file .env.production -f deploy/compose.selfhost.yml \
  run --rm --no-deps server channels register \
  --slug '<creator-slug>' \
  --display-name '<creator-display-name>' \
  --provider link_only \
  --provider-channel-id '<stable-link-only-id>' \
  --url '<public-https-channel-url>' \
  --live-url '<public-https-broadcast-url>' \
  --live-status online \
  --live-ttl-minutes 120
```

To renew a broadcast, confirm it is still live and re-run its full online
registration command. Expiry is recalculated from that registration time. The
channel UUID and creator association remain stable. When the expiry is reached,
the effective manual status becomes unknown and the channel stops qualifying
as live until renewed.

There is currently no separate `set-status` or `renew` subcommand. Re-registration
requires the full identity flags and `--display-name` again; link-only also
requires `--url` again. Repeating only a channel UUID and status is unsupported.
Omitting `--live-status` resets manual state to unknown, and omitting
`--live-url` clears the previously stored broadcast URL. Repeat optional channel
fields such as `--handle` if they should be retained.

Mark a YouTube broadcast offline when it ends:

```sh
docker compose --env-file .env.production -f deploy/compose.selfhost.yml \
  run --rm --no-deps server channels register \
  --slug '<creator-slug>' \
  --display-name '<creator-display-name>' \
  --provider youtube \
  --provider-channel-id '<UC-channel-id>' \
  --live-status offline
```

For link-only, use `--provider link_only`, its stable ID, and repeat `--url` in
the equivalent command. Offline manual state uses the same default 120-minute
TTL and then becomes unknown. Use `--live-status unknown` to reset immediately;
unknown has no expiry and never appears as live.

Manual URL, status, or expiry changes invalidate the prior cached session in
the same transaction. Public pages can show unknown until the running worker's
next successful reconciliation, normally about once per minute for manual providers.
Refresh the page to retrieve the latest state.

## Stable identities and registration conflicts

A slug identifies one creator. Reusing that slug attaches another provider
channel to the same creator and preserves the creator UUID, display name, bio,
locale, and status. Those profile fields are used only when creating a new
creator; re-registration is not a profile editor.

The pair of provider and verified provider-channel ID identifies a channel.
Re-registering that identity under the same creator refreshes channel fields
without inserting a duplicate or changing its UUID. It cannot transfer an
existing channel to another creator, and it cannot reactivate a disabled creator
or channel. A rejected registration rolls back the creator and channel writes
together. There is currently no CLI command for renaming creators, transferring
ownership, enabling, disabling, or deleting records.

## List and inspect registrations

List all registered states, including offline, unknown, and disabled channels:

```sh
docker compose --env-file .env.production -f deploy/compose.selfhost.yml \
  run --rm --no-deps server channels list
```

The default page size is 50. `--limit` accepts 1–99, and the table prints a
`next_cursor` when another page exists. JSON output contains `channels` and
`next_cursor` (`null` on the final page):

```sh
docker compose --env-file .env.production -f deploy/compose.selfhost.yml \
  run --rm --no-deps -T server channels list --json --limit 50
```

Continue using the exact cursor returned by the previous page:

```sh
docker compose --env-file .env.production -f deploy/compose.selfhost.yml \
  run --rm --no-deps -T server channels list --json --limit 50 \
  --after '<next-cursor-uuid>'
```

Rows are ordered by channel UUID. JSON includes creator display names; the
table and JSON both include channel UUID, creator slug, provider identity,
enabled state, effective status, and manual expiry. They omit URLs and
credentials. The list command only reads registrations and does not run
migrations or change broadcast state.

Active registered channels remain discoverable at `/rill3/channels` even when
offline or unknown. Disabled records are visible to this operator CLI but are
excluded from the public directory. Creator pages are at `/rill3/c/<slug>`.
