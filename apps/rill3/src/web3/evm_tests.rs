//! Explicitly selected integration coverage against disposable `PostgreSQL` and Anvil.
//! Real provider OAuth is covered separately; this test installs an exact-channel
//! owner-session fixture after exercising the unauthenticated HTTP rejection.

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
};
use chrono::{TimeDelta, Utc};
use clap::Parser;
use http_body_util::BodyExt;
use rill3_auth::{random_token, token_hash};
use rill3_db::{Database, DatabaseOptions};
use rill3_domain::ProviderKind;
use serde_json::{Value, json};
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;

use super::Web3Service;
use crate::{
    config::{Cli, Command},
    http::{HttpState, default_provider_statuses},
    views::EmbedConfig,
    webhooks::WebhookConfig,
};

const ORIGIN: &str = "http://localhost:3000";
const BASE: &str = "/rill3";
// Public development fixtures only. This key must never hold real funds.
const TEST_ATTESTOR_KEY: &str = "0000000000000000000000000000000000000000000000000000000000000001";
const TIP_WEI: &str = "1000000000000000";

struct Fixture {
    app: Router,
    database: Database,
    rpc: reqwest::Client,
    rpc_url: Url,
}

impl Fixture {
    async fn new() -> Self {
        let database_url = std::env::var("RILL3_TEST_DATABASE_URL")
            .expect("explicit local PostgreSQL test URL is required");
        let rpc_url =
            std::env::var("RILL3_TEST_EVM_RPC").expect("explicit local Anvil RPC URL is required");
        let factory = std::env::var("RILL3_TEST_EVM_FACTORY")
            .expect("explicit deployed local factory address is required");
        let code_hash = std::env::var("RILL3_TEST_EVM_CODE_HASH")
            .expect("explicit local factory runtime code hash is required");
        let database_endpoint = Url::parse(&database_url).unwrap();
        let rpc_url = Url::parse(&rpc_url).unwrap();
        for endpoint in [&database_endpoint, &rpc_url] {
            assert!(
                matches!(
                    endpoint.host_str(),
                    Some("localhost" | "127.0.0.1" | "[::1]")
                ),
                "this test only connects to loopback disposable services"
            );
        }
        assert_eq!(rpc_url.scheme(), "http", "local Anvil must use HTTP");
        let database = Database::connect(&DatabaseOptions::new(&database_url, 4))
            .await
            .unwrap();
        database.migrate().await.unwrap();
        let Command::Server(args) = Cli::try_parse_from([
            "rill3",
            "server",
            "--database-url",
            &database_url,
            "--base-path",
            BASE,
            "--public-origin",
            ORIGIN,
            "--chain-id",
            "31337",
            "--chain-name",
            "Local Anvil",
            "--rpc-url",
            rpc_url.as_str(),
            "--factory",
            &factory,
            "--attestor-key",
            TEST_ATTESTOR_KEY,
            "--factory-code-hash",
            &code_hash,
            "--twitch-client-id",
            "local-test-client",
            "--twitch-client-secret",
            "local-test-secret",
        ])
        .unwrap()
        .command
        else {
            panic!("server arguments expected")
        };
        args.validate().unwrap();
        let service = Web3Service::new(database.clone(), &args).await.unwrap();
        let mut state = HttpState::new(
            Arc::new(database.clone()),
            BASE,
            EmbedConfig {
                public_origin: args.public_origin,
                twitch_parent: "localhost".to_owned(),
            },
            default_provider_statuses(false, false),
            WebhookConfig::default(),
        );
        state.web3 = Some(Arc::new(service));
        Self {
            app: crate::http::router(state, Duration::from_secs(30)),
            database,
            rpc: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            rpc_url,
        }
    }

    async fn request(
        &self,
        path: &str,
        body: Option<Value>,
        cookie: Option<&str>,
    ) -> (StatusCode, HeaderMap, Value) {
        let mut request = if body.is_some() {
            Request::post(format!("{BASE}{path}"))
        } else {
            Request::get(format!("{BASE}{path}"))
        };
        request = request
            .header(header::ORIGIN, ORIGIN)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        let body = body.map_or_else(Body::empty, |value| Body::from(value.to_string()));
        let response = self
            .app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).expect("HTTP response should be JSON");
        (status, headers, json)
    }

    async fn rpc(&self, method: &str, params: Value) -> Value {
        let response: Value = self
            .rpc
            .post(self.rpc_url.clone())
            .json(&json!({"jsonrpc":"2.0", "id":1, "method":method, "params":params}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(
            response.get("error").is_none(),
            "local RPC {method} failed: {response}"
        );
        response
            .get("result")
            .expect("RPC result is present")
            .clone()
    }

    async fn send_transaction(&self, transaction: &Value, wallet: &str) -> Value {
        assert_eq!(transaction["from"], wallet);
        assert_eq!(transaction["chain_id"], 31_337);
        let hash = self
            .rpc(
                "eth_sendTransaction",
                json!([{
                    "from": wallet, "to":transaction["to"],
                    "data":transaction["data"], "value":transaction["value"],
                }]),
            )
            .await;
        assert!(hash.as_str().is_some_and(|value| value.starts_with("0x")));
        for _ in 0..50 {
            let receipt = self.rpc("eth_getTransactionReceipt", json!([hash])).await;
            if !receipt.is_null() {
                assert_eq!(
                    receipt["status"], "0x1",
                    "local transaction reverted: {receipt}"
                );
                return receipt;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("Anvil did not mine the transaction within five seconds");
    }

    async fn status(&self, channel_path: &str) -> Value {
        let (status, _, body) = self
            .request(&format!("{channel_path}/web3"), None, None)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["enabled"], true);
        body
    }
}

#[tokio::test]
#[ignore = "requires explicit disposable PostgreSQL and deployed local Anvil vault fixture"]
#[allow(clippy::too_many_lines)]
async fn local_evm_registration_donation_owner_claim_and_withdrawal() {
    let fixture = Fixture::new().await;
    assert_eq!(fixture.rpc("eth_chainId", json!([])).await, "0x7a69");
    let accounts = fixture.rpc("eth_accounts", json!([])).await;
    let wallet = accounts[0]
        .as_str()
        .expect("Anvil account 0 exists")
        .to_ascii_lowercase();
    let other_wallet = accounts[1]
        .as_str()
        .expect("Anvil account 1 exists")
        .to_ascii_lowercase();
    let provider_id = format!("9{}", Uuid::new_v4().as_u128() % 1_000_000_000_000_000_000);
    let (status, _, registration) = fixture.request("/api/registrations", Some(json!({
        "provider":"twitch", "provider_channel_id":provider_id, "display_name":"Local EVM test broadcaster",
    })), None).await;
    assert_eq!(status, StatusCode::OK, "{registration}");
    let channel_id: Uuid = registration["channel_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let channel_path = format!("/api/channels/{channel_id}");
    let before = fixture.status(&channel_path).await;
    assert_eq!(before["provider_channel_id"], provider_id);
    assert_eq!(before["balance_wei"], "0");
    assert!(before["owner_wallet"].is_null());
    let receive_address = before["receive_address"].as_str().unwrap().to_owned();

    let (status, _, tip) = fixture
        .request(
            &format!("{channel_path}/tip"),
            Some(json!({
                "wallet":wallet, "amount_wei":TIP_WEI,
            })),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{tip}");
    fixture.send_transaction(&tip, &wallet).await;
    let funded = fixture.status(&channel_path).await;
    assert_eq!(funded["receive_address"], receive_address);
    assert_eq!(funded["balance_wei"], TIP_WEI);
    assert_eq!(funded["deployed"], true);
    assert!(funded["owner_wallet"].is_null());

    let (status, _, _) = fixture
        .request(
            &format!("{channel_path}/claim"),
            Some(json!({
                "wallet":wallet, "signature":"0x",
            })),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Explicit test fixture: simulate only the completed provider proof, without
    // calling Twitch or introducing a production bypass. Actual OAuth is mocked
    // and validated in the auth crate's independent provider identity tests.
    let session = random_token();
    fixture
        .database
        .create_owner_session(
            &token_hash(&session),
            channel_id,
            ProviderKind::Twitch,
            &provider_id,
            Utc::now() + TimeDelta::minutes(15),
        )
        .await
        .unwrap();
    let session_cookie = format!("rill3_owner={session}");
    let (status, headers, challenge) = fixture
        .request(
            &format!("{channel_path}/wallet-challenge"),
            Some(json!({"wallet":wallet})),
            Some(&session_cookie),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{challenge}");
    let nonce_cookie = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with("rill3_wallet_nonce="))
        .expect("wallet nonce cookie must be issued")
        .split(';')
        .next()
        .unwrap();
    let signed_message = challenge["message"].as_str().unwrap();
    assert!(signed_message.contains(&receive_address));
    assert!(signed_message.contains(&format!("Channel: twitch/{provider_id}")));
    let signature = fixture
        .rpc(
            "personal_sign",
            json!([format!("0x{}", hex::encode(signed_message)), wallet,]),
        )
        .await;
    let claim_input = json!({"wallet":wallet, "signature":signature});
    let cookies = format!("{session_cookie}; {nonce_cookie}");
    let (status, _, claim) = fixture
        .request(
            &format!("{channel_path}/claim"),
            Some(claim_input.clone()),
            Some(&cookies),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{claim}");
    assert!(
        fixture.status(&channel_path).await["owner_wallet"].is_null(),
        "a reserved attestation must not appear as an on-chain claim"
    );
    let (status, _, _) = fixture
        .request(
            &format!("{channel_path}/claim"),
            Some(claim_input),
            Some(&cookies),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "consumed wallet nonce cannot replay"
    );
    fixture.send_transaction(&claim, &wallet).await;
    let claimed = fixture.status(&channel_path).await;
    assert_eq!(claimed["owner_wallet"], wallet);
    assert_eq!(claimed["balance_wei"], TIP_WEI);

    let (status, _, _) = fixture
        .request(
            &format!("{channel_path}/withdraw"),
            Some(json!({"wallet":other_wallet})),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // On-chain ownership, not a surviving web session, authorizes withdrawal.
    let (status, _, withdrawal) = fixture
        .request(
            &format!("{channel_path}/withdraw"),
            Some(json!({"wallet":wallet})),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{withdrawal}");
    assert_eq!(withdrawal["to"], receive_address);
    let owner_before = fixture
        .rpc("eth_getBalance", json!([wallet, "latest"]))
        .await;
    let receipt = fixture.send_transaction(&withdrawal, &wallet).await;
    let owner_after = fixture
        .rpc("eth_getBalance", json!([wallet, "latest"]))
        .await;
    let quantity = |value: &Value| {
        u128::from_str_radix(value.as_str().unwrap().strip_prefix("0x").unwrap(), 16).unwrap()
    };
    let gas_cost = quantity(&receipt["gasUsed"]) * quantity(&receipt["effectiveGasPrice"]);
    assert_eq!(
        quantity(&owner_after) + gas_cost,
        quantity(&owner_before) + TIP_WEI.parse::<u128>().unwrap(),
        "the exact donated amount must reach the verified owner, accounting for gas"
    );
    let final_status = fixture.status(&channel_path).await;
    assert_eq!(final_status["balance_wei"], "0");
    assert_eq!(final_status["owner_wallet"], wallet);
}
