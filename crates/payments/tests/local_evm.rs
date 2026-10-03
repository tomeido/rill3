//! Explicit local-only interoperability test; see contracts/README.md.
use rill3_payments::{AttestationSigner, TransactionRequest, VaultClient, channel_key};
use serde_json::{Value, json};

async fn rpc(http: &reqwest::Client, url: &str, method: &str, params: Value) -> Value {
    let value: Value = http
        .post(url)
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        value.get("error").is_none(),
        "local RPC rejected {method}: {value}"
    );
    value["result"].clone()
}

async fn send(http: &reqwest::Client, url: &str, from: &str, transaction: TransactionRequest) {
    let hash = rpc(
        http,
        url,
        "eth_sendTransaction",
        json!([{
            "from": from, "to": transaction.to, "data": transaction.data,
            "value": transaction.value, "gas": "0xf4240",
        }]),
    )
    .await;
    for _ in 0..50 {
        let receipt = rpc(http, url, "eth_getTransactionReceipt", json!([hash])).await;
        if !receipt.is_null() {
            assert_eq!(receipt["status"], "0x1", "local transaction reverted");
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("local transaction was not mined within five seconds");
}

#[tokio::test]
#[ignore = "requires explicitly configured local Anvil and deployed contracts"]
async fn rust_signatures_and_calldata_execute_against_solidity() {
    let url = std::env::var("RILL3_TEST_EVM_RPC").expect("local Anvil RPC required");
    assert!(url.starts_with("http://127.0.0.1:"), "local node only");
    let factory = std::env::var("RILL3_TEST_EVM_FACTORY").expect("local factory required");
    let code_hash = std::env::var("RILL3_TEST_EVM_CODE_HASH").expect("local runtime hash required");
    // Public test-only key. It must never be used on a public chain.
    let signer =
        AttestationSigner::new("0000000000000000000000000000000000000000000000000000000000000001")
            .unwrap();
    let client = VaultClient::new(&url, 31337, &factory, signer.address(), &code_hash).unwrap();
    client.validate().await.unwrap();
    let http = reqwest::Client::new();
    let accounts = rpc(&http, &url, "eth_accounts", json!([])).await;
    let owner = accounts[0].as_str().unwrap();
    let id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .to_string();
    let key = channel_key("twitch", &id);
    let before = client.vault(key).await.unwrap();
    assert!(!before.deployed);
    assert!(before.owner.is_none());
    send(
        &http,
        &url,
        owner,
        client.tip_transaction(key, "1000000000000000").unwrap(),
    )
    .await;
    let funded = client.vault(key).await.unwrap();
    assert_eq!(funded.address, before.address);
    assert_eq!(funded.balance_wei, "1000000000000000");
    assert!(funded.deployed && funded.owner.is_none());
    let block = rpc(
        &http,
        &url,
        "eth_getBlockByNumber",
        json!(["latest", false]),
    )
    .await;
    let now = u64::from_str_radix(
        block["timestamp"]
            .as_str()
            .unwrap()
            .trim_start_matches("0x"),
        16,
    )
    .unwrap();
    let deadline = now + 300;
    let proof = signer
        .sign_claim(31337, &factory, key, owner, deadline)
        .unwrap();
    send(
        &http,
        &url,
        owner,
        client
            .claim_transaction(key, owner, deadline, &proof)
            .unwrap(),
    )
    .await;
    let claimed = client.vault(key).await.unwrap();
    assert_eq!(claimed.owner.as_deref(), Some(owner));
    send(
        &http,
        &url,
        owner,
        client
            .withdraw_transaction(&claimed.address, owner)
            .unwrap(),
    )
    .await;
    assert_eq!(client.vault(key).await.unwrap().balance_wei, "0");
}
