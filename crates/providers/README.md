# rill3-providers

Official external-stream adapters for RILL3 M0/M1.

- Twitch uses Helix, caches an app access token, normalizes streams, and verifies
  EventSub HMAC/timestamps against the exact raw request body.
- YouTube supports validated manual live URLs, WebSub challenge validation, and
  constant-time SHA-256/SHA-1 notification HMAC validation;
  OAuth/liveBroadcasts intentionally remains a later interface boundary.
- CHZZK uses only the documented Open API and remains link-only.
- Link-only registrations validate HTTPS destinations but are never fetched.
- MockProvider exercises deterministic state transitions without a network.

HTTP 429 and 5xx responses are returned as typed transient errors containing
server retry hints. `BackoffPolicy` turns those hints into bounded exponential
delays for the worker. Adapters use a no-redirect HTTP client.

Run its tests with:

```sh
cargo test -p rill3-providers
cargo clippy -p rill3-providers --all-targets -- -D warnings
```
