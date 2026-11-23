-- Existing stores keep their satoshi threshold until the owner chooses a USD amount.
ALTER TABLE store ADD COLUMN onchain_min_usd_cents BIGINT CHECK (onchain_min_usd_cents >= 0);
