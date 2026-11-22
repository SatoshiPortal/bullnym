use std::path::PathBuf;
use std::process;

use anyhow::Context;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use bullnym::config::Config;


#[derive(Parser)]
#[command(name = "bullnym", version, about = "Lightning Address and store service for Ark wallets")]
struct Cli {
	/// The config file. `BULLNYM__` environment variables override it.
	#[arg(long, short = 'C', global = true, env = "BULLNYM_CONFIG")]
	config: Option<PathBuf>,
	#[command(subcommand)]
	command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
	/// Runs the service.
	Start,
	/// Checks the config and prints it.
	CheckConfig,
}

#[tokio::main]
async fn main() {
	tracing_subscriber::fmt()
		.with_env_filter(EnvFilter::try_from_default_env()
			.unwrap_or_else(|_| EnvFilter::new("info,tokio_postgres=warn")))
		.init();
	if let Err(e) = inner_main().await {
		eprintln!("An error occurred: {}", e);
		eprintln!();
		eprintln!("{:?}", e);
		process::exit(1);
	}
}

async fn inner_main() -> anyhow::Result<()> {
	let cli = Cli::parse();
	let cfg = Config::load(cli.config.as_deref())?;
	match cli.command {
		Command::CheckConfig => {
			println!("{}", serde_json::to_string_pretty(&cfg)?);
			Ok(())
		},
		Command::Start => start(cfg).await,
	}
}

async fn start(cfg: Config) -> anyhow::Result<()> {
	let listen = cfg.http.listen;
	let app = bullnym::App::new(cfg).await?;
	bullnym::spawn_background(app.clone());

	let listener = tokio::net::TcpListener::bind(listen).await
		.with_context(|| format!("failed to listen on {}", listen))?;
	tracing::info!("bullnym listening on {}", listen);
	tokio::select! {
		res = bullnym::serve(listener, app) => res?,
		_ = tokio::signal::ctrl_c() => tracing::info!("shutting down"),
	}
	Ok(())
}
