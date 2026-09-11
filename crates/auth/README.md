# rill3-auth

Authentication and creator authorization boundary for RILL3.

M0/M1 deliberately exposes no login, session, OAuth-token storage, or public write API. Wallet nonce login, external account verification, encrypted OAuth credentials, authorization checks, and audit records belong to M2. This crate exists now so earlier code cannot accidentally mix those concerns into provider or HTTP modules.

