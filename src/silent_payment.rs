//! Silent Payment addresses (BIP 352).
//!
//! The service only checks the form of the address the owner registers.
//! It never derives or watches Silent Payment outputs: the owner's wallet
//! scans for them.

use bitcoin::Network;
use bitcoin::bech32::primitives::decode::CheckedHrpstring;
use bitcoin::bech32::{Bech32m, Fe32, Fe32IterExt};
use bitcoin::secp256k1::PublicKey;


/// The length of a version 0 payload: the scan key and the spend key.
const V0_PAYLOAD_LEN: usize = 66;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SilentPaymentError {
	#[error("not a bech32m string")]
	Encoding,
	#[error("the address is for another network")]
	Network,
	#[error("only version 0 Silent Payment addresses are supported")]
	Version,
	#[error("the address does not hold two valid public keys")]
	Keys,
}

/// The human-readable part for `network`.
pub fn hrp(network: Network) -> &'static str {
	match network {
		Network::Bitcoin => "sp",
		_ => "tsp",
	}
}

/// Checks that `address` is a version 0 Silent Payment address for
/// `network`, with a valid scan key and spend key.
pub fn check(address: &str, network: Network) -> Result<(), SilentPaymentError> {
	let checked = CheckedHrpstring::new::<Bech32m>(address)
		.map_err(|_| SilentPaymentError::Encoding)?;
	if checked.hrp().to_lowercase() != hrp(network) {
		return Err(SilentPaymentError::Network);
	}
	let mut fes = checked.data_part_ascii_no_checksum().iter()
		.map(|c| Fe32::from_char(*c as char).map_err(|_| SilentPaymentError::Encoding));
	let version = fes.next().ok_or(SilentPaymentError::Encoding)??;
	if version != Fe32::Q {
		return Err(SilentPaymentError::Version);
	}
	let fes = fes.collect::<Result<Vec<Fe32>, _>>()?;
	let payload = fes.into_iter().fes_to_bytes().collect::<Vec<u8>>();
	if payload.len() != V0_PAYLOAD_LEN {
		return Err(SilentPaymentError::Keys);
	}
	PublicKey::from_slice(&payload[..33]).map_err(|_| SilentPaymentError::Keys)?;
	PublicKey::from_slice(&payload[33..]).map_err(|_| SilentPaymentError::Keys)?;
	Ok(())
}

#[cfg(test)]
mod test {
	use super::*;

	use bitcoin::bech32::{ByteIterExt, Hrp};
	use bitcoin::secp256k1::{Secp256k1, SecretKey};

	fn encode(hrp_str: &str, version: Fe32, payload: &[u8]) -> String {
		let hrp = Hrp::parse(hrp_str).unwrap();
		payload.iter().copied().bytes_to_fes()
			.with_checksum::<Bech32m>(&hrp)
			.with_witness_version(version)
			.chars()
			.collect()
	}

	fn keys() -> Vec<u8> {
		let secp = Secp256k1::new();
		let scan = SecretKey::from_slice(&[1u8; 32]).unwrap().public_key(&secp);
		let spend = SecretKey::from_slice(&[2u8; 32]).unwrap().public_key(&secp);
		[scan.serialize(), spend.serialize()].concat()
	}

	#[test]
	fn bip352_vector() {
		// The address of the first sending test vector of BIP 352.
		let addr = "sp1qqgste7k9hx0qftg6qmwlkqtwuy6cycyavzmzj85c6qdfhjdpdjtdgqjuexzk6murw56suy3e0rd2cgqvycxttddwsvgxe2usfpxumr70xc9pkqwv";
		assert_eq!(check(addr, Network::Bitcoin), Ok(()));
		assert_eq!(check(addr, Network::Signet), Err(SilentPaymentError::Network));
	}

	#[test]
	fn checks_addresses() {
		let sp = encode("sp", Fe32::Q, &keys());
		let tsp = encode("tsp", Fe32::Q, &keys());
		assert_eq!(check(&sp, Network::Bitcoin), Ok(()));
		assert_eq!(check(&tsp, Network::Signet), Ok(()));
		assert_eq!(check(&tsp, Network::Regtest), Ok(()));
		assert_eq!(check(&tsp, Network::Bitcoin), Err(SilentPaymentError::Network));
		assert_eq!(check(&sp, Network::Signet), Err(SilentPaymentError::Network));

		// A bad checksum.
		let mut bad = sp.clone();
		bad.pop();
		bad.push(if sp.ends_with('q') { 'p' } else { 'q' });
		assert_eq!(check(&bad, Network::Bitcoin), Err(SilentPaymentError::Encoding));

		// Another version.
		assert_eq!(
			check(&encode("sp", Fe32::P, &keys()), Network::Bitcoin),
			Err(SilentPaymentError::Version),
		);
		// A short payload, and keys that are not on the curve.
		assert_eq!(
			check(&encode("sp", Fe32::Q, &keys()[..65]), Network::Bitcoin),
			Err(SilentPaymentError::Keys),
		);
		let mut not_keys = keys();
		not_keys[0] = 0x05;
		assert_eq!(check(&encode("sp", Fe32::Q, &not_keys), Network::Bitcoin), Err(SilentPaymentError::Keys));
		// A segwit address is not a Silent Payment address.
		assert!(check("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq", Network::Bitcoin).is_err());
	}
}
