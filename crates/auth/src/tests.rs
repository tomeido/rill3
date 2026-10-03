use super::*;
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, header, method, path, query_param},
};

fn config(provider: ProviderKind) -> OAuthProviderConfig {
    OAuthProviderConfig {
        provider,
        client_id: "our-client".to_owned(),
        client_secret: "test-secret".into(),
        redirect_uri: Url::parse(&format!(
            "https://rill3.example/app/auth/{provider}/callback"
        ))
        .unwrap(),
    }
}

async fn client(provider: ProviderKind) -> (OAuthClient, MockServer) {
    let server = MockServer::start().await;
    let mut client = OAuthClient::new(vec![config(provider)]).unwrap();
    client.endpoints = Endpoints {
        twitch_token: format!("{}/token", server.uri()),
        twitch_identity: format!("{}/identity", server.uri()),
        youtube_token: format!("{}/token", server.uri()),
        youtube_identity: format!("{}/identity", server.uri()),
        chzzk_token: format!("{}/token", server.uri()),
        chzzk_identity: format!("{}/identity", server.uri()),
    };
    (client, server)
}

async fn token_mock(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("client_secret=test-secret"))
        .and(body_string_contains("grant_type=authorization_code"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token":"user-secret", "token_type":"bearer"})),
        )
        .mount(server)
        .await;
}

#[test]
fn config_rejects_unsafe_redirects_duplicate_and_link_only() {
    for redirect in [
        "http://public.example/callback",
        "https://user@rill3.example/callback",
        "https://rill3.example/callback?x=1",
        "https://rill3.example/callback#token",
    ] {
        let mut config = config(ProviderKind::Twitch);
        config.redirect_uri = Url::parse(redirect).unwrap();
        assert!(OAuthClient::new(vec![config]).is_err());
    }
    assert!(OAuthClient::new(vec![config(ProviderKind::LinkOnly)]).is_err());
    assert!(
        OAuthClient::new(vec![
            config(ProviderKind::Twitch),
            config(ProviderKind::Twitch)
        ])
        .is_err()
    );
}

#[test]
fn consent_uses_trusted_callback_state_and_least_privilege() {
    for provider in [
        ProviderKind::Twitch,
        ProviderKind::YouTube,
        ProviderKind::Chzzk,
    ] {
        let client = OAuthClient::new(vec![config(provider)]).unwrap();
        let url = client.authorization_url(provider, "random-state").unwrap();
        let params = url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(params.get("state").unwrap(), "random-state");
        let key = if provider == ProviderKind::Chzzk {
            "redirectUri"
        } else {
            "redirect_uri"
        };
        assert_eq!(
            params.get(key).unwrap(),
            config(provider).redirect_uri.as_str()
        );
        assert!(!params.contains_key("client_secret"));
        if provider == ProviderKind::YouTube {
            assert_eq!(
                params.get("scope").unwrap(),
                "https://www.googleapis.com/auth/youtube.readonly"
            );
        }
    }
    let client = OAuthClient::new(vec![]).unwrap();
    assert!(
        client
            .authorization_url(ProviderKind::Twitch, "state")
            .is_err()
    );
}

#[test]
fn bearer_tokens_have_unique_256_bit_hashes_and_debug_redacts_secrets() {
    let first = random_token();
    let second = random_token();
    assert_ne!(first, second);
    assert_eq!(first.len(), 64);
    assert_eq!(token_hash(&first).len(), 32);
    assert_ne!(token_hash(&first), token_hash(&second));
    assert!(!format!("{:?}", config(ProviderKind::Twitch)).contains("test-secret"));
}

#[tokio::test]
async fn twitch_requires_matching_client_and_user_token() {
    for (client_id, user_id, accepted) in [
        ("our-client", Some("12345"), true),
        ("other-client", Some("12345"), false),
        ("our-client", None, false),
    ] {
        let (client, server) = client(ProviderKind::Twitch).await;
        token_mock(&server).await;
        Mock::given(method("GET"))
            .and(path("/identity"))
            .and(header("authorization", "Bearer user-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"client_id":client_id, "user_id":user_id, "expires_in":3600}),
            ))
            .mount(&server)
            .await;
        let result = client
            .verify_code(ProviderKind::Twitch, "code", "state")
            .await;
        assert_eq!(result.is_ok(), accepted);
        if let Ok(identity) = result {
            assert_eq!(identity.provider_channel_ids, ["12345"]);
        }
    }
}

#[tokio::test]
async fn youtube_only_reads_authenticated_owned_channels() {
    let (client, server) = client(ProviderKind::YouTube).await;
    token_mock(&server).await;
    Mock::given(method("GET"))
        .and(path("/identity"))
        .and(header("authorization", "Bearer user-secret"))
        .and(query_param("mine", "true"))
        .and(query_param("part", "id"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"items":[{"id":"UCabcdefghijklmnopqrstuv"}]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let identity = client
        .verify_code(ProviderKind::YouTube, "code", "state")
        .await
        .unwrap();
    assert_eq!(identity.provider_channel_ids, ["UCabcdefghijklmnopqrstuv"]);
}

#[tokio::test]
async fn chzzk_sends_state_and_reads_me_not_public_channel() {
    let (client, server) = client(ProviderKind::Chzzk).await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("\"state\":\"browser-state\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"code":200,"content":{"accessToken":"user-secret", "tokenType":"Bearer"}}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/identity"))
        .and(header("authorization", "Bearer user-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"code":200,"content":{"channelId":"0123456789abcdef0123456789abcdef"}}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let identity = client
        .verify_code(ProviderKind::Chzzk, "code", "browser-state")
        .await
        .unwrap();
    assert_eq!(
        identity.provider_channel_ids,
        ["0123456789abcdef0123456789abcdef"]
    );
}

#[tokio::test]
async fn response_bounds_and_redirects_fail_without_exposing_tokens() {
    for response in [
        ResponseTemplate::new(302).insert_header("Location", "http://127.0.0.1:1/stolen"),
        ResponseTemplate::new(200).set_body_string("x".repeat(65_537)),
        ResponseTemplate::new(500).set_body_string("test-secret"),
    ] {
        let (client, server) = client(ProviderKind::Twitch).await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        let error = client
            .verify_code(ProviderKind::Twitch, "secret-code", "state")
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("secret"));
    }
}
