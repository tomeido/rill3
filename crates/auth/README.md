# rill3-auth

Official OAuth authorization-code exchanges for channel ownership. Registration and legacy discovery `verification_state` never establish ownership. The app consumes a single-use database OAuth state tied to a separate `HttpOnly` browser cookie before calling this crate; only exact provider channel identity matches create a short-lived owner session.

Twitch tokens are validated against the configured `client_id`, a nonempty user `user_id`, and expiry. `YouTube` uses authenticated `channels.list` with `mine=true`. CHZZK uses the authenticated `/open/v1/users/me` endpoint, requiring the application's **유저 정보 조회** permission. Link-only providers cannot prove ownership.

Configure provider client IDs/secrets and exact registered callback URLs. Redirects must use HTTPS except loopback development. Endpoints are fixed to official hosts, HTTP redirects are disabled, network requests expire after ten seconds, and response bodies are bounded. Provider tokens and callback codes are never stored, returned, or included in errors. Owner sessions expire within one hour; no refresh-token storage or background OAuth access is implemented.

References:

- [Twitch authorization code flow](https://dev.twitch.tv/docs/authentication/getting-tokens-oauth/#authorization-code-grant-flow)
- [Twitch token validation](https://dev.twitch.tv/docs/authentication/validate-tokens/)
- [Google web server OAuth](https://developers.google.com/identity/protocols/oauth2/web-server)
- [YouTube authenticated channel identity](https://developers.google.com/youtube/v3/docs/channels/list)
- [CHZZK authorization](https://chzzk.gitbook.io/chzzk/chzzk-api/authorization)
- [CHZZK authenticated user identity](https://chzzk.gitbook.io/chzzk/chzzk-api/user)
