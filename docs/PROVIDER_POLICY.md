# Provider policy

RILL3 uses official, documented provider interfaces only. It does not scrape player pages, extract HLS manifests, use private endpoints, download video, proxy media, or accept arbitrary iframe HTML.

| Provider | Discovery/state | Viewer mode | M0/M1 credential behavior |
| --- | --- | --- | --- |
| Twitch | EventSub plus batched Helix reconciliation | Official embed | Disabled without client ID/secret/EventSub secret |
| YouTube | Manual channel/video registration; WebSub is a change signal | Official iframe for a validated video ID | Manual mode works without OAuth; OAuth/liveBroadcasts is deferred |
| CHZZK | Official global live-directory API with bounded polling/backoff | Link only | Disabled without client ID/secret |
| Other | Operator-entered manual state with expiry | Link only | HTTPS URL validation; never fetched by the server |

## Adapter invariants

- `verify_channel` validates provider identifiers and canonicalizes provider-owned URLs without doing arbitrary URL resolution.
- `reconcile_many` is bounded and providers may split requests to documented batch limits.
- A 429 or 5xx response becomes a typed transient error with retry/backoff information. One provider failure does not stop other provider batches.
- Live observations contain provider time or local receipt time and cannot overwrite a newer observation.
- The last successful observation is retained when a provider fails; after its freshness window it is served as stale rather than fabricated offline. Viewer responses also derive staleness from observation age so a dead worker cannot leave an online row fresh forever (11 minutes for Twitch; 3 minutes for the other M1 adapters).
- A provider that lacks credentials reports `disabled`, while the server and other adapters remain available.

## Embed policy

Only `OfficialEmbed` can produce an iframe descriptor. Twitch descriptors carry a validated channel and the configured `parent`. YouTube descriptors carry a parsed video ID and the configured absolute HTTPS origin. CHZZK and generic providers return `LinkOnly`. Public pages may render at most one descriptor.

## Polling defaults

- Twitch reconciliation: approximately five minutes, with documented Helix batches of at most 100 channel identifiers.
- CHZZK: normally 60 seconds plus jitter, increasing after 429/5xx. The official API only exposes a viewer-count-ordered global directory, so one scan is shared across every registered CHZZK channel. A scan is bounded to 45 seconds and 100 pages. A partial scan refreshes only positively observed online channels; it never fabricates offline state for omitted channels.
- Link-only manual state: no network polling; it becomes unknown when its configured observation TTL expires.
- YouTube public search polling: prohibited. M1 uses manual URLs and a WebSub signal; owner OAuth/liveBroadcasts remains an explicit TODO.
