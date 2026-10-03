use std::{
    collections::VecDeque,
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use clap::Args;
use rill3_auth::{OAuthClient, OAuthProviderConfig};
use rill3_db::Database;
use rill3_domain::ProviderKind;
use rill3_payments::{AttestationSigner, VaultClient};
use url::Url;

#[derive(Clone, Default, Args)]
pub(crate) struct Web3Args {
    /// All Web3 settings are required together; no receive address is invented.
    #[arg(long, env = "RILL3_CHAIN_ID", default_value_t = 0)]
    pub(crate) chain_id: u64,
    #[arg(long, env = "RILL3_CHAIN_NAME")]
    pub(crate) chain_name: Option<String>,
    #[arg(long, env = "RILL3_RPC_URL", hide_env_values = true)]
    pub(crate) rpc_url: Option<String>,
    #[arg(long, env = "RILL3_VAULT_FACTORY")]
    pub(crate) factory: Option<String>,
    #[arg(long, env = "RILL3_ATTESTOR_PRIVATE_KEY", hide_env_values = true)]
    pub(crate) attestor_key: Option<String>,
    #[arg(long, env = "RILL3_FACTORY_CODE_HASH")]
    pub(crate) factory_code_hash: Option<String>,
    #[arg(long, env = "YOUTUBE_CLIENT_ID", hide_env_values = true)]
    pub(crate) youtube_client_id: Option<String>,
    #[arg(long, env = "YOUTUBE_CLIENT_SECRET", hide_env_values = true)]
    pub(crate) youtube_client_secret: Option<String>,
}

impl Web3Args {
    pub(crate) fn validate(&self) -> Result<()> {
        let values = [
            self.rpc_url.as_deref(),
            self.factory.as_deref(),
            self.attestor_key.as_deref(),
            self.chain_name.as_deref(),
            self.factory_code_hash.as_deref(),
        ];
        let enabled = self.chain_id != 0 || values.iter().any(|value| nonempty(*value).is_some());
        if enabled && !matches!(self.chain_id, 1 | 11_155_111 | 8_453 | 84_532 | 31_337) {
            bail!(
                "native ETH vaults currently support Ethereum, Sepolia, Base, Base Sepolia, and local Anvil only"
            );
        }
        if enabled
            && ((self.chain_id == 0 || self.chain_id > i64::MAX as u64)
                || values.iter().any(|value| nonempty(*value).is_none()))
        {
            bail!(
                "Web3 requires RILL3_CHAIN_ID, RILL3_CHAIN_NAME, RILL3_RPC_URL, RILL3_VAULT_FACTORY, RILL3_FACTORY_CODE_HASH, and RILL3_ATTESTOR_PRIVATE_KEY together"
            );
        }
        if self
            .chain_name
            .as_ref()
            .is_some_and(|name| name.len() > 100 || name.chars().any(char::is_control))
        {
            bail!("RILL3_CHAIN_NAME must be at most 100 bytes without control characters");
        }
        Ok(())
    }
}

pub(super) fn nonempty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

pub(crate) struct Payments {
    pub(super) client: VaultClient,
    pub(super) signer: AttestationSigner,
    pub(super) chain_id: u64,
    pub(super) chain_name: String,
    pub(super) factory: String,
}

pub(crate) struct Web3Service {
    pub(super) database: Database,
    pub(super) oauth: OAuthClient,
    pub(super) payments: Option<Payments>,
    pub(super) origin: String,
    pub(super) base_path: String,
    pub(super) secure: bool,
    limits: Mutex<[VecDeque<Instant>; 4]>,
}

impl Web3Service {
    pub(crate) async fn new(database: Database, args: &crate::config::ServerArgs) -> Result<Self> {
        let mut configs = Vec::new();
        for (provider, name, id, secret) in [
            (
                ProviderKind::Twitch,
                "twitch",
                args.twitch_client_id.as_deref(),
                args.twitch_client_secret.as_deref(),
            ),
            (
                ProviderKind::YouTube,
                "youtube",
                args.web3.youtube_client_id.as_deref(),
                args.web3.youtube_client_secret.as_deref(),
            ),
            (
                ProviderKind::Chzzk,
                "chzzk",
                args.chzzk_client_id.as_deref(),
                args.chzzk_client_secret.as_deref(),
            ),
        ] {
            if let (Some(id), Some(secret)) = (nonempty(id), nonempty(secret)) {
                configs.push(OAuthProviderConfig {
                    provider,
                    client_id: id.to_owned(),
                    client_secret: secret.to_owned().into(),
                    redirect_uri: Url::parse(&format!(
                        "{}{}{}",
                        args.public_origin.as_str().trim_end_matches('/'),
                        args.base_path,
                        format_args!("/auth/{name}/callback")
                    ))?,
                });
            }
        }
        let payments = if args.web3.chain_id != 0 {
            let chain_id = args.web3.chain_id;
            let signer = AttestationSigner::new(
                args.web3
                    .attestor_key
                    .as_deref()
                    .context("attestor key missing")?,
            )?;
            let factory = rill3_payments::normalize_address(
                args.web3.factory.as_deref().context("factory missing")?,
            )?;
            let client = VaultClient::new(
                args.web3.rpc_url.as_deref().context("RPC missing")?,
                chain_id,
                &factory,
                signer.address(),
                args.web3
                    .factory_code_hash
                    .as_deref()
                    .context("factory code hash missing")?,
            )?;
            client
                .validate()
                .await
                .context("validate Web3 chain and deployed factory")?;
            Some(Payments {
                client,
                signer,
                chain_id,
                factory,
                chain_name: args.web3.chain_name.clone().context("chain name missing")?,
            })
        } else {
            None
        };
        Ok(Self {
            database,
            oauth: OAuthClient::new(configs)?,
            payments,
            origin: args.public_origin.as_str().trim_end_matches('/').to_owned(),
            base_path: args.base_path.clone(),
            secure: args.public_origin.scheme() == "https",
            limits: Mutex::new(std::array::from_fn(|_| VecDeque::new())),
        })
    }

    /// Bounded per-process limits also protect anonymous registration and RPC reads.
    pub(super) fn allow(&self, bucket: usize, maximum: usize) -> bool {
        let Ok(mut limits) = self.limits.lock() else {
            return false;
        };
        let now = Instant::now();
        let entries = &mut limits[bucket];
        while entries
            .front()
            .is_some_and(|time| now.duration_since(*time) >= Duration::from_mins(1))
        {
            entries.pop_front();
        }
        if entries.len() >= maximum {
            return false;
        }
        entries.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web3_requires_complete_explicit_configuration() {
        assert!(Web3Args::default().validate().is_ok());
        assert!(
            Web3Args {
                chain_id: 1,
                ..Web3Args::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Web3Args {
                rpc_url: Some(String::new()),
                ..Web3Args::default()
            }
            .validate()
            .is_ok()
        );
    }
}
