mod assets;
mod command_channels;
mod command_indexer;
mod command_server;
mod command_worker;
mod config;
mod http;
mod provider_config;
mod shutdown;
mod store;
mod views;
mod watch_url;
mod web3;
mod webhooks;

use anyhow::Result;
use clap::Parser;
use config::{Cli, Command};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let service_name = match &cli.command {
        Command::Server(_) => "rill3-server",
        Command::Worker(_) => "rill3-worker",
        Command::Indexer(_) => "rill3-indexer",
        Command::SeedDemo(_) => "rill3-seed",
        Command::Channels(_) => "rill3-channels",
    };
    if !matches!(&cli.command, Command::Channels(_)) {
        rill3_telemetry::init(service_name)
            .map_err(|error| anyhow::anyhow!("failed to initialize telemetry: {error}"))?;
    }

    match cli.command {
        Command::Server(arguments) => command_server::run(*arguments).await,
        Command::Worker(arguments) => command_worker::run(*arguments).await,
        Command::Indexer(arguments) => command_indexer::run(&arguments),
        Command::SeedDemo(arguments) => command_server::seed_demo(arguments).await,
        Command::Channels(arguments) => command_channels::run(arguments).await,
    }
}
