# ADR 0003: Official embed or link only

- Status: accepted
- Date: 2026-09-02

## Decision

A provider is either `OfficialEmbed`, `LinkOnly`, or `Manual`. Only a documented official player can be embedded. Otherwise RILL3 renders metadata and a normal external link.

## Consequences

RILL3 never proxies media, extracts streams, hides provider controls, or runs a user-provided iframe. This limits in-page playback for some providers but materially reduces policy, SSRF, copyright, and security risk.

