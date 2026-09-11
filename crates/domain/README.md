# rill3-domain

Provider-neutral types and state-transition rules for the RILL3 live directory.
This crate deliberately has no HTTP, Axum, SQLx, or provider SDK dependency.

The central invariant is that a live observation may replace the stored state
only when its `observed_at` timestamp is strictly newer. Equal observations are
duplicates; conflicting or older observations are ignored. Provider adapters
also use the typed embed descriptors so only validated Twitch channels and
YouTube video IDs can become iframe URLs.

Run its tests with:

```sh
cargo test -p rill3-domain
cargo clippy -p rill3-domain --all-targets -- -D warnings
```
