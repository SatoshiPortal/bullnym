use std::num::NonZeroU32;
use std::path::Path;

use anyhow::{Context, ensure};
use clap::{Args, Subcommand};
use serde_json::json;
use tokio_postgres::{Client, NoTls};

use bullnym::{claim_codes, config::Config, nym};

#[derive(Args)]
pub struct GrantOptions {
	/// Operator note included in the listing.
	#[arg(long)]
	label: Option<String>,
	/// Number of days until the code expires.
	#[arg(long)]
	expires_days: Option<NonZeroU32>,
}

#[derive(Subcommand)]
pub enum Action {
	/// Issue single-use codes for any available name.
	Mint {
		#[arg(long, default_value = "1")]
		count: NonZeroU32,
		#[command(flatten)]
		options: GrantOptions,
	},
	/// Reserve an available name for the holder of one code.
	Lock {
		#[arg(long)]
		nym: String,
		#[command(flatten)]
		options: GrantOptions,
	},
	/// List grant metadata, one JSON object per line.
	List {
		#[arg(long, group = "filter")]
		unused: bool,
		#[arg(long, group = "filter")]
		used: bool,
		#[arg(long, group = "filter")]
		locked: bool,
	},
	/// Revoke an unused grant by its ID.
	Revoke { id: String },
}

async fn issue(client: &mut Client, count: u32, name: Option<&str>, options: GrantOptions) -> anyhow::Result<()> {
	let tx = client.transaction().await?;
	if let Some(name) = name {
		claim_codes::lock_registration(&tx).await?;
		ensure!(tx.query_opt("SELECT 1 FROM nym WHERE name = $1", &[&name]).await?.is_none(),
			"this nym is already registered");
		// An expired reservation can be replaced. Keep its original grant for audit.
		tx.execute("UPDATE claim_code SET revoked_at = clock_timestamp()
			WHERE locked_nym = $1 AND used_at IS NULL AND revoked_at IS NULL
				AND expires_at <= clock_timestamp()", &[&name]).await?;
	}
	let days = options.expires_days.map(|n| i64::from(n.get()));
	let mut grants = Vec::new();
	for _ in 0..count {
		let code = claim_codes::generate();
		let hash = claim_codes::digest(&code);
		let row = tx.query_opt("
			INSERT INTO claim_code (code_hash, locked_nym, label, expires_at)
			VALUES ($1, $2, $3, clock_timestamp() + $4::BIGINT * interval '1 day')
			ON CONFLICT DO NOTHING RETURNING id::TEXT
		", &[&hash, &name, &options.label, &days]).await?
			.context("a grant or name lock already exists")?;
		grants.push((row.get::<_, String>(0), code));
	}
	tx.commit().await?;
	for (id, code) in grants { println!("{}", json!({ "id": id, "claim_code": code })); }
	Ok(())
}

pub async fn run(action: Action, config_path: Option<&Path>) -> anyhow::Result<()> {
	let url = std::env::var("DATABASE_URL").context("set DATABASE_URL for the Bullnym database")?;
	let (mut client, connection) = tokio_postgres::connect(&url, NoTls).await
		.context("connecting to the Bullnym database")?;
	tokio::spawn(connection);
	match action {
		Action::Mint { count, options } => issue(&mut client, count.get(), None, options).await?,
		Action::Lock { nym, options } => {
			nym::check(&nym, &Config::reserved_nyms(config_path)?)?;
			issue(&mut client, 1, Some(&nym), options).await?;
		},
		Action::List { unused, used, locked } => {
			let rows = client.query("
				SELECT row_to_json(grant_info)::TEXT FROM (
					SELECT c.id, c.locked_nym, c.label, c.created_at, c.expires_at, c.revoked_at,
						c.used_at, n.name AS used_nym, n.ark_address AS used_by_ark_address
					FROM claim_code c LEFT JOIN nym n ON n.id = c.used_nym_id
					WHERE (NOT $1 OR (c.used_at IS NULL AND c.revoked_at IS NULL
						AND (c.expires_at IS NULL OR c.expires_at > statement_timestamp())))
						AND (NOT $2 OR c.used_at IS NOT NULL) AND (NOT $3 OR c.locked_nym IS NOT NULL)
					ORDER BY c.created_at, c.id
				) grant_info", &[&unused, &used, &locked]).await?;
			for row in rows { println!("{}", row.get::<_, String>(0)); }
		},
		Action::Revoke { id } => {
			let tx = client.transaction().await?;
			claim_codes::lock_registration(&tx).await?;
			let changed = tx.execute("UPDATE claim_code SET revoked_at = COALESCE(revoked_at, statement_timestamp())
				WHERE id = $1::TEXT::UUID AND used_at IS NULL", &[&id]).await?;
			ensure!(changed == 1, "no unused grant with that ID");
			tx.commit().await?;
		},
	}
	Ok(())
}
