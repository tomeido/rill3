-- Registrations are public; only short-lived, provider-proven sessions authorize claims.
CREATE TABLE creator_oauth_states (
    state_hash bytea PRIMARY KEY CHECK (octet_length(state_hash) = 32),
    browser_hash bytea NOT NULL CHECK (octet_length(browser_hash) = 32),
    channel_id uuid NOT NULL REFERENCES external_channels(id) ON DELETE CASCADE,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX creator_oauth_states_expiry_idx ON creator_oauth_states (expires_at);

CREATE TABLE creator_owner_sessions (
    session_hash bytea PRIMARY KEY CHECK (octet_length(session_hash) = 32),
    channel_id uuid NOT NULL REFERENCES external_channels(id) ON DELETE CASCADE,
    provider text NOT NULL,
    provider_channel_id text NOT NULL,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (session_hash, channel_id)
);
CREATE INDEX creator_owner_sessions_expiry_idx ON creator_owner_sessions (expires_at);

CREATE TABLE creator_wallet_challenges (
    nonce_hash bytea PRIMARY KEY CHECK (octet_length(nonce_hash) = 32),
    session_hash bytea NOT NULL,
    channel_id uuid NOT NULL,
    wallet_address text NOT NULL CHECK (wallet_address ~ '^0x[0-9a-f]{40}$'),
    message text NOT NULL CHECK (char_length(message) BETWEEN 1 AND 4096),
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    FOREIGN KEY (session_hash, channel_id)
        REFERENCES creator_owner_sessions(session_hash, channel_id) ON DELETE CASCADE
);
CREATE INDEX creator_wallet_challenges_expiry_idx ON creator_wallet_challenges (expires_at);

CREATE TABLE creator_web3_bindings (
    channel_id uuid PRIMARY KEY REFERENCES external_channels(id) ON DELETE CASCADE,
    chain_id bigint NOT NULL CHECK (chain_id > 0),
    factory_address text NOT NULL CHECK (factory_address ~ '^0x[0-9a-f]{40}$'),
    receive_address text NOT NULL CHECK (receive_address ~ '^0x[0-9a-f]{40}$'),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

-- The first signed claim locks the beneficiary before an attestation is issued.
-- This is an authorization reservation, never a claim of on-chain confirmation.
CREATE TABLE creator_wallet_claims (
    channel_id uuid PRIMARY KEY REFERENCES external_channels(id) ON DELETE CASCADE,
    wallet_address text NOT NULL CHECK (wallet_address ~ '^0x[0-9a-f]{40}$'),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE TABLE creator_ownership_audit (
    id uuid PRIMARY KEY,
    channel_id uuid NOT NULL REFERENCES external_channels(id) ON DELETE CASCADE,
    action text NOT NULL CHECK (action IN ('provider_verified', 'wallet_reserved')),
    wallet_address text CHECK (wallet_address IS NULL OR wallet_address ~ '^0x[0-9a-f]{40}$'),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX creator_ownership_audit_channel_idx ON creator_ownership_audit (channel_id, created_at);
