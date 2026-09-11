# ADR 0002: PostgreSQL before Redis

- Status: accepted
- Date: 2026-09-02

## Decision

Use PostgreSQL for durable state, idempotency, advisory scheduling locks, and later `SKIP LOCKED` queues. Put cacheable public responses behind HTTP/CDN caching. Do not deploy Redis, NATS, or Kafka until measured contention or latency justifies it.

## Consequences

There is one consistency model and fewer operational dependencies. Repository interfaces keep a future read cache possible, while correctness never depends on an ephemeral cache.

