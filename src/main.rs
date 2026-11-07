mod api;
mod cli;
mod commands;
mod config;
mod output;

use anyhow::Result;
use clap::{CommandFactory, Parser};
use cli::{Cli, Command};
use config::AppConfig;
use output::Output;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("错误: {error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    if let Command::Completions { shell } = &cli.command {
        let mut command = Cli::command();
        let mut buffer = Vec::new();
        clap_complete::generate(*shell, &mut command, "bilic", &mut buffer);
        use std::io::Write;
        let _ = std::io::stdout().write_all(&buffer);
        return Ok(());
    }

    let config = AppConfig::load(&cli)?;
    let output = Output::new(cli.json);
    commands::dispatch(cli.command, &config, output).await
}

fn init_tracing(verbose: u8) {
    let filter = match (verbose, std::env::var("RUST_LOG")) {
        (_, Ok(value)) if !value.trim().is_empty() => value,
        (0, _) => "warn".to_string(),
        (1, _) => "info".to_string(),
        _ => "debug".to_string(),
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}
