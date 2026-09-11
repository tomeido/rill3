# ADR 0001: Rust SSR over a SPA

- Status: accepted
- Date: 2026-09-02

## Decision

Render public pages with Axum and Askama. Use progressively enhanced HTML and introduce small JavaScript islands only where an official player, wallet, or OBS media API requires one.

## Consequences

The home route ships no iframe, framework runtime, hydration payload, or webfont. Server templates and CSP remain easy to audit. Rich client state must be deliberately isolated later instead of being globally available.

