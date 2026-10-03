use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use url::Url;

use crate::crypto::{ZERO, address_word, decode_signature, keccak, u64_word};
use crate::{PaymentError, Result, normalize_address};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TransactionRequest {
    pub to: String,
    pub data: String,
    /// JSON-RPC hexadecimal quantity in wei.
    pub value: String,
    pub chain_id: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VaultState {
    pub address: String,
    /// Exact decimal integer, never floating point.
    pub balance_wei: String,
    pub owner: Option<String>,
    pub deployed: bool,
}

/// Read-only RPC. Signing and sending transactions are always the user's wallet's responsibility.
#[derive(Clone)]
pub struct VaultClient {
    http: reqwest::Client,
    rpc_url: Url,
    chain_id: u64,
    factory: String,
    attestor: String,
    factory_code_hash: [u8; 32],
}

impl VaultClient {
    /// # Errors
    /// Rejects incomplete addresses, runtime hashes, invalid chain IDs, and unsafe RPC schemes.
    pub fn new(
        rpc_url: &str,
        chain_id: u64,
        factory: &str,
        attestor: &str,
        expected_factory_code_hash: &str,
    ) -> Result<Self> {
        let rpc_url = Url::parse(rpc_url).map_err(|_| PaymentError::Configuration)?;
        let local_http = rpc_url.scheme() == "http"
            && matches!(
                rpc_url.host_str(),
                Some("localhost" | "127.0.0.1" | "[::1]")
            );
        if chain_id == 0
            || (!local_http && rpc_url.scheme() != "https")
            || !rpc_url.username().is_empty()
            || rpc_url.password().is_some()
            || rpc_url.fragment().is_some()
        {
            return Err(PaymentError::Configuration);
        }
        let code_hash = hex::decode(
            expected_factory_code_hash
                .strip_prefix("0x")
                .unwrap_or(expected_factory_code_hash),
        )
        .map_err(|_| PaymentError::Configuration)?;
        let factory_code_hash = code_hash
            .try_into()
            .map_err(|_| PaymentError::Configuration)?;
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| PaymentError::Configuration)?,
            rpc_url,
            chain_id,
            factory: normalize_address(factory)?,
            attestor: normalize_address(attestor)?,
            factory_code_hash,
        })
    }

    /// Verify deployment before exposing a usable receiving address or transaction.
    ///
    /// # Errors
    /// Fails on unavailable/malformed RPC responses or deployment/configuration mismatches.
    pub async fn validate(&self) -> Result<()> {
        let chain = self.rpc("eth_chainId", json!([])).await?;
        let chain = chain.strip_prefix("0x").ok_or(PaymentError::Rpc)?;
        if u64::from_str_radix(chain, 16).map_err(|_| PaymentError::Rpc)? != self.chain_id {
            return Err(PaymentError::DeploymentMismatch);
        }
        let code = decode_hex(
            &self
                .rpc("eth_getCode", json!([self.factory, "latest"]))
                .await?,
        )?;
        if code.is_empty() || keccak(&code) != self.factory_code_hash {
            return Err(PaymentError::DeploymentMismatch);
        }
        let attestor = self
            .call(&self.factory, &encode_call("attestor()", &[]))
            .await?;
        if decode_address_word(&attestor)? != self.attestor {
            return Err(PaymentError::DeploymentMismatch);
        }
        Ok(())
    }

    /// # Errors
    /// Fails when deployment validation or a vault-state RPC read fails.
    pub async fn vault(&self, key: [u8; 32]) -> Result<VaultState> {
        self.validate().await?;
        let encoded = self
            .call(&self.factory, &encode_call("predictVault(bytes32)", &key))
            .await?;
        let address = normalize_address(&decode_address_word(&encoded)?)?;
        let code = self.rpc("eth_getCode", json!([address, "latest"])).await?;
        let deployed = !decode_hex(&code)?.is_empty();
        let balance = self
            .rpc("eth_getBalance", json!([address, "latest"]))
            .await?;
        let balance_wei = hex_quantity_decimal(&balance)?;
        let owner = if deployed {
            let raw = self.call(&address, &encode_call("owner()", &[])).await?;
            let owner = decode_address_word(&raw)?;
            if owner == ZERO {
                None
            } else {
                Some(normalize_address(&owner)?)
            }
        } else {
            None
        };
        Ok(VaultState {
            address,
            balance_wei,
            owner,
            deployed,
        })
    }

    /// Deploys the deterministic inbox and forwards the donation in one transaction.
    ///
    /// # Errors
    /// Rejects zero, non-decimal, negative, fractional, or overflowing amounts.
    pub fn tip_transaction(&self, key: [u8; 32], amount_wei: &str) -> Result<TransactionRequest> {
        let amount = decimal_word(amount_wei)?;
        if amount == [0; 32] {
            return Err(PaymentError::Amount);
        }
        Ok(self.transaction(
            &self.factory,
            encode_call("deployAndTip(bytes32)", &key),
            word_quantity(&amount),
        ))
    }

    /// # Errors
    /// Rejects malformed owners or signatures before constructing calldata.
    pub fn claim_transaction(
        &self,
        key: [u8; 32],
        owner: &str,
        deadline: u64,
        signature: &str,
    ) -> Result<TransactionRequest> {
        let signature = decode_signature(signature)?;
        let mut arguments = Vec::with_capacity(256);
        arguments.extend(key);
        arguments.extend(address_word(owner)?);
        arguments.extend(u64_word(deadline));
        arguments.extend(u64_word(128));
        arguments.extend(u64_word(65));
        arguments.extend(signature);
        arguments.resize(256, 0);
        Ok(self.transaction(
            &self.factory,
            encode_call("claim(bytes32,address,uint256,bytes)", &arguments),
            "0x0".to_owned(),
        ))
    }

    /// The contract independently enforces the caller and always transfers to its owner.
    ///
    /// # Errors
    /// Rejects malformed or zero vault/owner addresses.
    pub fn withdraw_transaction(&self, vault: &str, owner: &str) -> Result<TransactionRequest> {
        normalize_address(owner)?;
        Ok(self.transaction(
            &normalize_address(vault)?,
            encode_call("withdraw()", &[]),
            "0x0".to_owned(),
        ))
    }

    fn transaction(&self, to: &str, data: String, value: String) -> TransactionRequest {
        TransactionRequest {
            to: to.to_owned(),
            data,
            value,
            chain_id: self.chain_id,
        }
    }

    async fn call(&self, to: &str, data: &str) -> Result<String> {
        self.rpc("eth_call", json!([{ "to": to, "data": data }, "latest"]))
            .await
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<String> {
        let mut response = self
            .http
            .post(self.rpc_url.clone())
            .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }))
            .send()
            .await
            .map_err(|_| PaymentError::Rpc)?;
        if !response.status().is_success()
            || response.content_length().is_some_and(|len| len > 131_072)
        {
            return Err(PaymentError::Rpc);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| PaymentError::Rpc)? {
            if body.len() + chunk.len() > 131_072 {
                return Err(PaymentError::Rpc);
            }
            body.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&body).map_err(|_| PaymentError::Rpc)?;
        if value.get("error").is_some() || value.get("id") != Some(&json!(1)) {
            return Err(PaymentError::Rpc);
        }
        value
            .get("result")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or(PaymentError::Rpc)
    }
}

fn encode_call(signature: &str, arguments: &[u8]) -> String {
    let selector = keccak(signature.as_bytes());
    format!(
        "0x{}{}",
        hex::encode(&selector[..4]),
        hex::encode(arguments)
    )
}

fn decode_hex(value: &str) -> Result<Vec<u8>> {
    hex::decode(value.strip_prefix("0x").ok_or(PaymentError::Rpc)?).map_err(|_| PaymentError::Rpc)
}

fn decode_address_word(value: &str) -> Result<String> {
    let bytes = decode_hex(value)?;
    if bytes.len() != 32 || bytes[..12] != [0; 12] {
        return Err(PaymentError::Rpc);
    }
    Ok(format!("0x{}", hex::encode(&bytes[12..])))
}

fn decimal_word(decimal: &str) -> Result<[u8; 32]> {
    if decimal.is_empty()
        || decimal.len() > 78
        || !decimal.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(PaymentError::Amount);
    }
    let mut word = [0_u8; 32];
    for digit in decimal.bytes() {
        let mut carry = u16::from(digit - b'0');
        for byte in word.iter_mut().rev() {
            let product = u16::from(*byte) * 10 + carry;
            *byte = product.to_le_bytes()[0];
            carry = product >> 8;
        }
        if carry != 0 {
            return Err(PaymentError::Amount);
        }
    }
    Ok(word)
}

fn word_quantity(word: &[u8; 32]) -> String {
    let encoded = hex::encode(word);
    let quantity = encoded.trim_start_matches('0');
    format!("0x{}", if quantity.is_empty() { "0" } else { quantity })
}

fn hex_quantity_decimal(value: &str) -> Result<String> {
    let raw = value.strip_prefix("0x").ok_or(PaymentError::Rpc)?;
    if raw.is_empty() || raw.len() > 64 || !raw.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(PaymentError::Rpc);
    }
    let mut digits = vec![0_u16];
    for byte in raw.bytes() {
        let mut carry = u16::from(match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            _ => byte - b'A' + 10,
        });
        for digit in &mut digits {
            let product = *digit * 16 + carry;
            *digit = product % 10;
            carry = product / 10;
        }
        while carry > 0 {
            digits.push(carry % 10);
            carry /= 10;
        }
    }
    Ok(digits
        .iter()
        .rev()
        .map(|digit| char::from(b'0' + u8::try_from(*digit).unwrap_or(0)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = "0x7e5f4552091a69125d5dfcb7b8c2659029395bdf";
    const FACTORY: &str = "0x1111111111111111111111111111111111111111";

    #[test]
    fn amounts_preserve_256_bits_and_reject_overflow() {
        let maximum =
            "115792089237316195423570985008687907853269984665640564039457584007913129639935";
        assert_eq!(decimal_word(maximum).unwrap(), [255; 32]);
        assert_eq!(
            hex_quantity_decimal(&format!("0x{}", "ff".repeat(32))).unwrap(),
            maximum
        );
        assert!(
            decimal_word(
                "115792089237316195423570985008687907853269984665640564039457584007913129639936"
            )
            .is_err()
        );
        assert_eq!(
            word_quantity(&decimal_word("1000000000000000000").unwrap()),
            "0xde0b6b3a7640000"
        );
        for invalid in ["-1", "1.5", "1e18", "", " 1"] {
            assert!(decimal_word(invalid).is_err());
        }
    }

    #[tokio::test]
    async fn rpc_rejects_wrong_chain_runtime_and_attestor() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{body_partial_json, method},
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method":"eth_chainId"})))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"jsonrpc":"2.0","id":1,"result":"0x14a34"})),
            )
            .mount(&server)
            .await;
        let hash = hex::encode(keccak(&[1]));
        let wrong_chain = VaultClient::new(&server.uri(), 1, FACTORY, OWNER, &hash).unwrap();
        assert!(matches!(
            wrong_chain.validate().await,
            Err(PaymentError::DeploymentMismatch)
        ));
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method":"eth_getCode"})))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"jsonrpc":"2.0","id":1,"result":"0x02"})),
            )
            .mount(&server)
            .await;
        let wrong_code = VaultClient::new(&server.uri(), 84532, FACTORY, OWNER, &hash).unwrap();
        assert!(matches!(
            wrong_code.validate().await,
            Err(PaymentError::DeploymentMismatch)
        ));
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method":"eth_getCode"})))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"jsonrpc":"2.0","id":1,"result":"0x01"})),
            )
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({"method":"eth_call"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc":"2.0", "id":1,
                "result":format!("0x{}{}", "0".repeat(24), &FACTORY[2..]),
            })))
            .mount(&server)
            .await;
        assert!(matches!(
            wrong_code.validate().await,
            Err(PaymentError::DeploymentMismatch)
        ));
    }
}
