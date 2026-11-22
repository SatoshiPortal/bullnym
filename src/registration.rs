//! The registrations that the owner's wallet signs.
//!
//! The wallet signs a registration's [message](NymRegistration::message)
//! with bark's message signing (BIP 340, `bark/message` tag), using the key
//! of the Ark address that receives the payments. Any wallet with that
//! signing call can register: no wallet code is specific to this service.
//!
//! A message is a versioned header, then one `field:value` line per field,
//! joined by `\n`, with no trailing newline.
//!
//! - A [NymRegistration] creates the Lightning Address.
//! - A [StoreRegistration] creates the store of a nym. It is posd's store
//!   registration, with `nym` added.
//!
//! The Ark address of both must be one the Ark server accepts as the
//! destination hint of a forwarded invoice: for this server and network,
//! paying a public key, with exactly one server mailbox delivery. The
//! server also unblinds the mailbox with its own key, which the service
//! cannot do.

use std::str::FromStr;

use ark::VtxoPolicy;
use ark::address::VtxoDelivery;
use ark::mailbox::MailboxIdentifier;
use bitcoin::Network;
use bitcoin::secp256k1::{PublicKey, schnorr};
use serde::{Deserialize, Serialize};


/// The first line of a nym registration.
pub const NYM_REGISTRATION_HEADER: &str = "bullnym registration v1";

/// The first line of a store registration, as in posd.
pub const STORE_REGISTRATION_HEADER: &str = "scuttle-pos registration v1";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistrationError {
	#[error("field {0} is empty")]
	Empty(&'static str),
	#[error("field {0} contains a line break")]
	LineBreak(&'static str),
	#[error("not a valid Ark address")]
	BadAddress,
	#[error("not a valid mailbox id")]
	BadMailbox,
	#[error("the Ark address is for another network")]
	NetworkMismatch,
	#[error("the Ark address is for another Ark server")]
	ServerMismatch,
	#[error("the Ark address must pay a public key and have exactly one server mailbox delivery")]
	UnsupportedAddress,
	#[error("the signature is not valid for the Ark address")]
	BadSignature,
}

/// Checks that no field is empty or holds a line break. A line break would
/// let one field pose as another.
fn check_fields(fields: &[(&'static str, &str)]) -> Result<(), RegistrationError> {
	for (field, value) in fields {
		if value.is_empty() {
			return Err(RegistrationError::Empty(field));
		}
		if value.contains(['\n', '\r']) {
			return Err(RegistrationError::LineBreak(field));
		}
	}
	Ok(())
}

/// Parses an Ark address that the service can pay, with the checks the
/// Ark server makes on the destination hint of a forwarded invoice: an
/// address for `network`, of the server with `server_pubkey`, paying a
/// public key, with exactly one delivery, to the server mailbox. The
/// address carries the mailbox only in blinded form, which is all payments
/// need.
pub fn parse_owner_address(
	address: &str,
	server_pubkey: PublicKey,
	network: Network,
) -> Result<ark::Address, RegistrationError> {
	let address = ark::Address::from_str(address).map_err(|_| RegistrationError::BadAddress)?;
	if address.is_testnet() != (network != Network::Bitcoin) {
		return Err(RegistrationError::NetworkMismatch);
	}
	if !address.is_for_server(server_pubkey) {
		return Err(RegistrationError::ServerMismatch);
	}
	let pubkey_policy = matches!(address.policy(), VtxoPolicy::Pubkey(_));
	let one_mailbox = matches!(address.delivery(), [VtxoDelivery::ServerMailbox { .. }]);
	if !pubkey_policy || !one_mailbox {
		return Err(RegistrationError::UnsupportedAddress);
	}
	Ok(address)
}

fn verify_signature(
	address: &ark::Address,
	message: &str,
	signature: &schnorr::Signature,
) -> Result<(), RegistrationError> {
	if !ark::message::verify_for_address(address, message.as_bytes(), signature) {
		return Err(RegistrationError::BadSignature);
	}
	Ok(())
}

/// The registration of a nym, which creates its Lightning Address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NymRegistration {
	pub nym: String,
	/// The Ark address every Ark payment to the owner goes to. Its key owns
	/// the nym.
	pub ark_address: String,
	/// The owner's static Silent Payment address, for Bitcoin payments to
	/// the Lightning Address.
	pub silent_payment_address: String,
	/// Seconds since the UNIX epoch.
	pub timestamp: u64,
}

impl NymRegistration {
	pub fn message(&self) -> String {
		format!(
			"{}\nnym:{}\nark_address:{}\nsilent_payment_address:{}\ntimestamp:{}",
			NYM_REGISTRATION_HEADER, self.nym, self.ark_address, self.silent_payment_address,
			self.timestamp,
		)
	}

	/// Checks the fields, the address and the signature. The service checks
	/// the timestamp, the nym and the Silent Payment address itself.
	pub fn verify(
		&self,
		signature: &schnorr::Signature,
		server_pubkey: PublicKey,
		network: Network,
	) -> Result<ark::Address, RegistrationError> {
		check_fields(&[
			("nym", &self.nym),
			("ark_address", &self.ark_address),
			("silent_payment_address", &self.silent_payment_address),
		])?;
		let address = parse_owner_address(&self.ark_address, server_pubkey, network)?;
		verify_signature(&address, &self.message(), signature)?;
		Ok(address)
	}
}

/// The registration of a store, as in posd, with `nym` added last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreRegistration {
	/// The store's display name.
	pub name: String,
	/// The nym's registered Ark address.
	pub ark_address: String,
	/// The unblinded mailbox of the owner's wallet, as in posd. The service
	/// stores it and does not use it: payments need only the blinded
	/// mailbox in the Ark address.
	pub mailbox_id: String,
	/// The public descriptor of the on-chain account the wallet chose.
	pub onchain_descriptor: String,
	pub currency: String,
	/// Seconds since the UNIX epoch.
	pub timestamp: u64,
	/// The wallet's nym. The store's Lightning address is always the nym.
	pub nym: String,
}

impl StoreRegistration {
	pub fn message(&self) -> String {
		format!(
			"{}\nname:{}\nark_address:{}\nmailbox_id:{}\nonchain_descriptor:{}\ncurrency:{}\n\
			timestamp:{}\nnym:{}",
			STORE_REGISTRATION_HEADER, self.name, self.ark_address, self.mailbox_id,
			self.onchain_descriptor, self.currency, self.timestamp, self.nym,
		)
	}

	/// Checks the fields, the address, the form of the mailbox id and the
	/// signature. The service checks the timestamp, the descriptor, the
	/// currency and the nym itself. As in posd, nothing checks that the
	/// mailbox id belongs to the address.
	pub fn verify(
		&self,
		signature: &schnorr::Signature,
		server_pubkey: PublicKey,
		network: Network,
	) -> Result<(ark::Address, MailboxIdentifier), RegistrationError> {
		check_fields(&[
			("name", &self.name),
			("ark_address", &self.ark_address),
			("mailbox_id", &self.mailbox_id),
			("onchain_descriptor", &self.onchain_descriptor),
			("currency", &self.currency),
			("nym", &self.nym),
		])?;
		let address = parse_owner_address(&self.ark_address, server_pubkey, network)?;
		let mailbox = MailboxIdentifier::from_str(&self.mailbox_id)
			.map_err(|_| RegistrationError::BadMailbox)?;
		verify_signature(&address, &self.message(), signature)?;
		Ok((address, mailbox))
	}
}

#[cfg(test)]
mod test {
	use super::*;

	use bitcoin::secp256k1::{Keypair, Secp256k1, SecretKey};

	fn server() -> (PublicKey, PublicKey) {
		let secp = Secp256k1::new();
		let server = SecretKey::from_slice(&[3u8; 32]).unwrap().public_key(&secp);
		let mailbox = SecretKey::from_slice(&[4u8; 32]).unwrap().public_key(&secp);
		(server, mailbox)
	}

	fn owner_key() -> Keypair {
		Keypair::from_seckey_slice(&Secp256k1::new(), &[5u8; 32]).unwrap()
	}

	fn owner_address(server_pubkey: PublicKey, mailbox_pubkey: PublicKey) -> ark::Address {
		let key = owner_key();
		ark::Address::builder()
			.testnet(true)
			.server_pubkey(server_pubkey)
			.pubkey_policy(key.public_key())
			.mailbox(mailbox_pubkey, MailboxIdentifier::from_pubkey(key.public_key()), &key).unwrap()
			.into_address().unwrap()
	}

	fn mailbox_id() -> String {
		MailboxIdentifier::from_pubkey(owner_key().public_key()).to_string()
	}

	fn nym_registration(address: &ark::Address) -> NymRegistration {
		NymRegistration {
			nym: "cafe".into(),
			ark_address: address.to_string(),
			silent_payment_address: "tsp1placeholder".into(),
			timestamp: 1795190400,
		}
	}

	fn store_registration(address: &ark::Address) -> StoreRegistration {
		StoreRegistration {
			name: "Café Uvita".into(),
			ark_address: address.to_string(),
			mailbox_id: mailbox_id(),
			onchain_descriptor: "tr(xpub/0/*)".into(),
			currency: "CRC".into(),
			timestamp: 1795190400,
			nym: "cafe".into(),
		}
	}

	#[test]
	fn message_formats() {
		let (server, mailbox) = server();
		let address = owner_address(server, mailbox);
		assert_eq!(nym_registration(&address).message(), format!(
			"bullnym registration v1\nnym:cafe\nark_address:{}\n\
			silent_payment_address:tsp1placeholder\ntimestamp:1795190400",
			address,
		));
		// posd's message, in posd's order, with nym added last.
		assert_eq!(store_registration(&address).message(), format!(
			"scuttle-pos registration v1\nname:Café Uvita\nark_address:{}\nmailbox_id:{}\n\
			onchain_descriptor:tr(xpub/0/*)\ncurrency:CRC\ntimestamp:1795190400\nnym:cafe",
			address, mailbox_id(),
		));
	}

	#[test]
	fn verifies_signatures() {
		let (server, mailbox) = server();
		let address = owner_address(server, mailbox);
		let reg = nym_registration(&address);
		let sig = ark::message::sign(&owner_key(), reg.message().as_bytes());
		assert_eq!(reg.verify(&sig, server, NET).unwrap().to_string(), address.to_string());

		// Any change to a signed field breaks the signature.
		let mut other = reg.clone();
		other.nym = "cafe2".into();
		assert_eq!(other.verify(&sig, server, NET), Err(RegistrationError::BadSignature));

		// Another key's signature.
		let thief = Keypair::from_seckey_slice(&Secp256k1::new(), &[9u8; 32]).unwrap();
		let sig = ark::message::sign(&thief, reg.message().as_bytes());
		assert_eq!(reg.verify(&sig, server, NET), Err(RegistrationError::BadSignature));

		let store = store_registration(&address);
		let sig = ark::message::sign(&owner_key(), store.message().as_bytes());
		let (a, mailbox) = store.verify(&sig, server, NET).unwrap();
		assert_eq!((a.to_string(), mailbox.to_string()), (address.to_string(), mailbox_id()));
		// The mailbox id is signed.
		let mut other = store.clone();
		other.mailbox_id = MailboxIdentifier::from_pubkey(thief.public_key()).to_string();
		assert_eq!(other.verify(&sig, server, NET).err(), Some(RegistrationError::BadSignature));
		let mut other = store.clone();
		other.mailbox_id = "02zz".into();
		assert_eq!(other.verify(&sig, server, NET).err(), Some(RegistrationError::BadMailbox));
	}

	const NET: Network = Network::Regtest;

	#[test]
	fn the_address_must_be_a_usable_hint() {
		let (server, mailbox) = server();
		let key = owner_key();
		let parse = |a: &ark::Address, net| parse_owner_address(&a.to_string(), server, net);
		let good = owner_address(server, mailbox);
		assert!(parse(&good, Network::Regtest).is_ok());
		assert!(parse(&good, Network::Signet).is_ok());
		// A test address on mainnet.
		assert_eq!(parse(&good, Network::Bitcoin).err(), Some(RegistrationError::NetworkMismatch));
		// A mainnet address on a test network.
		let main = ark::Address::builder()
			.server_pubkey(server)
			.pubkey_policy(key.public_key())
			.mailbox(mailbox, MailboxIdentifier::from_pubkey(key.public_key()), &key).unwrap()
			.into_address().unwrap();
		assert_eq!(parse(&main, Network::Regtest).err(), Some(RegistrationError::NetworkMismatch));
		assert!(parse(&main, Network::Bitcoin).is_ok());
		// No server mailbox, or a second delivery next to it.
		let other = VtxoDelivery::Unknown { delivery_type: 0x7f, data: vec![1, 2, 3] };
		let no_mailbox = ark::Address::builder()
			.testnet(true)
			.server_pubkey(server)
			.pubkey_policy(key.public_key())
			.delivery(other.clone())
			.into_address().unwrap();
		assert_eq!(parse(&no_mailbox, Network::Regtest).err(), Some(RegistrationError::UnsupportedAddress));
		let mailbox_and_other = ark::Address::builder()
			.testnet(true)
			.server_pubkey(server)
			.pubkey_policy(key.public_key())
			.mailbox(mailbox, MailboxIdentifier::from_pubkey(key.public_key()), &key).unwrap()
			.delivery(other)
			.into_address().unwrap();
		assert_eq!(parse(&mailbox_and_other, Network::Regtest).err(), Some(RegistrationError::UnsupportedAddress));
		let blinded = match good.delivery() {
			[d @ VtxoDelivery::ServerMailbox { .. }] => d.clone(),
			other => panic!("unexpected delivery {:?}", other),
		};
		let two = ark::Address::builder()
			.testnet(true)
			.server_pubkey(server)
			.pubkey_policy(key.public_key())
			.delivery(blinded.clone())
			.delivery(blinded)
			.into_address().unwrap();
		assert_eq!(parse(&two, Network::Regtest).err(), Some(RegistrationError::UnsupportedAddress));
	}

	#[test]
	fn refuses_bad_fields() {
		let (server, mailbox) = server();
		let address = owner_address(server, mailbox);
		let sig = schnorr::Signature::from_slice(&[1u8; 64]).unwrap();

		let mut reg = nym_registration(&address);
		reg.nym = "a\nark_address:x".into();
		assert_eq!(reg.verify(&sig, server, NET), Err(RegistrationError::LineBreak("nym")));
		let mut reg = store_registration(&address);
		reg.currency = String::new();
		assert_eq!(reg.verify(&sig, server, NET).err(), Some(RegistrationError::Empty("currency")));
		let mut reg = store_registration(&address);
		reg.mailbox_id = String::new();
		assert_eq!(reg.verify(&sig, server, NET).err(), Some(RegistrationError::Empty("mailbox_id")));
		let mut reg = store_registration(&address);
		reg.nym = "x\r".into();
		assert_eq!(reg.verify(&sig, server, NET).err(), Some(RegistrationError::LineBreak("nym")));

		let mut reg = nym_registration(&address);
		reg.ark_address = "tark1placeholder".into();
		assert_eq!(reg.verify(&sig, server, NET), Err(RegistrationError::BadAddress));

		// An address of another Ark server.
		let other_server = SecretKey::from_slice(&[7u8; 32]).unwrap().public_key(&Secp256k1::new());
		let reg = nym_registration(&owner_address(other_server, mailbox));
		assert_eq!(reg.verify(&sig, server, NET), Err(RegistrationError::ServerMismatch));
	}
}
