//! The cashier PIN, which unlocks the list of a cashier screen's own sales.
//!
//! The owner sets or changes it through the owner API. The service keeps
//! only an Argon2id hash with a random salt, and checks a PIN for the
//! cashier screen: the browser never gets the hash. Without a PIN the list
//! stays hidden. Selling, following a sale and canceling it need no PIN.
//!
//! Wrong PINs back off per source and per store: a few are free, then each
//! one locks further checks for twice as long as the one before, up to
//! [MAX_LOCK_SECS]. A right PIN clears nothing, so a PIN known for one
//! store cannot reset a source's count while it guesses at another. A
//! count is forgotten a day after its last wrong PIN, and a store's count
//! also when the owner sets a new PIN.

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use tokio_postgres::GenericClient;


pub const MIN_PIN_DIGITS: usize = 4;
pub const MAX_PIN_DIGITS: usize = 12;

/// Wrong PINs before any lock.
pub const FREE_FAILURES: i32 = 5;
/// The longest lock after a wrong PIN.
pub const MAX_LOCK_SECS: i64 = 900;
/// A count of wrong PINs is forgotten this long after the last one.
pub const FAILURE_MEMORY_SECS: i64 = 86_400;

/// Checks the form of a new PIN: 4 to 12 digits, as the keypad types.
pub fn check_format(pin: &str) -> Result<(), String> {
	if pin.len() < MIN_PIN_DIGITS || pin.len() > MAX_PIN_DIGITS || !pin.bytes().all(|b| b.is_ascii_digit()) {
		return Err(format!("the PIN must be {} to {} digits", MIN_PIN_DIGITS, MAX_PIN_DIGITS));
	}
	Ok(())
}

/// The Argon2id hash of a PIN, with a random salt, in PHC string form.
pub fn hash(pin: &str) -> anyhow::Result<String> {
	let salt = SaltString::generate(&mut OsRng);
	let hash = Argon2::default().hash_password(pin.as_bytes(), &salt)
		.map_err(|e| anyhow::anyhow!("hashing the PIN failed: {}", e))?;
	Ok(hash.to_string())
}

/// Whether `pin` matches a hash from [hash].
pub fn verify(pin: &str, hash: &str) -> bool {
	match PasswordHash::new(hash) {
		Ok(parsed) => Argon2::default().verify_password(pin.as_bytes(), &parsed).is_ok(),
		Err(e) => {
			tracing::error!("a stored PIN hash is not valid: {}", e);
			false
		},
	}
}

/// How long checks lock after the `failures`-th wrong PIN in a row.
pub fn lock_secs(failures: i32) -> i64 {
	if failures <= FREE_FAILURES {
		return 0;
	}
	let doublings = (failures - FREE_FAILURES).min(20) as u32;
	(1i64 << doublings).min(MAX_LOCK_SECS)
}

/// The counters a check uses: the store's, and the source's for any store.
pub fn scopes(store_id: i64, source: &str) -> [String; 2] {
	[format!("store:{}", store_id), format!("source:{}", source)]
}

/// Admits a check in `tx`: locks the counters of `scopes` until `tx` ends,
/// so concurrent checks of the same store or source run one after the
/// other, each seeing the failures of the ones before. Returns the seconds
/// until the longest of their locks ends, 0 if none is locked.
pub async fn admit(tx: &tokio_postgres::Transaction<'_>, scopes: &[String]) -> anyhow::Result<i64> {
	// Always in the same order, so two checks cannot wait on each other.
	let mut sorted = scopes.to_vec();
	sorted.sort();
	for scope in &sorted {
		tx.execute("INSERT INTO pin_failure (scope) VALUES ($1) ON CONFLICT DO NOTHING", &[scope]).await?;
	}
	let row = tx.query_one("
		WITH locked AS (SELECT locked_until FROM pin_failure WHERE scope = ANY($1) ORDER BY scope FOR UPDATE)
		SELECT coalesce(ceil(max(extract(epoch FROM locked_until - NOW()))), 0)::BIGINT AS secs FROM locked
	", &[&sorted]).await?;
	Ok(row.get::<_, i64>("secs").max(0))
}

/// Counts a wrong PIN in each scope and locks it as its count requires,
/// in the transaction that admitted the check. Returns the longest lock, in
/// seconds.
pub async fn record_failure(tx: &tokio_postgres::Transaction<'_>, scopes: &[String]) -> anyhow::Result<i64> {
	let mut longest = 0;
	for scope in scopes {
		let failures: i32 = tx.query_one("
			INSERT INTO pin_failure (scope, failures) VALUES ($1, 1)
			ON CONFLICT (scope) DO UPDATE SET
				failures = CASE
					WHEN pin_failure.updated_at < NOW() - make_interval(secs => $2) THEN 1
					ELSE pin_failure.failures + 1
				END,
				updated_at = NOW()
			RETURNING failures
		", &[scope, &(FAILURE_MEMORY_SECS as f64)]).await?.get("failures");
		let secs = lock_secs(failures);
		tx.execute("
			UPDATE pin_failure SET locked_until = NOW() + make_interval(secs => $2) WHERE scope = $1
		", &[scope, &(secs as f64)]).await?;
		longest = longest.max(secs);
	}
	Ok(longest)
}

/// Forgets the wrong PINs of a scope.
pub async fn clear(conn: &impl GenericClient, scope: &str) -> anyhow::Result<()> {
	conn.execute("DELETE FROM pin_failure WHERE scope = $1", &[&scope]).await?;
	Ok(())
}

/// Drops counts that are forgotten and no longer lock anything.
pub async fn sweep(conn: &impl GenericClient) -> anyhow::Result<()> {
	conn.execute("
		DELETE FROM pin_failure
		WHERE updated_at < NOW() - make_interval(secs => $1) AND locked_until < NOW()
	", &[&(FAILURE_MEMORY_SECS as f64)]).await?;
	Ok(())
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn pin_format() {
		assert!(check_format("1234").is_ok());
		assert!(check_format("123456789012").is_ok());
		for bad in ["", "123", "1234567890123", "12a4", " 1234", "１２３４"] {
			assert!(check_format(bad).is_err(), "{:?}", bad);
		}
	}

	#[test]
	fn hashes_are_salted_and_verify() {
		let a = hash("4321").unwrap();
		let b = hash("4321").unwrap();
		assert!(a.starts_with("$argon2id$"), "{}", a);
		assert_ne!(a, b, "each hash has its own salt");
		assert!(!a.contains("4321"));
		assert!(verify("4321", &a) && verify("4321", &b));
		assert!(!verify("1234", &a));
		assert!(!verify("4321", "not a hash"));
	}

	#[test]
	fn backoff_doubles_after_the_free_failures() {
		for f in 0..=FREE_FAILURES {
			assert_eq!(lock_secs(f), 0);
		}
		assert_eq!(lock_secs(FREE_FAILURES + 1), 2);
		assert_eq!(lock_secs(FREE_FAILURES + 2), 4);
		assert_eq!(lock_secs(FREE_FAILURES + 9), 512);
		assert_eq!(lock_secs(FREE_FAILURES + 10), MAX_LOCK_SECS);
		assert_eq!(lock_secs(i32::MAX), MAX_LOCK_SECS);
	}
}
