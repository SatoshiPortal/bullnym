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
	onchain_descriptor TEXT NOT NULL,
	-- The extended public key of the descriptor. A store's on-chain account
	-- is its own: no two stores share one.
	onchain_xpub TEXT NOT NULL UNIQUE,
	-- The next derivation index to hand out for an on-chain sale.
	next_onchain_index BIGINT NOT NULL DEFAULT 0,
	currency TEXT NOT NULL,
	onchain_min_sat BIGINT NOT NULL,
	-- Whether anyone can open the store's checkout under its nym.
	public_checkout BOOLEAN NOT NULL DEFAULT TRUE,
	-- The hash of the secret of the unlisted cashier link, if there is one.
	unlisted_link_hash TEXT UNIQUE,
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
		OR NEW.onchain_descriptor IS DISTINCT FROM OLD.onchain_descriptor
		OR NEW.onchain_xpub IS DISTINCT FROM OLD.onchain_xpub
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
-- wallet without the service.
CREATE TABLE lnurl_invoice (
	id BIGSERIAL PRIMARY KEY,
	nym_id BIGINT NOT NULL REFERENCES nym(id),
	payment_hash TEXT NOT NULL UNIQUE,
	invoice TEXT NOT NULL,
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
-- payment-issue: a counted on-chain payment went away in a reorg.
CREATE TYPE sale_state AS ENUM ('open', 'in-progress', 'paid', 'payment-issue', 'expired', 'canceled');
CREATE TYPE payment_rail AS ENUM ('lightning', 'ark', 'onchain');
-- How a sale was created: through the store's public checkout under its
-- nym, or through its unlisted cashier link.
CREATE TYPE checkout_link AS ENUM ('public', 'unlisted');

CREATE TABLE sale (
	id BIGSERIAL PRIMARY KEY,
	-- The identifier the API shows. It is random so that sales cannot be
	-- enumerated, and holding it is what lets a cashier follow or cancel
	-- the sale.
	public_id TEXT NOT NULL UNIQUE,
	store_id BIGINT NOT NULL REFERENCES store(id),
	link checkout_link NOT NULL,
	-- A name typed on the cashier screen. The service never verifies it.
	cashier_name TEXT,
	amount_sat BIGINT NOT NULL,
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
	-- Paid by an invoice, or by on-chain payments with 3 confirmations.
	final_at TIMESTAMPTZ,
	updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX sale_store_id_idx ON sale (store_id, id DESC);
CREATE INDEX sale_watch_idx ON sale (state, expires_at);

-- An on-chain address of a store. It is handed to one live sale at a time.
-- An address that never received anything is handed out again once its last
-- sale ended more than the grace period ago; one that received anything,
-- confirmed or not, never is.
CREATE TABLE onchain_address (
	id BIGSERIAL PRIMARY KEY,
	store_id BIGINT NOT NULL REFERENCES store(id),
	derivation_index BIGINT NOT NULL,
	address TEXT NOT NULL UNIQUE,
	ever_received BOOLEAN NOT NULL DEFAULT FALSE,
	-- The sale it is handed to, while that sale is live.
	sale_id BIGINT UNIQUE REFERENCES sale(id),
	-- When its last sale ended.
	released_at TIMESTAMPTZ,
	created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	UNIQUE (store_id, derivation_index)
);
CREATE INDEX onchain_address_reuse_idx ON onchain_address (store_id, derivation_index)
	WHERE NOT ever_received AND sale_id IS NULL;

-- One row per payment rail offered for a sale: the invoice, which
-- Lightning and Ark payers both pay, and the on-chain address.
CREATE TABLE sale_payment (
	id BIGSERIAL PRIMARY KEY,
	sale_id BIGINT NOT NULL REFERENCES sale(id),
	-- lightning for the invoice, onchain for the address.
	rail payment_rail NOT NULL,
	payment_hash TEXT UNIQUE,
	invoice TEXT,
	onchain_address_id BIGINT REFERENCES onchain_address(id),
	onchain_address TEXT,
	-- The amount this rail delivered: the invoice amount once delivered, or
	-- the sum of the confirmed outputs to the address.
	received_sat BIGINT NOT NULL DEFAULT 0,
	-- The amount in unconfirmed outputs to the address. It never counts.
	pending_sat BIGINT NOT NULL DEFAULT 0,
	-- The warnings about the unconfirmed payments: not-guaranteed, low-fee,
	-- unconfirmed-parents, replaced.
	warnings TEXT[] NOT NULL DEFAULT '{}',
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
