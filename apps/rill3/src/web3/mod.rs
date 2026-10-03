mod config;
mod handlers;

pub(crate) use config::{Web3Args, Web3Service};
pub(crate) use handlers::{openapi, router};

#[cfg(test)]
mod evm_tests;
