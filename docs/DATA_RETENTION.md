# Data retention

M0/M1 stores only public creator/channel data, normalized live observations, provider health metadata, and provider-event hashes.

- Provider raw webhook and API bodies: never persisted.
- `provider_events`: retain for 30 days by default, then delete in bounded batches once the public-beta maintenance scheduler exists.
- Live sessions: retained as operational history; titles/categories/thumbnails may be removed on creator deletion or provider policy request.
- Logs: production target 14 days; redact secrets and do not log webhook bodies.
- OAuth, wallet, private tip content, and TTS data do not exist in this milestone and require a separate retention review before implementation.

Backups inherit the same classification. Public beta must document backup expiry and prove restore/deletion procedures before accepting credentials or payments.

