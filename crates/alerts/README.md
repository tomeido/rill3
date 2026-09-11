# rill3-alerts

OBS alert delivery and playback-state boundary for RILL3.

M0/M1 does not create audio, store alert media, expose overlay keys, or open SSE connections. M4 will add the idempotent alert state machine, PostgreSQL-backed work queue, TTS preparation, rotating overlay credentials, SSE delivery, acknowledgement, skip, and mute controls. Alert creation must remain downstream of confirmed payment.

