# Security model

This document covers discovery and the opt-in broadcaster ownership/vault extension. See [Web3 setup and trust boundaries](WEB3.md) for the complete donation lifecycle. OBS alerts remain outside the current implementation.

## Trust boundaries

Provider HTTP responses and webhooks, route parameters, headers, database contents, and operator-supplied environment variables are untrusted. Only typed, validated identifiers cross from those inputs into rendered URLs. Secrets are read from environment variables and are never included in debug output or structured fields.

## Web and embed controls

- The application emits Content-Security-Policy with `default-src 'self'`, a small image allowlist, and `frame-src` restricted to Twitch and YouTube official origins.
- `frame-ancestors 'none'`, HSTS, `X-Content-Type-Options: nosniff`, and a strict referrer policy are emitted on every response.
- Twitch `parent` and YouTube `origin` are parsed from trusted configuration. Channel/video IDs are validated before interpolation, HTML is escaped by Askama, and arbitrary iframe markup is never accepted.
- Link-only URLs must use HTTPS, must not contain credentials, and reject localhost/private/link-local/unspecified literal IP hosts. RILL3 stores but never server-fetches these URLs or follows their redirects.
- Webhook request bodies and request duration are limited before expensive processing.

## Webhooks and replay

Twitch EventSub uses the exact raw body and the `message-id + timestamp + body` HMAC-SHA256 construction. Signatures are compared in constant time. Timestamps outside the configured tolerance are rejected, and the database unique key prevents delivery reuse. A valid-but-duplicated notification returns success without applying state twice. Verification challenges are echoed only after signature validation.

YouTube WebSub callbacks validate the configured callback token and accepted topic before echoing a challenge. Notification bodies are bounded and stored only as a short-lived hash/event marker; they trigger reconciliation rather than being trusted as normalized live state.

## Persistence and retention

Only normalized provider state and a SHA-256 payload hash are durable. Raw provider payloads are not stored. Event markers have a documented retention job target of 30 days; implementation of scheduled deletion is an operations task before public beta. Database enums are represented as checked text values so new provider/state values can be rolled out without PostgreSQL enum surgery.

## Broadcaster ownership and donations

Public registration never proves ownership or transfers an existing channel. Display names are untrusted; the support page exposes the actual provider channel ID. Official OAuth verifies an exact provider/channel identity using browser-bound, single-use state. Tokens are transient. Owner sessions live for 15 minutes and are scoped to one channel; legacy discovery verification flags never authorize payouts.

Mutations require an exact trusted Origin and bounded JSON bodies. Auth/payment responses disable caching and use no-referrer. Opaque session/state tokens are stored only as SHA-256 hashes; cookies are HttpOnly, SameSite=Lax, path-scoped, and Secure on HTTPS. A wallet challenge is bound to the authenticated session, origin, channel, chain, factory, wallet and expiry, then consumed atomically. The first valid claim reserves its beneficiary permanently before the attestation can be returned.

An immutable, code-hash-pinned EVM factory creates a dedicated native-ETH vault per provider/channel. The server attestor is trusted to assign the first owner; compromising it threatens unclaimed vaults. There is no operator withdrawal, upgrade or owner reset after a claim. A wallet signs and submits every deposit/claim/withdrawal itself. No donor or payout private key is held by the service. Unclaimed funds have no automatic refund, and lost owner keys cannot be recovered. See [contracts](../contracts/README.md).

## Operational limits

- Per-process request limits protect public registration, OAuth and RPC work. Caddy/CDN should also enforce IP-based and multi-instance limits.
- Smart-account signatures, ERC-20 recovery, owner rotation and transaction indexing are not implemented.
- External provider integration can be contract-tested locally, but production credentials and provider delivery must be verified in staging.

