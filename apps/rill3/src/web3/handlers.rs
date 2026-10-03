use askama::Template;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use chrono::{TimeDelta, Utc};
use rill3_auth::{random_token, token_hash};
use rill3_db::{PublicRegistration, WalletChallenge};
use rill3_domain::ProviderKind;
use rill3_payments::{VaultState, channel_key, normalize_address, verify_wallet_signature};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Web3Service, config::Payments};
use crate::{http::HttpState, views::PagePaths};

const SESSION: &str = "rill3_owner";
const OAUTH_BINDING: &str = "rill3_oauth_binding";
const WALLET_NONCE: &str = "rill3_wallet_nonce";

#[derive(Template)]
#[template(path = "register.html")]
struct RegisterTemplate {
    paths: PagePaths,
}

#[derive(Template)]
#[template(path = "wallet.html")]
struct WalletTemplate {
    paths: PagePaths,
    channel_id: String,
}

pub(crate) fn router() -> Router<HttpState> {
    Router::new()
        .route("/register", get(registration_page))
        .route("/support/{channel}", get(wallet_page))
        .route("/static/wallet.js", get(wallet_script))
        .route("/static/web3.css", get(wallet_styles))
        .route("/api/registrations", post(register))
        .route("/api/channels/{channel}/web3", get(status))
        .route("/api/channels/{channel}/tip", post(tip))
        .route(
            "/api/channels/{channel}/wallet-challenge",
            post(wallet_challenge),
        )
        .route("/api/channels/{channel}/claim", post(claim))
        .route("/api/channels/{channel}/withdraw", post(withdraw))
        .route("/auth/{provider}/start", get(oauth_start))
        .route("/auth/{provider}/callback", get(oauth_callback))
        .route("/api/auth/logout", post(logout))
        .layer(DefaultBodyLimit::max(8192))
        .layer(axum::middleware::from_fn(no_store))
}

async fn no_store(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[derive(Debug)]
struct ApiError(StatusCode, &'static str);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

fn unavailable() -> ApiError {
    ApiError(
        StatusCode::SERVICE_UNAVAILABLE,
        "후원 서비스를 잠시 이용할 수 없습니다. 설정 또는 네트워크 연결을 확인해 주세요.",
    )
}
fn invalid() -> ApiError {
    ApiError(
        StatusCode::BAD_REQUEST,
        "입력한 채널 정보 또는 지갑 요청이 올바르지 않습니다.",
    )
}
fn unauthorized() -> ApiError {
    ApiError(
        StatusCode::UNAUTHORIZED,
        "해당 방송 계정으로 다시 인증해 주세요.",
    )
}
fn conflict() -> ApiError {
    ApiError(
        StatusCode::CONFLICT,
        "이미 연결된 지갑이 다르거나 요청이 만료되었습니다. 상태를 새로고침해 주세요.",
    )
}

fn service(state: &HttpState) -> Result<&Web3Service, ApiError> {
    state.web3.as_deref().ok_or_else(unavailable)
}
fn payments(service: &Web3Service) -> Result<&Payments, ApiError> {
    service.payments.as_ref().ok_or_else(unavailable)
}

fn origin_allowed(headers: &HeaderMap, origin: &str) -> bool {
    headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) == Some(origin)
        && headers
            .get("sec-fetch-site")
            .and_then(|v| v.to_str().ok())
            .is_none_or(|v| matches!(v, "same-origin" | "none"))
}

fn mutation(
    service: &Web3Service,
    headers: &HeaderMap,
    bucket: usize,
    limit: usize,
) -> Result<(), ApiError> {
    if !origin_allowed(headers, &service.origin) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "같은 사이트에서 요청해 주세요.",
        ));
    }
    rate(service, bucket, limit)
}

fn rate(service: &Web3Service, bucket: usize, limit: usize) -> Result<(), ApiError> {
    if service.allow(bucket, limit) {
        Ok(())
    } else {
        Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "요청이 많습니다. 1분 후 다시 시도해 주세요.",
        ))
    }
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let mut matching = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|v| v.trim().split_once('='))
        .filter(|(key, value)| {
            *key == name && value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
        });
    let first = matching.next()?.1.to_owned();
    matching.next().is_none().then_some(first)
}

fn set_cookie(
    response: &mut Response,
    service: &Web3Service,
    name: &str,
    value: &str,
    seconds: u16,
) {
    let path = if service.base_path.is_empty() {
        "/"
    } else {
        &service.base_path
    };
    let secure = if service.secure { "; Secure" } else { "" };
    if let Ok(value) = HeaderValue::from_str(&format!(
        "{name}={value}; Path={path}; Max-Age={seconds}; HttpOnly; SameSite=Lax{secure}"
    )) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
}

async fn channel(service: &Web3Service, id: Uuid) -> Result<PublicRegistration, ApiError> {
    service
        .database
        .web3_channel(id)
        .await
        .map_err(|_| unavailable())?
        .ok_or(ApiError(
            StatusCode::NOT_FOUND,
            "등록된 방송을 찾을 수 없습니다.",
        ))
}

async fn session(
    service: &Web3Service,
    headers: &HeaderMap,
    channel: Uuid,
) -> Result<Vec<u8>, ApiError> {
    let hash = token_hash(&cookie(headers, SESSION).ok_or_else(unauthorized)?);
    if service
        .database
        .owner_session(&hash, channel)
        .await
        .map_err(|_| unavailable())?
    {
        Ok(hash)
    } else {
        Err(unauthorized())
    }
}

async fn bound_vault(
    service: &Web3Service,
    record: &PublicRegistration,
) -> Result<VaultState, ApiError> {
    let payments = payments(service)?;
    let vault = payments
        .client
        .vault(channel_key(
            record.provider.as_str(),
            &record.provider_channel_id,
        ))
        .await
        .map_err(|_| unavailable())?;
    let chain_id = i64::try_from(payments.chain_id).map_err(|_| unavailable())?;
    if !service
        .database
        .bind_channel_vault(
            record.channel_id,
            chain_id,
            &payments.factory,
            &vault.address,
        )
        .await
        .map_err(|_| unavailable())?
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "이 방송의 기존 후원 네트워크 설정과 다릅니다. 운영자의 확인이 필요합니다.",
        ));
    }
    Ok(vault)
}

async fn registration_page(State(state): State<HttpState>) -> Result<Html<String>, ApiError> {
    Ok(Html(
        RegisterTemplate { paths: state.paths }
            .render()
            .map_err(|_| unavailable())?,
    ))
}

async fn wallet_page(
    State(state): State<HttpState>,
    Path(id): Path<Uuid>,
) -> Result<Html<String>, ApiError> {
    channel(service(&state)?, id).await?;
    Ok(Html(
        WalletTemplate {
            paths: state.paths,
            channel_id: id.to_string(),
        }
        .render()
        .map_err(|_| unavailable())?,
    ))
}

async fn wallet_script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../../../../static/js/wallet.js"),
    )
}
async fn wallet_styles() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../../../../static/css/web3.css"),
    )
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct Registration {
    provider: String,
    provider_channel_id: String,
    display_name: String,
}

#[utoipa::path(post, path = "/api/registrations", request_body = Registration, responses((status = 200, description = "Canonical channel and support URL"), (status = 403, description = "Origin rejected")))]
async fn register(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(input): Json<Registration>,
) -> Result<Json<Value>, ApiError> {
    let service = service(&state)?;
    mutation(service, &headers, 0, 30)?;
    let provider: ProviderKind = input.provider.parse().map_err(|_| invalid())?;
    let record = service
        .database
        .register_public_channel(provider, &input.provider_channel_id, &input.display_name)
        .await
        .map_err(|error| match error {
            rill3_db::DbError::RegistrationConflict(_) => invalid(),
            _ => unavailable(),
        })?;
    // Persist the address as soon as configured; never expose a synthetic fallback address.
    if service.payments.is_some() && service.oauth.is_configured(provider) {
        bound_vault(service, &record).await?;
    }
    Ok(Json(
        json!({"channel_id": record.channel_id, "url": format!("{}/support/{}", service.base_path, record.channel_id)}),
    ))
}

#[utoipa::path(get, path = "/api/channels/{channel}/web3", params(("channel" = Uuid, Path)), responses((status = 200, description = "Deployment, native ETH balance, owner and session status")))]
async fn status(
    State(state): State<HttpState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let service = service(&state)?;
    rate(service, 3, 240)?;
    let record = channel(service, id).await?;
    let verified = if cookie(&headers, SESSION).is_some() {
        session(service, &headers, id).await.is_ok()
    } else {
        false
    };
    let configured = service.oauth.is_configured(record.provider);
    let mut body = json!({
        "enabled": false, "reason": "후원 네트워크와 컨트랙트 설정이 준비되지 않았습니다.",
        "provider": record.provider.as_str(), "display_name": record.display_name, "channel_id": id,
        "provider_channel_id": record.provider_channel_id,
        "channel_url": match record.provider {
            ProviderKind::YouTube => Some(format!("https://www.youtube.com/channel/{}", record.provider_channel_id)),
            ProviderKind::Chzzk => Some(format!("https://chzzk.naver.com/live/{}", record.provider_channel_id)),
            _ => None,
        },
        "session_verified": verified, "owner_wallet": Value::Null,
        "oauth_url": if configured { format!("{}/auth/{}/start?channel={id}", service.base_path, record.provider.as_str()) } else { String::new() },
    });
    if !configured {
        body["reason"] =
            json!("이 플랫폼의 공식 방송 계정 인증이 아직 설정되지 않아 후원을 받을 수 없습니다.");
    }
    if let Some(payments) = &service.payments {
        let vault = bound_vault(service, &record).await?;
        if !configured && vault.owner.is_none() {
            return Ok(Json(body));
        }
        body["enabled"] = json!(true);
        body["reason"] = json!("");
        body["chain_id"] = json!(payments.chain_id);
        body["chain_name"] = json!(payments.chain_name);
        body["native_symbol"] = json!("ETH");
        body["factory_address"] = json!(payments.factory);
        body["receive_address"] = json!(vault.address);
        body["channel_key"] = json!(format!(
            "0x{}",
            hex::encode(channel_key(
                record.provider.as_str(),
                &record.provider_channel_id
            ))
        ));
        body["balance_wei"] = json!(vault.balance_wei);
        body["owner_wallet"] = json!(vault.owner);
        body["deployed"] = json!(vault.deployed);
        body["reserved_wallet"] = json!(record.wallet_address);
    }
    Ok(Json(body))
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct Tip {
    wallet: String,
    amount_wei: String,
}

fn transaction(value: impl serde::Serialize, wallet: &str) -> Result<Json<Value>, ApiError> {
    let mut body = serde_json::to_value(value).map_err(|_| unavailable())?;
    body["from"] = json!(wallet);
    Ok(Json(body))
}

#[utoipa::path(post, path = "/api/channels/{channel}/tip", params(("channel" = Uuid, Path)), request_body = Tip, responses((status = 200, description = "Unsigned donation transaction for wallet submission")))]
async fn tip(
    State(state): State<HttpState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(input): Json<Tip>,
) -> Result<Json<Value>, ApiError> {
    let service = service(&state)?;
    mutation(service, &headers, 2, 120)?;
    let wallet = normalize_address(&input.wallet).map_err(|_| invalid())?;
    let record = channel(service, id).await?;
    let vault = bound_vault(service, &record).await?;
    if vault.owner.is_none() && !service.oauth.is_configured(record.provider) {
        return Err(unavailable());
    }
    transaction(
        payments(service)?
            .client
            .tip_transaction(
                channel_key(record.provider.as_str(), &record.provider_channel_id),
                &input.amount_wei,
            )
            .map_err(|_| invalid())?,
        &wallet,
    )
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct Wallet {
    wallet: String,
}

#[utoipa::path(post, path = "/api/channels/{channel}/wallet-challenge", params(("channel" = Uuid, Path)), request_body = Wallet, responses((status = 200, description = "Session-bound one-use personal_sign message"), (status = 401, description = "Broadcaster OAuth required")))]
async fn wallet_challenge(
    State(state): State<HttpState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(input): Json<Wallet>,
) -> Result<Response, ApiError> {
    let service = service(&state)?;
    mutation(service, &headers, 2, 120)?;
    let session_hash = session(service, &headers, id).await?;
    let wallet = normalize_address(&input.wallet).map_err(|_| invalid())?;
    let record = channel(service, id).await?;
    let vault = bound_vault(service, &record).await?;
    if vault.owner.is_some() {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "이미 수령 지갑이 등록되었습니다. 상태를 새로고침해 주세요.",
        ));
    }
    if record
        .wallet_address
        .as_deref()
        .is_some_and(|existing| existing != wallet)
        || vault.owner.as_deref().is_some_and(|owner| owner != wallet)
    {
        return Err(conflict());
    }
    let payments = payments(service)?;
    let nonce = random_token();
    let expires_at = Utc::now() + TimeDelta::minutes(5);
    let message = format!(
        "RILL3 broadcaster wallet claim\nOrigin: {}\nURI: {}{}/support/{id}\nChannel: {}/{}\nChain ID: {}\nFactory: {}\nReceive address: {}\nWallet: {wallet}\nNonce: {nonce}\nExpires at: {}\n\nI bind this wallet permanently to this verified broadcasting channel. Only this wallet may withdraw its donations.",
        service.origin,
        service.origin,
        service.base_path,
        record.provider,
        record.provider_channel_id,
        payments.chain_id,
        payments.factory,
        vault.address,
        expires_at.to_rfc3339()
    );
    service
        .database
        .save_wallet_challenge(&WalletChallenge {
            nonce_hash: token_hash(&nonce),
            session_hash,
            channel_id: id,
            wallet_address: wallet,
            message: message.clone(),
            expires_at,
        })
        .await
        .map_err(|_| unavailable())?;
    let mut response = Json(json!({"message": message})).into_response();
    set_cookie(&mut response, service, WALLET_NONCE, &nonce, 300);
    Ok(response)
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct Claim {
    wallet: String,
    signature: String,
}

#[utoipa::path(post, path = "/api/channels/{channel}/claim", params(("channel" = Uuid, Path)), request_body = Claim, responses((status = 200, description = "Unsigned irreversible owner claim transaction"), (status = 403, description = "Wallet signature invalid"), (status = 409, description = "Expired challenge or different reserved wallet")))]
async fn claim(
    State(state): State<HttpState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(input): Json<Claim>,
) -> Result<Response, ApiError> {
    let service = service(&state)?;
    mutation(service, &headers, 2, 120)?;
    let session_hash = session(service, &headers, id).await?;
    let wallet = normalize_address(&input.wallet).map_err(|_| invalid())?;
    let nonce_hash = token_hash(&cookie(&headers, WALLET_NONCE).ok_or_else(conflict)?);
    let challenge = service
        .database
        .wallet_challenge(&nonce_hash, &session_hash, id)
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(conflict)?;
    if challenge.wallet_address != wallet {
        return Err(conflict());
    }
    verify_wallet_signature(&challenge.message, &input.signature, &wallet).map_err(|_| {
        ApiError(
            StatusCode::FORBIDDEN,
            "지갑 서명을 확인할 수 없습니다. 해당 지갑으로 다시 서명해 주세요.",
        )
    })?;
    let record = channel(service, id).await?;
    let vault = bound_vault(service, &record).await?;
    if vault.owner.is_some() {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "이미 수령 지갑이 등록되었습니다. 상태를 새로고침해 주세요.",
        ));
    }
    let payments = payments(service)?;
    let key = channel_key(record.provider.as_str(), &record.provider_channel_id);
    let deadline = u64::try_from(challenge.expires_at.timestamp()).map_err(|_| invalid())?;
    let signature = payments
        .signer
        .sign_claim(payments.chain_id, &payments.factory, key, &wallet, deadline)
        .map_err(|_| unavailable())?;
    let tx = payments
        .client
        .claim_transaction(key, &wallet, deadline, &signature)
        .map_err(|_| invalid())?;
    if !service
        .database
        .reserve_verified_wallet(&nonce_hash, &session_hash, id, &wallet)
        .await
        .map_err(|_| unavailable())?
    {
        return Err(conflict());
    }
    let mut response = transaction(tx, &wallet)?.into_response();
    set_cookie(&mut response, service, WALLET_NONCE, "", 0);
    Ok(response)
}

#[utoipa::path(post, path = "/api/channels/{channel}/withdraw", params(("channel" = Uuid, Path)), request_body = Wallet, responses((status = 200, description = "Unsigned full-balance withdrawal to on-chain owner"), (status = 403, description = "Not the on-chain owner")))]
async fn withdraw(
    State(state): State<HttpState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(input): Json<Wallet>,
) -> Result<Json<Value>, ApiError> {
    let service = service(&state)?;
    mutation(service, &headers, 2, 120)?;
    let wallet = normalize_address(&input.wallet).map_err(|_| invalid())?;
    let record = channel(service, id).await?;
    let vault = bound_vault(service, &record).await?;
    // Once claimed, ownership is on-chain and does not depend on an OAuth session.
    if vault.owner.as_deref() != Some(&wallet) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "이 방송에 연결된 소유자 지갑만 출금할 수 있습니다.",
        ));
    }
    if vault.balance_wei == "0" {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "출금할 후원금이 없습니다.",
        ));
    }
    transaction(
        payments(service)?
            .client
            .withdraw_transaction(&vault.address, &wallet)
            .map_err(|_| invalid())?,
        &wallet,
    )
}

#[derive(Deserialize)]
struct StartQuery {
    channel: Uuid,
}

#[utoipa::path(get, path = "/auth/{provider}/start", params(("provider" = String, Path), ("channel" = Uuid, Query)), responses((status = 303, description = "Official provider authorization URL")))]
async fn oauth_start(
    State(state): State<HttpState>,
    Path(provider): Path<String>,
    Query(query): Query<StartQuery>,
) -> Result<Response, ApiError> {
    let service = service(&state)?;
    rate(service, 1, 60)?;
    let record = channel(service, query.channel).await?;
    if record.provider.as_str() != provider {
        return Err(invalid());
    }
    let state_token = random_token();
    let browser_token = random_token();
    let destination = service
        .oauth
        .authorization_url(record.provider, &state_token)
        .map_err(|_| unavailable())?;
    service
        .database
        .save_oauth_state(
            &token_hash(&state_token),
            &token_hash(&browser_token),
            record.channel_id,
            Utc::now() + TimeDelta::minutes(10),
        )
        .await
        .map_err(|_| unavailable())?;
    let mut response = Redirect::to(destination.as_str()).into_response();
    set_cookie(&mut response, service, OAUTH_BINDING, &browser_token, 600);
    Ok(response)
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: String,
    error: Option<String>,
}

#[utoipa::path(get, path = "/auth/{provider}/callback", params(("provider" = String, Path), ("state" = String, Query), ("code" = String, Query)), responses((status = 303, description = "Verified broadcaster session and support page redirect"), (status = 403, description = "Wrong provider account")))]
async fn oauth_callback(
    State(state): State<HttpState>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    Query(query): Query<CallbackQuery>,
) -> Result<Response, ApiError> {
    let service = service(&state)?;
    rate(service, 1, 60)?;
    let browser = cookie(&headers, OAUTH_BINDING).ok_or_else(unauthorized)?;
    let record = service
        .database
        .consume_oauth_state(&token_hash(&query.state), &token_hash(&browser))
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(unauthorized)?;
    if record.provider.as_str() != provider || query.error.is_some() {
        return Err(unauthorized());
    }
    let code = query
        .code
        .filter(|code| !code.is_empty() && code.len() <= 4096)
        .ok_or_else(unauthorized)?;
    let identity = service
        .oauth
        .verify_code(record.provider, &code, &query.state)
        .await
        .map_err(|_| unauthorized())?;
    if identity.provider != record.provider
        || !identity
            .provider_channel_ids
            .contains(&record.provider_channel_id)
    {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "로그인한 방송 계정이 이 채널의 소유자가 아닙니다.",
        ));
    }
    let token = random_token();
    service
        .database
        .create_owner_session(
            &token_hash(&token),
            record.channel_id,
            record.provider,
            &record.provider_channel_id,
            Utc::now() + TimeDelta::minutes(15),
        )
        .await
        .map_err(|_| unavailable())?;
    let mut response = Redirect::to(&format!(
        "{}/support/{}",
        service.base_path, record.channel_id
    ))
    .into_response();
    set_cookie(&mut response, service, SESSION, &token, 900);
    set_cookie(&mut response, service, OAUTH_BINDING, "", 0);
    Ok(response)
}

#[utoipa::path(post, path = "/api/auth/logout", responses((status = 200, description = "Owner session and challenges invalidated")))]
async fn logout(State(state): State<HttpState>, headers: HeaderMap) -> Result<Response, ApiError> {
    let service = service(&state)?;
    mutation(service, &headers, 2, 120)?;
    if let Some(token) = cookie(&headers, SESSION) {
        service
            .database
            .logout(&token_hash(&token))
            .await
            .map_err(|_| unavailable())?;
    }
    let mut response = Json(json!({"ok": true})).into_response();
    for name in [SESSION, OAUTH_BINDING, WALLET_NONCE] {
        set_cookie(&mut response, service, name, "", 0);
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csrf_requires_exact_origin_and_rejects_cross_site() {
        let mut headers = HeaderMap::new();
        assert!(!origin_allowed(&headers, "https://rill3.example"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://rill3.example.evil"),
        );
        assert!(!origin_allowed(&headers, "https://rill3.example"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://rill3.example"),
        );
        assert!(origin_allowed(&headers, "https://rill3.example"));
        headers.insert("sec-fetch-site", HeaderValue::from_static("cross-site"));
        assert!(!origin_allowed(&headers, "https://rill3.example"));
    }

    #[test]
    fn cookies_require_full_random_tokens_and_no_ambiguity() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("{SESSION}={}", "a".repeat(64))).unwrap(),
        );
        assert!(cookie(&headers, SESSION).is_some());
        headers.append(
            header::COOKIE,
            HeaderValue::from_str(&format!("{SESSION}={}", "b".repeat(64))).unwrap(),
        );
        assert!(cookie(&headers, SESSION).is_none());
    }

    #[tokio::test]
    // One end-to-end request sequence shares its database-backed router and channel.
    #[allow(clippy::too_many_lines)]
    async fn public_registration_routes_enforce_origin_and_fail_closed_without_chain() {
        use clap::Parser;
        use http_body_util::BodyExt;
        use std::{sync::Arc, time::Duration};
        use tower::ServiceExt;
        let Ok(url) = std::env::var("RILL3_TEST_DATABASE_URL") else {
            return;
        };
        let database = rill3_db::Database::connect(&rill3_db::DatabaseOptions::new(&url, 2))
            .await
            .unwrap();
        database.migrate().await.unwrap();
        let crate::config::Command::Server(args) = crate::config::Cli::try_parse_from([
            "rill3",
            "server",
            "--database-url",
            &url,
            "--base-path",
            "/rill3",
            "--public-origin",
            "http://localhost:3000",
        ])
        .unwrap()
        .command
        else {
            panic!("server expected")
        };
        let service = Web3Service::new(database.clone(), &args).await.unwrap();
        let mut state = HttpState::new(
            Arc::new(database),
            &args.base_path,
            crate::views::EmbedConfig {
                public_origin: args.public_origin.clone(),
                twitch_parent: "localhost".to_owned(),
            },
            crate::http::default_provider_statuses(false, false),
            crate::webhooks::WebhookConfig::new(
                65_536,
                None,
                None,
                None,
                rill3_providers::YOUTUBE_WEBSUB_TOPIC_PREFIX.to_owned(),
            ),
        );
        state.web3 = Some(Arc::new(service));
        let app = crate::http::router(state, Duration::from_secs(5));
        let id = format!("9{}", Uuid::new_v4().as_u128() % 1_000_000_000_000);
        let input =
            json!({"provider":"twitch", "provider_channel_id": id, "display_name":"테스트 방송"});
        let request = |origin: &str, path: &str, body: &Value| {
            axum::http::Request::post(path)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ORIGIN, origin)
                .body(axum::body::Body::from(body.to_string()))
                .unwrap()
        };
        let denied = app
            .clone()
            .oneshot(request(
                "https://evil.example",
                "/rill3/api/registrations",
                &input,
            ))
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        let registered = app
            .clone()
            .oneshot(request(
                "http://localhost:3000",
                "/rill3/api/registrations",
                &input,
            ))
            .await
            .unwrap();
        assert_eq!(registered.status(), StatusCode::OK);
        assert_eq!(registered.headers()[header::CACHE_CONTROL], "no-store");
        let registration: Value =
            serde_json::from_slice(&registered.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let channel = registration["channel_id"].as_str().unwrap();
        assert_eq!(registration["url"], format!("/rill3/support/{channel}"));
        let status = app
            .clone()
            .oneshot(
                axum::http::Request::get(format!("/rill3/api/channels/{channel}/web3"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::OK);
        let status: Value =
            serde_json::from_slice(&status.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(status["enabled"], false);
        assert!(status.get("receive_address").is_none());
        let wallet = json!({"wallet":"0x1111111111111111111111111111111111111111"});
        let unauthenticated = app
            .clone()
            .oneshot(request(
                "http://localhost:3000",
                &format!("/rill3/api/channels/{channel}/wallet-challenge"),
                &wallet,
            ))
            .await
            .unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
        let page = app
            .oneshot(
                axum::http::Request::get("/rill3/register")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(page.status(), StatusCode::OK);
        let page = String::from_utf8(
            page.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        assert!(page.contains("/rill3/static/wallet.js"));
    }
}

#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        register,
        status,
        tip,
        wallet_challenge,
        claim,
        withdraw,
        oauth_start,
        oauth_callback,
        logout
    ),
    components(schemas(Registration, Tip, Wallet, Claim))
)]
struct Web3Api;

pub(crate) fn openapi() -> utoipa::openapi::OpenApi {
    <Web3Api as utoipa::OpenApi>::openapi()
}
