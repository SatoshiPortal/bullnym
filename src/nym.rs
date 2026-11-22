//! The nym: a permanent public name, which is the Lightning Address.
//!
//! A nym is fixed to the Ark address that registered it and can never be
//! renamed, re-pointed, released or registered again. Names that would
//! shadow a route of the service, or pass for the operator, are reserved.

/// The longest nym.
pub const MAX_NYM_LEN: usize = 32;

/// Names that would shadow a route of the service if a nym took them, as
/// in Bullnym's `reserved_nyms.rs`, plus this service's own routes.
pub const RESERVED_ROUTE_NAMES: &[&str] = &[
	"register", "health", "ready", "version", "webhook", "lnurlp", "api", "img",
	"donation-page", "well-known", "admin", "static", "assets", "favicon", "robots",
	"sitemap", "about", "terms", "privacy", "support", "help", "login", "logout",
	"signup", "settings", "account", "dashboard", "test", "i", "invoice", "invoices",
	"pos", "a",
	// This service's routes and web app paths.
	"owner", "nym", "nyms", "store", "stores", "checkout", "sales", "sale",
	"icons", "manifest", "sw", "workbox", "registersw", "index", "pair", "cashier",
	"history", "receipt", "pay",
];

/// Names that would pass for the operator, as in Bullnym's reserved
/// aliases.
pub const RESERVED_BRAND_NAMES: &[&str] = &[
	"bullbitcoin", "bull-bitcoin", "bullpay", "bullnym", "bull", "bitcoin",
	"scuttle", "ark", "lightning",
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NymError {
	#[error("a nym has 1 to 32 characters")]
	Length,
	#[error("a nym has only lowercase letters, digits and inner hyphens")]
	Charset,
	#[error("this nym is reserved")]
	Reserved,
}

/// Checks the form of a nym: 1 to 32 lowercase letters, digits and
/// hyphens, starting and ending with a letter or digit. This is Bullnym's
/// rule; it fits LUD-16 and URL paths without escaping.
pub fn check_form(nym: &str) -> Result<(), NymError> {
	if nym.is_empty() || nym.len() > MAX_NYM_LEN {
		return Err(NymError::Length);
	}
	let ok_char = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-';
	let bytes = nym.as_bytes();
	if !bytes.iter().all(|b| ok_char(*b)) || bytes[0] == b'-' || bytes[bytes.len() - 1] == b'-' {
		return Err(NymError::Charset);
	}
	Ok(())
}

/// Whether `nym` is reserved, by the built-in lists or by `extra` from the
/// config.
pub fn is_reserved(nym: &str, extra: &[String]) -> bool {
	RESERVED_ROUTE_NAMES.contains(&nym)
		|| RESERVED_BRAND_NAMES.contains(&nym)
		|| extra.iter().any(|e| e.eq_ignore_ascii_case(nym))
}

/// Checks a nym for registration.
pub fn check(nym: &str, extra_reserved: &[String]) -> Result<(), NymError> {
	check_form(nym)?;
	if is_reserved(nym, extra_reserved) {
		return Err(NymError::Reserved);
	}
	Ok(())
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn nym_form() {
		for ok in ["francis", "my-nym", "abc", "user123", "a-b", "a", "7"] {
			assert_eq!(check_form(ok), Ok(()), "{ok}");
		}
		assert_eq!(check_form(&"a".repeat(32)), Ok(()));
		assert_eq!(check_form(&"a".repeat(33)), Err(NymError::Length));
		assert_eq!(check_form(""), Err(NymError::Length));
		for bad in ["Francis", "-mynym", "mynym-", "has space", "has_underscore", "user@name",
			"user.name", "user!name", "café"]
		{
			assert_eq!(check_form(bad), Err(NymError::Charset), "{bad}");
		}
	}

	#[test]
	fn reserved_names() {
		assert_eq!(check("api", &[]), Err(NymError::Reserved));
		assert_eq!(check("owner", &[]), Err(NymError::Reserved));
		assert_eq!(check("bullbitcoin", &[]), Err(NymError::Reserved));
		assert_eq!(check("cafe", &[]), Ok(()));
		assert_eq!(check("cafe", &["Cafe".to_string()]), Err(NymError::Reserved));
		// Every reserved name is itself a valid form, so the lists hold no
		// dead entries.
		for n in RESERVED_ROUTE_NAMES.iter().chain(RESERVED_BRAND_NAMES) {
			assert_eq!(check_form(n), Ok(()), "{n}");
		}
	}
}
