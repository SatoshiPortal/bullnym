//! Single-use grants for permanent nym registration.

use bitcoin::secp256k1::rand::{Rng, thread_rng};
use tokio_postgres::Transaction;

use crate::error::{ApiError, ApiResult};
use crate::token;

// Crockford's alphabet with the visually ambiguous digits 0 and 1 omitted.
const ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTVWXYZ";

pub fn generate() -> String {
	let mut rng = thread_rng();
	let mut code = String::with_capacity(19);
	for i in 0..16 {
		if i > 0 && i % 4 == 0 { code.push('-'); }
		code.push(ALPHABET[rng.gen_range(0..ALPHABET.len())] as char);
	}
	code
}

pub fn digest(code: &str) -> String {
	let normalized: String = code.chars().filter(|c| *c != '-' && !c.is_whitespace())
		.map(|c| c.to_ascii_uppercase()).collect();
	token::hash(&normalized)
}

/// The existing nym-cap lock also orders an operator's name reservation
/// against a registration for the same name.
pub async fn lock_registration(tx: &Transaction<'_>) -> Result<(), tokio_postgres::Error> {
	tx.execute("SELECT pg_advisory_xact_lock(hashtext('bullnym:nym-cap')::BIGINT)", &[]).await?;
	Ok(())
}

/// Called after the nym insert, inside the same transaction and registration lock.
pub async fn redeem(tx: &Transaction<'_>, nym_id: i64, nym: &str, required: bool, hash: Option<&str>) -> ApiResult<()> {
	let policy = tx.query_one("
		SELECT statement_timestamp(), EXISTS (SELECT 1 FROM claim_code WHERE locked_nym = $1
			AND used_at IS NULL AND revoked_at IS NULL
			AND (expires_at IS NULL OR expires_at > statement_timestamp()))
	", &[&nym]).await?;
	let checked_at: chrono::DateTime<chrono::Utc> = policy.get(0);
	let locked: bool = policy.get(1);
	if !locked && !required { return Ok(()); }
	let hash = hash.ok_or(ApiError::ClaimCodeRequired)?;
	let expected_lock = locked.then_some(nym);
	let changed = tx.execute("
		UPDATE claim_code SET used_at = $4, used_nym_id = $1
		WHERE code_hash = $2 AND locked_nym IS NOT DISTINCT FROM $3
			AND used_at IS NULL AND revoked_at IS NULL
			AND (expires_at IS NULL OR expires_at > $4)
	", &[&nym_id, &hash, &expected_lock, &checked_at]).await?;
	if changed == 0 { return Err(ApiError::ClaimCodeInvalid); }
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn grouped_codes_are_case_and_whitespace_insensitive() {
		let code = generate();
		let groups: Vec<_> = code.split('-').collect();
		assert_eq!(groups.len(), 4);
		assert!(groups.iter().all(|g| g.len() == 4 && g.bytes().all(|c| ALPHABET.contains(&c))));
		assert_eq!(digest(&code), digest(&groups.join(" \t").to_lowercase()));
		assert_ne!(digest("ABCD-EFGH-JKMN-PQRS"), digest("ABCD-EFGH-JKMN-PQRT"));
	}
}
