//! Random bearer tokens and codes. Only their hashes are stored.

use bitcoin::hashes::{Hash, sha256};
use bitcoin::hex::DisplayHex;
use bitcoin::secp256k1::rand::{RngCore, thread_rng};


/// A new random token of 32 bytes, as hex.
pub fn new_token() -> String {
	random_hex(32)
}

/// The secret of a new unlisted cashier link: 16 random bytes, as hex.
pub fn new_link_secret() -> String {
	random_hex(16)
}

pub fn random_hex(len: usize) -> String {
	let mut bytes = vec![0u8; len];
	thread_rng().fill_bytes(&mut bytes);
	bytes.to_lower_hex_string()
}

pub fn hash(token: &str) -> String {
	sha256::Hash::hash(token.as_bytes()).to_string()
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn tokens() {
		let a = new_token();
		assert_eq!(a.len(), 64);
		assert_ne!(a, new_token());
		assert_eq!(new_link_secret().len(), 32);
		assert_ne!(new_link_secret(), new_link_secret());
		assert_eq!(hash("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
	}
}
