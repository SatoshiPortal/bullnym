//! The on-chain rail: one address per sale, derived from the owner's
//! public descriptor.
//!
//! The service watches the addresses in a watch-only wallet of its own on
//! bitcoind, never through a third-party API. Each sale address is imported
//! when it is first handed out. A fresh address has no history, so the
//! import needs no rescan.

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, bail};
use bdk_wallet::miniscript::ForEachKey;
use bdk_wallet::miniscript::descriptor::{Descriptor, DescriptorPublicKey, Wildcard};
use bitcoin::{Address, Amount, Network};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::stages::{Mempool, TxObservation};


/// A public, single-path, ranged descriptor such as
/// `tr([fp/86'/0'/1']xpub…/0/*)`.
#[derive(Debug, Clone)]
pub struct StoreDescriptor(Descriptor<DescriptorPublicKey>);

impl StoreDescriptor {
	pub fn parse(s: &str) -> anyhow::Result<StoreDescriptor> {
		let desc = Descriptor::<DescriptorPublicKey>::from_str(s)
			.context("not a valid public descriptor")?;
		if !desc.has_wildcard() {
			bail!("the descriptor must be ranged (end in /*)");
		}
		if desc.is_multipath() {
			bail!("the descriptor must have a single derivation path");
		}
		// A hardened wildcard needs a private key to derive.
		let hardened = desc.for_any_key(|k| match k {
			DescriptorPublicKey::XPub(x) => x.wildcard == Wildcard::Hardened,
			DescriptorPublicKey::MultiXPub(x) => x.wildcard == Wildcard::Hardened,
			DescriptorPublicKey::Single(_) => false,
		});
		if hardened {
			bail!("the descriptor must derive without private keys (no hardened wildcard)");
		}
		Ok(StoreDescriptor(desc))
	}

	pub fn address(&self, index: u32, network: Network) -> anyhow::Result<Address> {
		let derived = self.0.at_derivation_index(index)
			.context("index out of range for the descriptor")?;
		derived.address(network).context("the descriptor has no address form")
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BitcoindConfig {
	/// The RPC URL, for example `http://127.0.0.1:8332`.
	pub url: String,
	/// The RPC cookie file. Used when `rpc_user` is not set.
	pub cookie: Option<PathBuf>,
	pub rpc_user: Option<String>,
	#[serde(skip_serializing)]
	pub rpc_password: Option<String>,
	/// The watch-only wallet the service creates and uses.
	#[serde(default = "default_wallet")]
	pub wallet: String,
}

fn default_wallet() -> String { "bullnym".into() }

pub struct Chain {
	cfg: BitcoindConfig,
	http: reqwest::Client,
}

impl Chain {
	/// Connects, and creates or loads the watch-only wallet.
	pub async fn connect(cfg: BitcoindConfig) -> anyhow::Result<Chain> {
		let http = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
		let chain = Chain { cfg, http };
		chain.open_wallet().await?;
		Ok(chain)
	}

	async fn open_wallet(&self) -> anyhow::Result<()> {
		let name = self.cfg.wallet.clone();
		let loaded = self.call(None, "listwallets", json!([])).await?;
		if loaded.as_array().is_some_and(|w| w.iter().any(|n| n.as_str() == Some(&name))) {
			return Ok(());
		}
		if self.call(None, "loadwallet", json!([name])).await.is_ok() {
			return Ok(());
		}
		// A blank descriptor wallet without private keys.
		self.call(None, "createwallet", json!([name, true, true, "", false, true, true]))
			.await.context("failed to create the watch-only wallet")?;
		Ok(())
	}

	/// Starts watching an address. Its history starts now.
	pub async fn watch(&self, address: &Address) -> anyhow::Result<()> {
		let desc = format!("addr({})", address);
		let info = self.call(None, "getdescriptorinfo", json!([desc])).await?;
		let desc = info.get("descriptor").and_then(|d| d.as_str())
			.context("getdescriptorinfo returned no descriptor")?;
		let res = self.call(Some(&self.cfg.wallet), "importdescriptors", json!([[{
			"desc": desc,
			"timestamp": "now",
			"label": "",
		}]])).await?;
		let ok = res.get(0).and_then(|r| r.get("success")).and_then(|s| s.as_bool());
		if ok != Some(true) {
			bail!("bitcoind refused to watch {}: {}", address, res);
		}
		Ok(())
	}

	/// The transactions paying each address of the wallet, with unconfirmed
	/// ones. A transaction that conflicts with a confirmed one is left out
	/// by bitcoind; the watcher keeps the ones it saw.
	pub async fn received_by_address(&self) -> anyhow::Result<HashMap<String, Vec<String>>> {
		let list = self.call(Some(&self.cfg.wallet), "listreceivedbyaddress", json!([0, false, true])).await?;
		let mut out = HashMap::new();
		for entry in list.as_array().context("listreceivedbyaddress returned no list")? {
			let Some(address) = entry.get("address").and_then(|a| a.as_str()) else { continue };
			let txids = entry.get("txids").and_then(|t| t.as_array())
				.map(|t| t.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>())
				.unwrap_or_default();
			out.insert(address.to_string(), txids);
		}
		Ok(out)
	}

	/// Whether the wallet saw any payment, confirmed or not, to `address`.
	pub async fn has_received(&self, address: &Address) -> anyhow::Result<bool> {
		let list = self.call(
			Some(&self.cfg.wallet), "listreceivedbyaddress", json!([0, true, true, address.to_string()]),
		).await?;
		let Some(entry) = list.as_array().and_then(|l| l.first()) else { return Ok(false) };
		let amount = entry.get("amount").map(btc_value).transpose()?.unwrap_or(Amount::ZERO);
		let txids = entry.get("txids").and_then(|t| t.as_array()).map_or(0, |t| t.len());
		Ok(amount > Amount::ZERO || txids > 0)
	}

	/// What bitcoind says about transaction `txid` paying `address`.
	/// `address_txids` are every transaction known to pay the address: a
	/// conflict among them still pays the sale.
	pub async fn observe_tx(
		&self,
		txid: &str,
		address: &Address,
		address_txids: &[String],
	) -> anyhow::Result<TxObservation> {
		let tx = self.call(Some(&self.cfg.wallet), "gettransaction", json!([txid, true, true])).await?;
		let confirmations = tx.get("confirmations").and_then(|c| c.as_i64())
			.context("gettransaction returned no confirmations")?;
		let addr = address.to_string();
		let mut amount = Amount::ZERO;
		for out in tx.pointer("/decoded/vout").and_then(|v| v.as_array()).context("gettransaction returned no outputs")? {
			if out.pointer("/scriptPubKey/address").and_then(|a| a.as_str()) == Some(addr.as_str()) {
				amount += btc_value(out.get("value").context("output without value")?)?;
			}
		}
		let conflicts = ["walletconflicts", "mempoolconflicts"].iter()
			.filter_map(|k| tx.get(*k).and_then(|c| c.as_array()))
			.flatten()
			.filter_map(|c| c.as_str())
			.collect::<Vec<_>>();
		let foreign_conflicts = conflicts.iter().any(|c| !address_txids.iter().any(|t| t == c));
		let mempool = if confirmations == 0 { self.mempool_entry(txid).await } else { Mempool::Unknown };
		Ok(TxObservation { txid: txid.to_string(), amount, confirmations, mempool, foreign_conflicts })
	}

	/// The mempool entry of `txid`. Only bitcoind's own answer that the
	/// transaction is not in its mempool counts as [Mempool::NotIn]; any
	/// other failure is [Mempool::Unknown].
	async fn mempool_entry(&self, txid: &str) -> Mempool {
		match self.call_rpc(None, "getmempoolentry", json!([txid])).await {
			Ok(Ok(entry)) => {
				let fee = entry.pointer("/fees/base").map(btc_value).and_then(Result::ok);
				let vsize = entry.get("vsize").and_then(|v| v.as_u64()).filter(|v| *v > 0);
				let ancestors = entry.get("ancestorcount").and_then(|a| a.as_u64()).unwrap_or(1);
				match (fee, vsize) {
					(Some(fee), Some(vsize)) => Mempool::In {
						fee_rate_sat_vb: fee.to_sat() as f64 / vsize as f64,
						unconfirmed_parents: ancestors > 1,
					},
					_ => Mempool::Unknown,
				}
			},
			Ok(Err(RPC_INVALID_ADDRESS_OR_KEY)) => Mempool::NotIn,
			Ok(Err(_)) | Err(_) => Mempool::Unknown,
		}
	}

	/// bitcoind's fee rate estimate for a confirmation within `blocks`, in
	/// sat/vB, if it has one.
	pub async fn fee_estimate(&self, blocks: u16) -> Option<f64> {
		let res = self.call(None, "estimatesmartfee", json!([blocks])).await.ok()?;
		let btc_per_kvb = res.get("feerate").and_then(|f| f.as_f64())?;
		Some(btc_per_kvb * 100_000.0)
	}

	/// Whether bitcoind answers and has the wallet loaded.
	pub async fn ping(&self) -> anyhow::Result<()> {
		self.call(Some(&self.cfg.wallet), "getwalletinfo", json!([])).await?;
		Ok(())
	}

	async fn call(&self, wallet: Option<&str>, method: &str, params: Value) -> anyhow::Result<Value> {
		match self.call_rpc(wallet, method, params).await? {
			Ok(v) => Ok(v),
			Err(code) => bail!("bitcoind {} failed with error code {}", method, code),
		}
	}

	/// Calls bitcoind. The outer error is a failure to get an answer; the
	/// inner one is bitcoind's own error code.
	async fn call_rpc(
		&self,
		wallet: Option<&str>,
		method: &str,
		params: Value,
	) -> anyhow::Result<Result<Value, i64>> {
		let url = match wallet {
			Some(w) => format!("{}/wallet/{}", self.cfg.url.trim_end_matches('/'), w),
			None => self.cfg.url.clone(),
		};
		let (user, pass) = self.credentials()?;
		let body = json!({ "jsonrpc": "1.0", "id": "bullnym", "method": method, "params": params });
		let resp = self.http.post(url).basic_auth(user, Some(pass)).json(&body).send().await
			.with_context(|| format!("bitcoind {} request failed", method))?;
		let status = resp.status();
		let resp: Value = resp.json().await
			.with_context(|| format!("bitcoind {} returned {} without JSON", method, status))?;
		if let Some(err) = resp.get("error").filter(|e| !e.is_null()) {
			let code = err.get("code").and_then(|c| c.as_i64())
				.with_context(|| format!("bitcoind {} failed: {}", method, err))?;
			return Ok(Err(code));
		}
		Ok(Ok(resp.get("result").cloned().unwrap_or(Value::Null)))
	}

	fn credentials(&self) -> anyhow::Result<(String, String)> {
		if let Some(user) = &self.cfg.rpc_user {
			return Ok((user.clone(), self.cfg.rpc_password.clone().unwrap_or_default()));
		}
		let path = self.cfg.cookie.as_ref().context("bitcoind needs rpc_user or a cookie file")?;
		// Read on every call: bitcoind writes a new cookie when it restarts.
		let cookie = std::fs::read_to_string(path)
			.with_context(|| format!("failed to read the bitcoind cookie {}", path.display()))?;
		let (user, pass) = cookie.trim().split_once(':').context("the bitcoind cookie is malformed")?;
		Ok((user.to_string(), pass.to_string()))
	}
}

/// bitcoind's error code for a transaction that is not in its mempool.
const RPC_INVALID_ADDRESS_OR_KEY: i64 = -5;

fn btc_value(v: &Value) -> anyhow::Result<Amount> {
	// An amount has 8 decimals, well within the precision of an f64, and
	// rounding to the nearest sat gives it back exactly. The text form is
	// not usable: small amounts print in exponent notation.
	let btc = v.as_f64().with_context(|| format!("bitcoind returned an invalid amount: {}", v))?;
	Amount::from_btc(btc).with_context(|| format!("bitcoind returned an invalid amount: {}", v))
}

#[cfg(test)]
mod test {
	use super::*;

	use bitcoin::bip32::{Xpriv, Xpub};
	use bitcoin::secp256k1::Secp256k1;

	#[test]
	fn parses_descriptors() {
		let xpriv = Xpriv::new_master(Network::Regtest, &[7u8; 32]).unwrap();
		let xpub = Xpub::from_priv(&Secp256k1::new(), &xpriv).to_string();
		let desc = StoreDescriptor::parse(&format!("tr({}/0/*)", xpub)).unwrap();
		let a0 = desc.address(0, Network::Regtest).unwrap();
		let a1 = desc.address(1, Network::Regtest).unwrap();
		assert_ne!(a0, a1);
		assert_eq!(a0, desc.address(0, Network::Regtest).unwrap());

		assert!(StoreDescriptor::parse(&format!("tr({}/0/0)", xpub)).is_err(), "not ranged");
		assert!(StoreDescriptor::parse(&format!("tr({}/<0;1>/*)", xpub)).is_err(), "multipath");
		assert!(StoreDescriptor::parse(&format!("tr({}/0/*')", xpub)).is_err(), "hardened");
		assert!(StoreDescriptor::parse("tr(nonsense)").is_err());
	}

	#[test]
	fn reads_btc_values() {
		assert_eq!(btc_value(&json!(0.00035)).unwrap(), Amount::from_sat(35_000));
		assert_eq!(btc_value(&json!(0)).unwrap(), Amount::ZERO);
		assert_eq!(btc_value(&json!(21.12345678)).unwrap(), Amount::from_sat(2_112_345_678));
		// Small amounts, which serialize in exponent notation.
		assert_eq!(btc_value(&json!(0.00001)).unwrap(), Amount::from_sat(1_000));
		assert_eq!(btc_value(&json!(0.00000001)).unwrap(), Amount::from_sat(1));
		assert!(btc_value(&json!("x")).is_err());
	}
}
