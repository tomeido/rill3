# Discovery improvements — 2026-09-07

Priorities 1–5 from the direct-link release review were validated and deployed as
`release-20260907-094909`. The 125 tests, operator CLI checks, browser checks,
and release evidence are recorded in [SELFHOST.md](SELFHOST.md).

The production database still has zero creators and zero external channels.
Actual channel details have not been supplied for registration. CLI examples
are placeholders, and development fixtures are not production registrations.

## Completed changes

| Priority | Original problem | Implemented behavior | Validation focus |
| --- | --- | --- | --- |
| 1 | Operators could only insert development fixtures, and offline channels were absent from discovery. | Added `channels register` and paginated `channels list`, provider/input validation, and atomic creator/channel registration. Existing IDs, creator profiles, and channel ownership are preserved. `/channels` and the home preview expose enabled channels even when offline or unknown; empty messages distinguish no channels from no current broadcasts. | Registration and re-registration, rollback on cross-owner/disabled conflicts, credential and unsafe-URL errors, list cursors, and public offline discovery. Real production registration awaits actual channel information. |
| 2 | A stored YouTube URL could imply LIVE despite Offline/Unknown state or an absent expiry. | YouTube and link-only state now requires explicit, unexpired manual status; online also requires a valid broadcast URL. The CLI defaults to Unknown, with a 120-minute TTL for explicit online/offline state and a 1440-minute maximum. Pages and JSON re-evaluate expiry before rendering. Manual information is labeled, API items include `status_source` and `manual_state_expires_at`, and affected responses require cache revalidation. | Status/URL/expiry combinations, expired cached sessions before worker reconciliation, manual-source labels, JSON fields and ETags, and cache headers that prevent stale manual LIVE reuse. |
| 3 | Cards for several platforms could open the same preferred channel on the creator page. | Cards include `?channel=<uuid>`; detail lookup requires that exact enabled channel to belong to the creator. Connected-channel navigation preserves the selected channel and paginates additional connections. Omitting the selector retains the preferred-channel behavior. | Two simultaneous provider channels, owned offline selections, cross-owner/disabled/missing UUID rejection, malformed UUID handling, and selected navigation state. |
| 4 | Incomplete provider credentials could appear connected in the server and stop every worker provider from starting. | Server and worker share credential-set checks. Missing, incomplete, and invalid sets disable only the affected provider, using redacted diagnostics. Other providers continue reconciliation. | Credential combinations and invalid secret formats, matching server labels, secret redaction, and successful unrelated-provider reconciliation. |
| 5 | The introduction and platform shortcuts delayed live cards on narrow screens, and timestamps were raw UTC strings. | Populated home pages use a compact introduction and show broadcasts before platform shortcuts. Cards and detail pages use relative times with machine-readable `<time>` values. The list has a refresh link and labels its count as the number displayed. | Populated and empty layouts at mobile/desktop widths, first-card placement, readable times, refresh links, keyboard focus, and player fallbacks. |

Implementation references:

- [Operator guide](CHANNELS.md) and [channel CLI](../apps/rill3/src/command_channels.rs).
- [Registration and directory queries](../crates/db/src/discovery.rs).
- [Shared manual-state rules](../crates/domain/src/manual.rs),
  [YouTube provider](../crates/providers/src/youtube.rs), and
  [link-only provider](../crates/providers/src/link_only.rs).
- [Routes, channel selection, JSON, and cache policy](../apps/rill3/src/http.rs).
- [Shared credential checks](../apps/rill3/src/provider_config.rs) and
  [worker scheduling](../apps/rill3/src/command_worker.rs).
- [View models](../apps/rill3/src/views.rs), [home template](../templates/home.html),
  and [stylesheet](../static/css/app.css).

Registration refreshes channel fields rather than editing creator profiles.
There is no separate status/renew command: operators repeat the full registration
arguments to renew, go offline, or reset to unknown. Changes to manual URL,
status, or expiry invalidate the previous cached session atomically; the worker
then reconstructs it from the new registration. The CLI does not expose profile
editing, ownership transfer, enable/disable, or deletion.

## Follow-up work

| Area | Remaining work | Acceptance check |
| --- | --- | --- |
| 6 — Live filtering and pagination | The live home/snapshot still has a 60-row bound and no provider filter or continuation cursor. Its count now explicitly means “displayed,” while the registered-channel directory already has pagination. Add provider filtering before the result limit, live pagination, and sorting that uses the same provider-age freshness rules as the UI. | A platform or broadcast outside the first live page remains discoverable, and fresh observations consistently precede stale ones. |
| Operator workflow | Add focused manual status/renewal commands and deliberate profile/lifecycle management when required. Creator-facing authenticated registration remains a later milestone. | Operators can change a single manual state without re-entering unaffected fields; ownership and disabled-state protections remain intact. |
| YouTube live verification | Add creator-authorized OAuth/liveBroadcasts verification. Current manual registration validates syntax and records operator-supplied state; it does not prove broadcast activity or channel/video ownership. | Provider-confirmed live changes replace bounded manual observations for authorized channels. |
| Real provider validation | Exercise Twitch and CHZZK registration, reconciliation, and callbacks in staging with actual configured applications and authorized channel data. | Document observed provider behavior and failure recovery without using fixtures as production broadcasts. |
| Off-host recovery | Configure automated off-host database backups, retention, failure reporting, and scheduled restore exercises. | A retained backup outside this host restores successfully into an isolated database under a documented schedule. |

The earlier direct-link release added a content-derived stylesheet URL and
performed a restore of a local pre-release database dump. That local recovery
copy does not protect against loss of the host and does not complete the
off-host recovery work above. This release also restored its own production
backup into an isolated database before deployment.
