-- A nym: the permanent name of a Lightning Address, registered by the Ark
-- address that receives its payments. One nym per Ark address and one Ark
-- address per nym. Only the display name ever changes.
CREATE TABLE nym (
	id BIGSERIAL PRIMARY KEY,
	name TEXT NOT NULL UNIQUE,
	ark_address TEXT NOT NULL UNIQUE,
	silent_payment_address TEXT NOT NULL,
	display_name TEXT,
	registration_timestamp BIGINT NOT NULL,
	registration_signature TEXT NOT NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	-- Lets a store require the nym's own Ark address.
	UNIQUE (id, ark_address)
);

CREATE FUNCTION nym_is_permanent() RETURNS trigger AS $$
BEGIN
	IF TG_OP = 'DELETE' THEN
		RAISE EXCEPTION 'a nym cannot be deleted';
	END IF;
	IF NEW.name IS DISTINCT FROM OLD.name
		OR NEW.ark_address IS DISTINCT FROM OLD.ark_address
		OR NEW.silent_payment_address IS DISTINCT FROM OLD.silent_payment_address
		OR NEW.registration_timestamp IS DISTINCT FROM OLD.registration_timestamp
		OR NEW.registration_signature IS DISTINCT FROM OLD.registration_signature
	THEN
		RAISE EXCEPTION 'the name and destinations of a nym cannot change';
	END IF;
	RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER nym_is_permanent BEFORE UPDATE OR DELETE ON nym
	FOR EACH ROW EXECUTE FUNCTION nym_is_permanent();

-- A store: at most one per nym, paid at the nym's Ark address.
CREATE TABLE store (
	id BIGSERIAL PRIMARY KEY,
	nym_id BIGINT NOT NULL UNIQUE,
	name TEXT NOT NULL,
	ark_address TEXT NOT NULL UNIQUE,
	-- The unblinded mailbox of the owner's wallet, from posd's signed
	-- registration. Nothing uses it.
	mailbox_id TEXT NOT NULL,
	-- The public descriptor of the on-chain account the wallet chose.
	onchain_descriptor TEXT NOT NULL,
	-- The next derivation index to hand out for an on-chain sale. The
	-- owner's wallet scans the store account up to it.
	next_onchain_index BIGINT NOT NULL DEFAULT 0,
	currency TEXT NOT NULL,
	-- The Argon2id hash, with its salt, of the PIN that unlocks the list of
	-- a cashier screen's own sales. NULL: no PIN, and the list stays hidden.
	cashier_pin_hash TEXT,
	onchain_min_sat BIGINT NOT NULL CHECK (onchain_min_sat BETWEEN 0 AND 2100000000000000),
	registration_timestamp BIGINT NOT NULL,
	registration_signature TEXT NOT NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	FOREIGN KEY (nym_id, ark_address) REFERENCES nym (id, ark_address)
);

CREATE FUNCTION store_is_permanent() RETURNS trigger AS $$
BEGIN
	IF TG_OP = 'DELETE' THEN
		RAISE EXCEPTION 'a store cannot be deleted';
	END IF;
	IF NEW.nym_id IS DISTINCT FROM OLD.nym_id
		OR NEW.ark_address IS DISTINCT FROM OLD.ark_address
		OR NEW.mailbox_id IS DISTINCT FROM OLD.mailbox_id
		OR NEW.onchain_descriptor IS DISTINCT FROM OLD.onchain_descriptor
		OR NEW.registration_timestamp IS DISTINCT FROM OLD.registration_timestamp
		OR NEW.registration_signature IS DISTINCT FROM OLD.registration_signature
	THEN
		RAISE EXCEPTION 'the nym and destinations of a store cannot change';
	END IF;
	RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER store_is_permanent BEFORE UPDATE OR DELETE ON store
	FOR EACH ROW EXECUTE FUNCTION store_is_permanent();

CREATE TABLE owner_challenge (
	id BIGSERIAL PRIMARY KEY,
	ark_address TEXT NOT NULL,
	challenge TEXT NOT NULL UNIQUE,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	expires_at TIMESTAMPTZ NOT NULL,
	used_at TIMESTAMPTZ
);

-- An owner login. Only the hash of its token is stored.
CREATE TABLE owner_session (
	id BIGSERIAL PRIMARY KEY,
	nym_id BIGINT NOT NULL REFERENCES nym(id),
	token_hash TEXT NOT NULL UNIQUE,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	expires_at TIMESTAMPTZ NOT NULL
);

-- An invoice of a Lightning Address payment. Ark and Silent Payment
-- payments to a Lightning Address are not recorded: they reach the owner's
-- wallet without the service. A row is reserved under the caps before barkd
-- makes the invoice, so its payment hash and invoice come after.
CREATE TABLE lnurl_invoice (
	id BIGSERIAL PRIMARY KEY,
	nym_id BIGINT NOT NULL REFERENCES nym(id),
	payment_hash TEXT UNIQUE,
	invoice TEXT,
	amount_sat BIGINT NOT NULL,
	-- open, paid or expired.
	state TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'paid', 'expired')),
	received_sat BIGINT NOT NULL DEFAULT 0,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	expires_at TIMESTAMPTZ NOT NULL,
	settled_at TIMESTAMPTZ
);
CREATE INDEX lnurl_invoice_open_idx ON lnurl_invoice (nym_id) WHERE state = 'open';
CREATE INDEX lnurl_invoice_nym_idx ON lnurl_invoice (nym_id, id DESC);

-- in-progress: a payment is seen but does not count yet: an on-chain payment
-- in the mempool, or an Ark payment the Ark server has not delivered.
-- paid: for good, as in posd: a delivered invoice, or a confirmed payment.
CREATE TYPE sale_state AS ENUM ('open', 'in-progress', 'paid', 'expired', 'canceled');
CREATE TYPE payment_rail AS ENUM ('lightning', 'ark', 'onchain');

CREATE TABLE sale (
	id BIGSERIAL PRIMARY KEY,
	-- The identifier the API shows. It is random so that sales cannot be
	-- enumerated, and holding it is what lets a cashier follow or cancel
	-- the sale.
	public_id TEXT NOT NULL UNIQUE,
	store_id BIGINT NOT NULL REFERENCES store(id),
	-- At most 21,000,000 BTC, checked before the sale is reserved.
	amount_sat BIGINT NOT NULL CHECK (amount_sat BETWEEN 1 AND 2100000000000000),
	fiat_amount TEXT,
	currency TEXT,
	btc_price TEXT,
	memo TEXT,
	state sale_state NOT NULL DEFAULT 'open',
	-- Why the sale is in progress: onchain or ark.
	progress TEXT CHECK (progress IN ('onchain', 'ark')),
	-- What happened to a payment that went away: payment-replaced or
	-- payment-dropped.
	notice TEXT,
	paid_rail payment_rail,
	-- Only counted payments: delivered invoices and confirmed outputs.
	received_sat BIGINT NOT NULL DEFAULT 0,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	expires_at TIMESTAMPTZ NOT NULL,
	paid_at TIMESTAMPTZ,
	-- When the sale's invoice was stored. Before, the sale is only reserved:
	-- nobody sees it, and it is deleted if its invoice cannot be made.
	issued_at TIMESTAMPTZ,
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX sale_store_id_idx ON sale (store_id, id DESC);
CREATE INDEX sale_watch_idx ON sale (state, expires_at);

-- Wrong cashier PINs in a row, per store and per source, and the lock they
-- put on further checks.
CREATE TABLE pin_failure (
	scope TEXT PRIMARY KEY,
	failures INT NOT NULL DEFAULT 0,
	locked_until TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Shared rate-limit counters: hits per bucket, such as callbacks per nym.
CREATE TABLE rate_limit_event (
	id BIGSERIAL PRIMARY KEY,
	bucket TEXT NOT NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX rate_limit_event_idx ON rate_limit_event (bucket, created_at);

-- Shared distinct-name counters: which names, or registering addresses, a
-- source used.
CREATE TABLE name_access_event (
	id BIGSERIAL PRIMARY KEY,
	source_key TEXT NOT NULL,
	name TEXT NOT NULL,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX name_access_event_idx ON name_access_event (source_key, created_at);

-- One row per payment rail offered for a sale: the invoice, which
-- Lightning and Ark payers both pay, and the on-chain address. The invoice
-- row is reserved under the caps before barkd makes the invoice, so its
-- payment hash and invoice come after.
CREATE TABLE sale_payment (
	id BIGSERIAL PRIMARY KEY,
	sale_id BIGINT NOT NULL REFERENCES sale(id),
	-- lightning for the invoice, onchain for the address.
	rail payment_rail NOT NULL,
	payment_hash TEXT UNIQUE,
	invoice TEXT,
	-- The derivation index of the address. Each on-chain sale takes the
	-- store's next index, and no index is ever handed out twice.
	onchain_index BIGINT,
	-- An address belongs to one sale, even when two stores registered the
	-- same descriptor, so a payment to it counts for that sale only.
	onchain_address TEXT UNIQUE,
	-- The amount this rail delivered: the invoice amount once delivered, or
	-- the sum of the confirmed outputs to the address.
	received_sat BIGINT NOT NULL DEFAULT 0,
	-- The amount in unconfirmed outputs to the address. It never counts.
	pending_sat BIGINT NOT NULL DEFAULT 0,
	-- The warnings about the unconfirmed payments: not-guaranteed, low-fee,
	-- unconfirmed-parents, replaced.
	warnings TEXT[] NOT NULL DEFAULT '{}',
	-- When barkd first showed an Ark transfer of this invoice awaiting
	-- delivery. The sale stays in progress for it until barkd settles it.
	awaiting_delivery_at TIMESTAMPTZ,
	-- When barkd first reported a payment of this invoice arriving or
	-- delivered while its sale was expiring. The sale then never expires
	-- without barkd saying the invoice ended unpaid.
	arriving_at TIMESTAMPTZ,
	settled_at TIMESTAMPTZ,
	UNIQUE (sale_id, rail)
);

-- A transaction paying a sale's address, as the service last saw it.
CREATE TABLE onchain_tx (
	id BIGSERIAL PRIMARY KEY,
	sale_payment_id BIGINT NOT NULL REFERENCES sale_payment(id),
	txid TEXT NOT NULL,
	amount_sat BIGINT NOT NULL,
	-- seen, confirmed or gone.
	status TEXT NOT NULL CHECK (status IN ('seen', 'confirmed', 'gone')),
	confirmations BIGINT NOT NULL DEFAULT 0,
	low_fee BOOLEAN NOT NULL DEFAULT FALSE,
	unconfirmed_parents BOOLEAN NOT NULL DEFAULT FALSE,
	replaced BOOLEAN NOT NULL DEFAULT FALSE,
	-- How many answers in a row said it is not in the mempool.
	absent_count INT NOT NULL DEFAULT 0,
	first_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	UNIQUE (sale_payment_id, txid)
);
