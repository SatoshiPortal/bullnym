# bullnym

bullnym is a Lightning Address and store service for Ark wallets on a
Scuttle Ark server. Every payment goes to the owner's wallet, which does not
need to be online. The service holds no funds and no private keys.

It started from posd, Lee Salminen's point-of-sale service for Scuttle. See
[NOTICE](NOTICE).

| | Lightning Address | Store |
|---|---|---|
| Lightning | A barkd forwarding invoice, paying the registered Ark address | One barkd forwarding invoice that names the registered Ark address as its hint |
| Ark | LUD-22 `SCUTTLE`: the registered Ark address | Through the invoice's hint |
| Bitcoin | LUD-22 `SP`: the static Silent Payment address | On the merchant's request, an address per sale from the descriptor of the account the wallet chose |
| Who starts a payment | Any payer, through LNURL | Anyone with the store's cashier link, which is also its public checkout |

- **The nym** is permanent and is the Lightning Address: `<nym>@<domain>`.
  It is registered by the Ark address that receives its payments, with a
  message the wallet signs.
- **The store** is a separate registration from the same wallet, after the
  Lightning Address, with the same Ark address. A nym has one store.
- **The cashier link** replaces posd's paired terminals. Any device with it
  takes payments; the store's history and CSV are behind the owner login.
  The screen lists its own sales only after the service accepted the
  owner's cashier PIN.
- **Bitcoin for the store** is added to a sale only when the merchant asks,
  after a warning that it can take up to 60 minutes to confirm. It is an
  address per sale from the store's descriptor, as in posd: never reused,
  not even by another store with the same descriptor. The owner's wallet
  scans the store account up to the store's next index, which the owner's
  store view shows. A QR never combines Lightning and Bitcoin.
- **Paid** means: an invoice barkd reports settled, whether a Lightning
  payment or an Ark transfer delivered to the owner's mailbox (S4); on-chain
  at 1 confirmation. Paid is final, as in posd. A
  payment seen but not counted shows as in progress, not guaranteed, and
  never as paid.

The API is in [docs/api.md](docs/api.md). `bullnym.default.toml` documents
every setting.

## What the service needs

- A Scuttle Ark server in expiry payout mode, with
  `ln_receive_ark_address_hints = true`, and a release with S1 (a forwarded
  invoice that names its destination in its hint) and S4 (the delivery of
  Ark payments in the receive status). As posd did, the service refuses to
  start unless the server is in expiry payout mode, and refuses a sale whose
  invoice does not name the store's Ark address in its hint, with a message
  saying the server must allow Ark address hints.
- A private barkd on that server. It issues the forwarding invoices and
  forwards the payments to the owners. It needs no funds. Run it with
  `daemon_sync_interval_secs = 5` in its `config.toml`: a sale paid over Ark
  counts only once barkd settles the receive, and barkd checks on that
  interval.
- A bitcoind. The service creates a watch-only wallet on it.
- A Postgres database.

The Ark operator sets the Lightning receive fee, which forwarded receives
pay, and the server's limits. One barkd making every receive is one heavy
client: agree its traffic with the Ark operator.

## Build

```sh
(cd web && npm ci && npm run build)   # builds web/dist, which the binary embeds
cargo build --release
bullnym --config bullnym.toml start
```

`contrib/build-static.sh` builds a static x86_64 Linux binary in a Docker
container. The Scuttle crates come from the `SatoshiPortal/scuttle`
repository at the release tag set in `Cargo.toml`.

## Tests

```sh
cargo test --lib                      # unit tests
(cd web && npm test && npm run check)
```

The integration tests (`tests/integration.rs`) run the service in the test
process against a real Ark server, barkd, bitcoind and lightningd, through
the Scuttle testing crate. They need the same environment as Scuttle's own
integration tests: `CAPTAIND_EXEC`, `BARKD_EXEC`, `BARK_EXEC`,
`BARK_CLIENT_EXEC`, `WATCHMAND_EXEC`, `BITCOIND_EXEC`, `LIGHTNINGD_EXEC`,
`LIGHTNINGD_PLUGIN_DIR`, `POSTGRES_BINS` and `TEST_DIRECTORY` (see Scuttle's
`nix` shell and `justfile`).

```sh
cargo nextest run --features test-overrides --test integration --test-threads 6
```

The `test-overrides` feature lets the tests start the service with
`TestOverrides`, which no config can set and the service binary never
builds with. The tests that need S1 and S4 skip unless
`BULLNYM_TEST_S1_S4=1`, with `CAPTAIND_EXEC` and `BARKD_EXEC` pointing at a
release that has them. Without it, barkd ignores the hint request, and the
tests run the service with the override that accepts store invoices without
the Ark hint; one test runs it without the override and checks that the
sale is refused.
