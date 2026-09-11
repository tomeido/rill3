CREATE FUNCTION rill3_set_updated_at()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.updated_at = clock_timestamp();
    RETURN NEW;
END;
$$;

CREATE TABLE creators (
    id uuid PRIMARY KEY,
    slug text NOT NULL,
    display_name text NOT NULL,
    bio text,
    locale text NOT NULL DEFAULT 'ko-KR',
    status text NOT NULL DEFAULT 'active',
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT creators_slug_unique UNIQUE (slug),
    CONSTRAINT creators_slug_format CHECK (
        char_length(slug) BETWEEN 1 AND 63
        AND slug ~ '^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$'
    ),
    CONSTRAINT creators_display_name_length CHECK (
        char_length(btrim(display_name)) BETWEEN 1 AND 120
    ),
    CONSTRAINT creators_bio_length CHECK (
        bio IS NULL OR char_length(bio) <= 2000
    ),
    CONSTRAINT creators_locale_format CHECK (
        char_length(locale) BETWEEN 2 AND 35
        AND locale ~ '^[A-Za-z]{2,3}(?:-[A-Za-z0-9]{2,8})*$'
    ),
    CONSTRAINT creators_status_valid CHECK (status IN ('active', 'disabled')),
    CONSTRAINT creators_timestamps_ordered CHECK (updated_at >= created_at)
);

CREATE TRIGGER creators_set_updated_at
BEFORE UPDATE ON creators
FOR EACH ROW
EXECUTE FUNCTION rill3_set_updated_at();

CREATE TABLE external_channels (
    id uuid PRIMARY KEY,
    creator_id uuid NOT NULL REFERENCES creators(id) ON DELETE CASCADE,
    provider text NOT NULL,
    provider_channel_id text NOT NULL,
    handle text,
    canonical_url text NOT NULL,
    current_live_url text,
    manual_live_status text,
    manual_state_expires_at timestamptz,
    embed_capability text NOT NULL DEFAULT 'link_only',
    verification_state text NOT NULL DEFAULT 'pending',
    enabled boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT external_channels_provider_channel_unique
        UNIQUE (provider, provider_channel_id),
    CONSTRAINT external_channels_provider_format CHECK (
        char_length(provider) BETWEEN 1 AND 32
        AND provider ~ '^[a-z][a-z0-9_]*$'
    ),
    CONSTRAINT external_channels_provider_channel_id_length CHECK (
        char_length(btrim(provider_channel_id)) BETWEEN 1 AND 255
    ),
    CONSTRAINT external_channels_handle_length CHECK (
        handle IS NULL OR char_length(handle) BETWEEN 1 AND 255
    ),
    CONSTRAINT external_channels_canonical_url_https CHECK (
        char_length(canonical_url) <= 2048
        AND canonical_url ~ '^https://'
    ),
    CONSTRAINT external_channels_current_live_url_https CHECK (
        current_live_url IS NULL
        OR (char_length(current_live_url) <= 2048 AND current_live_url ~ '^https://')
    ),
    CONSTRAINT external_channels_embed_capability_valid CHECK (
        embed_capability IN ('official_embed', 'link_only', 'manual')
    ),
    CONSTRAINT external_channels_verification_state_valid CHECK (
        verification_state IN ('pending', 'verified', 'disabled')
    ),
    CONSTRAINT external_channels_manual_live_status_valid CHECK (
        manual_live_status IS NULL
        OR manual_live_status IN ('online', 'offline', 'unknown')
    ),
    CONSTRAINT external_channels_manual_state_consistent CHECK (
        (manual_live_status IS NULL AND manual_state_expires_at IS NULL)
        OR (manual_live_status IS NOT NULL AND manual_state_expires_at IS NOT NULL)
    ),
    CONSTRAINT external_channels_timestamps_ordered CHECK (updated_at >= created_at)
);

CREATE INDEX external_channels_creator_id_idx
    ON external_channels (creator_id, id);
CREATE INDEX external_channels_provider_enabled_idx
    ON external_channels (provider, id)
    WHERE enabled;
CREATE INDEX external_channels_manual_expiry_idx
    ON external_channels (manual_state_expires_at, id)
    WHERE manual_state_expires_at IS NOT NULL;

CREATE TRIGGER external_channels_set_updated_at
BEFORE UPDATE ON external_channels
FOR EACH ROW
EXECUTE FUNCTION rill3_set_updated_at();

CREATE TABLE live_sessions (
    id uuid PRIMARY KEY,
    channel_id uuid NOT NULL REFERENCES external_channels(id) ON DELETE CASCADE,
    provider_session_id text,
    status text NOT NULL,
    title text,
    category text,
    thumbnail_url text,
    started_at timestamptz,
    ended_at timestamptz,
    observed_at timestamptz NOT NULL,
    last_success_at timestamptz,
    stale_since timestamptz,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT live_sessions_channel_unique
        UNIQUE (channel_id),
    CONSTRAINT live_sessions_provider_session_id_length CHECK (
        provider_session_id IS NULL
        OR char_length(btrim(provider_session_id)) BETWEEN 1 AND 255
    ),
    CONSTRAINT live_sessions_status_valid CHECK (
        status IN ('online', 'offline', 'unknown')
    ),
    CONSTRAINT live_sessions_title_length CHECK (
        title IS NULL OR char_length(title) <= 500
    ),
    CONSTRAINT live_sessions_category_length CHECK (
        category IS NULL OR char_length(category) <= 255
    ),
    CONSTRAINT live_sessions_thumbnail_url_https CHECK (
        thumbnail_url IS NULL
        OR (
            char_length(thumbnail_url) <= 2048
            AND thumbnail_url ~ '^https://'
        )
    ),
    CONSTRAINT live_sessions_end_after_start CHECK (
        ended_at IS NULL OR started_at IS NULL OR ended_at >= started_at
    ),
    CONSTRAINT live_sessions_last_success_not_future CHECK (
        last_success_at IS NULL OR last_success_at <= observed_at
    ),
    CONSTRAINT live_sessions_stale_after_success CHECK (
        stale_since IS NULL
        OR last_success_at IS NULL
        OR stale_since >= last_success_at
    ),
    CONSTRAINT live_sessions_timestamps_ordered CHECK (updated_at >= created_at)
);

CREATE INDEX live_sessions_online_observed_idx
    ON live_sessions (observed_at DESC, id DESC)
    WHERE status = 'online';

CREATE TRIGGER live_sessions_set_updated_at
BEFORE UPDATE ON live_sessions
FOR EACH ROW
EXECUTE FUNCTION rill3_set_updated_at();

CREATE TABLE provider_events (
    id uuid PRIMARY KEY,
    provider text NOT NULL,
    external_event_id text NOT NULL,
    channel_id uuid REFERENCES external_channels(id) ON DELETE SET NULL,
    event_type text NOT NULL,
    occurred_at timestamptz NOT NULL,
    received_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    payload_hash bytea NOT NULL,
    status text NOT NULL DEFAULT 'received',
    handled_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT provider_events_provider_external_event_unique
        UNIQUE (provider, external_event_id),
    CONSTRAINT provider_events_provider_format CHECK (
        char_length(provider) BETWEEN 1 AND 32
        AND provider ~ '^[a-z][a-z0-9_]*$'
    ),
    CONSTRAINT provider_events_external_event_id_length CHECK (
        char_length(btrim(external_event_id)) BETWEEN 1 AND 255
    ),
    CONSTRAINT provider_events_event_type_length CHECK (
        char_length(btrim(event_type)) BETWEEN 1 AND 100
    ),
    CONSTRAINT provider_events_payload_hash_sha256 CHECK (
        octet_length(payload_hash) = 32
    ),
    CONSTRAINT provider_events_status_valid CHECK (
        status IN (
            'received',
            'applied',
            'duplicate',
            'ignored_out_of_order',
            'failed'
        )
    ),
    CONSTRAINT provider_events_handled_consistent CHECK (
        (status = 'received' AND handled_at IS NULL)
        OR (status <> 'received' AND handled_at IS NOT NULL)
    ),
    CONSTRAINT provider_events_handled_after_received CHECK (
        handled_at IS NULL OR handled_at >= received_at
    ),
    CONSTRAINT provider_events_timestamps_ordered CHECK (updated_at >= created_at)
);

CREATE INDEX provider_events_channel_occurred_idx
    ON provider_events (channel_id, occurred_at DESC, id DESC)
    WHERE channel_id IS NOT NULL;
CREATE INDEX provider_events_unhandled_idx
    ON provider_events (received_at, id)
    WHERE status = 'received';

CREATE TRIGGER provider_events_set_updated_at
BEFORE UPDATE ON provider_events
FOR EACH ROW
EXECUTE FUNCTION rill3_set_updated_at();
