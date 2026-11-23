CREATE TABLE claim_code (
	id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
	code_hash TEXT NOT NULL UNIQUE,
	locked_nym TEXT,
	label TEXT,
	expires_at TIMESTAMPTZ,
	used_at TIMESTAMPTZ,
	used_nym_id BIGINT REFERENCES nym(id),
	revoked_at TIMESTAMPTZ,
	created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
	CHECK ((used_at IS NULL) = (used_nym_id IS NULL)),
	CHECK (used_at IS NULL OR revoked_at IS NULL)
);

CREATE UNIQUE INDEX claim_code_name_lock ON claim_code (locked_nym)
	WHERE locked_nym IS NOT NULL AND used_at IS NULL AND revoked_at IS NULL;
