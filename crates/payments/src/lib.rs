#![doc = include_str!("../README.md")]

mod crypto;
mod rpc;

pub use crypto::{AttestationSigner, channel_key, normalize_address, verify_wallet_signature};
pub use rpc::{TransactionRequest, VaultClient, VaultState};

#[derive(Debug, thiserror::Error)]
pub enum PaymentError {
    #[error("invalid Ethereum address")]
    Address,
    #[error("invalid wallet signature")]
    Signature,
    #[error("invalid attestation signing key")]
    SigningKey,
    #[error("invalid native-token amount")]
    Amount,
    #[error("invalid web3 configuration")]
    Configuration,
    #[error("chain RPC unavailable or invalid response")]
    Rpc,
    #[error("configured chain, factory code, or attestor differs from the RPC")]
    DeploymentMismatch,
}

pub type Result<T> = std::result::Result<T, PaymentError>;
