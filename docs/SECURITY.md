# Security model

This document covers M0/M1. Wallet, payment, OAuth token storage, and OBS threat models will be expanded before those features are implemented.

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

## Known M0/M1 limits

- No authentication-bearing browser route exists.
- OAuth is an interface/TODO only; no OAuth tokens are stored.
- Rate limiting/WAF policy is deployed at Caddy/CDN and receives production tuning in M6.
- External provider integration can be contract-tested locally, but production credentials and provider delivery must be verified in staging.

