use k256::ecdsa::{RecoveryId, Signature, SigningKey, VerifyingKey};
use k256::elliptic_curve::zeroize::Zeroizing;
use sha3::{Digest, Keccak256};

use crate::{PaymentError, Result};

pub(crate) const ZERO: &str = "0x0000000000000000000000000000000000000000";

/// Canonical lower-case address. Zero is never an eligible owner.
///
/// # Errors
/// Rejects malformed, non-hexadecimal, and zero addresses.
pub fn normalize_address(value: &str) -> Result<String> {
    let raw = value.strip_prefix("0x").ok_or(PaymentError::Address)?;
    if raw.len() != 40 || !raw.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(PaymentError::Address);
    }
    let normalized = format!("0x{}", raw.to_ascii_lowercase());
    if normalized == ZERO {
        return Err(PaymentError::Address);
    }
    Ok(normalized)
}

/// Versioned, platform-scoped key. The adapter must canonicalize the provider ID first.
#[must_use]
pub fn channel_key(provider: &str, id: &str) -> [u8; 32] {
    let mut hash = Keccak256::new();
    hash.update(b"RILL3_CHANNEL_V1\0");
    hash.update(provider.as_bytes());
    hash.update([0]);
    hash.update(id.as_bytes());
    hash.finalize().into()
}

/// Verify the exact EIP-191 challenge. The caller enforces purpose, expiry, and nonce consumption.
///
/// # Errors
/// Rejects malformed/malleable signatures and signatures by another wallet.
pub fn verify_wallet_signature(
    message: &str,
    signature: &str,
    expected_address: &str,
) -> Result<()> {
    let actual = recover(
        personal_digest(message.as_bytes()),
        &decode_signature(signature)?,
    )?;
    if actual != normalize_address(expected_address)? {
        return Err(PaymentError::Signature);
    }
    Ok(())
}

/// First-owner attestation key; never signs fund transfers. Intentionally not Debug/Serialize.
#[derive(Clone)]
pub struct AttestationSigner {
    key: SigningKey,
    address: String,
}

impl AttestationSigner {
    /// # Errors
    /// Rejects malformed or invalid secp256k1 private scalars.
    pub fn new(private_key: &str) -> Result<Self> {
        let encoded = private_key.strip_prefix("0x").unwrap_or(private_key);
        if encoded.len() != 64 {
            return Err(PaymentError::SigningKey);
        }
        let bytes = Zeroizing::new(hex::decode(encoded).map_err(|_| PaymentError::SigningKey)?);
        let key = SigningKey::from_slice(&bytes).map_err(|_| PaymentError::SigningKey)?;
        let address = verifying_address(key.verifying_key());
        Ok(Self { key, address })
    }

    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// # Errors
    /// Rejects invalid factory/owner addresses or a signing failure.
    pub fn sign_claim(
        &self,
        chain_id: u64,
        factory: &str,
        key: [u8; 32],
        owner: &str,
        deadline: u64,
    ) -> Result<String> {
        let digest = claim_digest(chain_id, factory, key, owner, deadline)?;
        let (signature, recovery) = self
            .key
            .sign_prehash_recoverable(&digest)
            .map_err(|_| PaymentError::Signature)?;
        let mut bytes = signature.to_bytes().to_vec();
        bytes.push(recovery.to_byte() + 27);
        Ok(format!("0x{}", hex::encode(bytes)))
    }
}

pub(crate) fn claim_digest(
    chain_id: u64,
    factory: &str,
    key: [u8; 32],
    owner: &str,
    deadline: u64,
) -> Result<[u8; 32]> {
    let mut words = Vec::with_capacity(160);
    words.extend(u64_word(chain_id));
    words.extend(address_word(factory)?);
    words.extend(key);
    words.extend(address_word(owner)?);
    words.extend(u64_word(deadline));
    Ok(personal_digest(&keccak(&words)))
}

fn personal_digest(message: &[u8]) -> [u8; 32] {
    let mut hash = Keccak256::new();
    hash.update(format!("\x19Ethereum Signed Message:\n{}", message.len()).as_bytes());
    hash.update(message);
    hash.finalize().into()
}

pub(crate) fn keccak(bytes: &[u8]) -> [u8; 32] {
    Keccak256::digest(bytes).into()
}

fn verifying_address(key: &VerifyingKey) -> String {
    let uncompressed = key.to_encoded_point(false);
    let digest = keccak(&uncompressed.as_bytes()[1..]);
    format!("0x{}", hex::encode(&digest[12..]))
}

pub(crate) fn decode_signature(value: &str) -> Result<[u8; 65]> {
    if value.len() != 132 {
        return Err(PaymentError::Signature);
    }
    let bytes = hex::decode(value.strip_prefix("0x").ok_or(PaymentError::Signature)?)
        .map_err(|_| PaymentError::Signature)?;
    let mut signature: [u8; 65] = bytes.try_into().map_err(|_| PaymentError::Signature)?;
    if signature[64] <= 1 {
        signature[64] += 27;
    }
    if !(27..=28).contains(&signature[64]) {
        return Err(PaymentError::Signature);
    }
    Ok(signature)
}

fn recover(digest: [u8; 32], bytes: &[u8; 65]) -> Result<String> {
    let signature = Signature::from_slice(&bytes[..64]).map_err(|_| PaymentError::Signature)?;
    if signature.normalize_s().is_some() {
        return Err(PaymentError::Signature);
    }
    let recovery = RecoveryId::from_byte(bytes[64] - 27).ok_or(PaymentError::Signature)?;
    let key = VerifyingKey::recover_from_prehash(&digest, &signature, recovery)
        .map_err(|_| PaymentError::Signature)?;
    Ok(verifying_address(&key))
}

pub(crate) fn address_word(address: &str) -> Result<[u8; 32]> {
    let normalized = normalize_address(address)?;
    let bytes = hex::decode(&normalized[2..]).map_err(|_| PaymentError::Address)?;
    let mut word = [0; 32];
    word[12..].copy_from_slice(&bytes);
    Ok(word)
}

pub(crate) fn u64_word(value: u64) -> [u8; 32] {
    let mut word = [0; 32];
    word[24..].copy_from_slice(&value.to_be_bytes());
    word
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIVATE: &str = "0000000000000000000000000000000000000000000000000000000000000001";
    const OWNER: &str = "0x7e5f4552091a69125d5dfcb7b8c2659029395bdf";
    const FACTORY: &str = "0x1111111111111111111111111111111111111111";

    #[test]
    fn accepts_independent_cast_personal_sign_fixture_with_utf8() {
        // Generated with Foundry cast wallet sign, using public test scalar 1.
        let message = "RILL3 방송인 인증\nOrigin: https://rill3.example\nChain ID: 84532\nNonce: 1a2b3c4d5e6f";
        let signature = "0x04cbbf667e8d8dd0dec25c394e9d3798b5528717094e4e360667a5565223035a1e482e2810efa5d5030a06cee13ee9334d42fc7b8efd75e5fc88b92e77abcaa01b";
        verify_wallet_signature(message, signature, OWNER).unwrap();
    }

    #[test]
    fn wallet_signature_is_bound_to_exact_challenge_and_wallet() {
        let signer = AttestationSigner::new(PRIVATE).unwrap();
        assert_eq!(signer.address(), OWNER);
        let message = "rill3.example\nChain ID: 84532\nNonce: unpredictable\nPurpose: claim";
        let (sig, recovery) = signer
            .key
            .sign_prehash_recoverable(&personal_digest(message.as_bytes()))
            .unwrap();
        let mut bytes = sig.to_bytes().to_vec();
        bytes.push(recovery.to_byte() + 27);
        let sig = format!("0x{}", hex::encode(bytes));
        assert!(verify_wallet_signature(message, &sig, OWNER).is_ok());
        assert!(verify_wallet_signature("another domain", &sig, OWNER).is_err());
        assert!(verify_wallet_signature(message, &sig, FACTORY).is_err());
        assert!(verify_wallet_signature(message, "0x00", OWNER).is_err());
    }

    #[test]
    fn attestation_binds_every_authority_field() {
        let signer = AttestationSigner::new(PRIVATE).unwrap();
        let key = channel_key("twitch", "123");
        let sig =
            decode_signature(&signer.sign_claim(84532, FACTORY, key, OWNER, 200).unwrap()).unwrap();
        assert_eq!(
            recover(claim_digest(84532, FACTORY, key, OWNER, 200).unwrap(), &sig).unwrap(),
            OWNER
        );
        for digest in [
            claim_digest(8453, FACTORY, key, OWNER, 200).unwrap(),
            claim_digest(84532, OWNER, key, OWNER, 200).unwrap(),
            claim_digest(84532, FACTORY, [0; 32], OWNER, 200).unwrap(),
            claim_digest(84532, FACTORY, key, FACTORY, 200).unwrap(),
            claim_digest(84532, FACTORY, key, OWNER, 201).unwrap(),
        ] {
            assert_ne!(recover(digest, &sig).unwrap(), OWNER);
        }
    }

    #[test]
    fn keys_and_addresses_are_platform_scoped_and_validated() {
        assert_ne!(channel_key("twitch", "123"), channel_key("youtube", "123"));
        assert_ne!(channel_key("ab", "c"), channel_key("a", "bc"));
        assert_eq!(
            normalize_address("0x7E5F4552091A69125D5DFCB7B8C2659029395BDF").unwrap(),
            OWNER
        );
        assert!(normalize_address(ZERO).is_err());
        assert!(normalize_address("0x123").is_err());
    }
}
