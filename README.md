# bullnym

bullnym is a Lightning Address and store service for Ark wallets on a
Scuttle Ark server. Every payment goes to the owner's wallet, which does not
need to be online. The service holds no funds and no private keys.

It started from posd, Lee Salminen's point-of-sale service for Scuttle. See
[NOTICE](NOTICE).

| | Lightning Address | Store |
|---|---|---|
| Lightning | A barkd forwarding invoice, paying the registered Ark address | One barkd forwarding invoice that names the registered Ark address as its hint |
| Ark | LUD-22 `ARK`: the registered Ark address | Through the invoice's hint |
| Bitcoin | LUD-22 `SP`: the static Silent Payment address | An address per sale from the store's descriptor |
| Who starts a payment | Any payer, through LNURL | Anyone with the store's cashier link, which is also its public checkout |

- **The nym** is permanent and is the Lightning Address: `<nym>@<domain>`.
  It is registered by the Ark address that receives its payments, with a
  message the wallet signs.
- **The store** is a separate registration from the same wallet, after the
  Lightning Address, with the same Ark address. A nym has one store.
- **The cashier link** replaces posd's paired terminals. Any device with it
  takes payments; the store's history and CSV are behind the owner login.
- **Bitcoin for the store** is an address per sale from the store's
  descriptor, as in posd: never reused. The owner's wallet scans the store
  account up to the store's next index, which the owner's store view shows.
- **Paid** means: Lightning delivered by barkd; Ark delivered to the owner's
  mailbox (S4); on-chain at 1 confirmation. A payment seen but not counted
  shows as in progress, not guaranteed, and never as paid.

The API is in [docs/api.md](docs/api.md). `bullnym.default.toml` documents
every setting.

## What the service needs

- A Scuttle Ark server in expiry payout mode, with
  `ln_receive_ark_address_hints = true`, and a release with S1 (a forwarded
  invoice that names its destination in its hint) and S4 (the delivery of
  Ark payments in the receive status). The service checks all of them at
  startup, through the server's public API, and refuses to start otherwise.
- A private barkd on that server. It issues the forwarding invoices and
  forwards the payments to the owners. It needs no funds.
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
cargo nextest run --test integration --test-threads 6
```

The tests that need S1 and S4 skip unless `BULLNYM_TEST_S1_S4=1`, with
`CAPTAIND_EXEC` and `BARKD_EXEC` pointing at a release that has them. Without
it, the service starts with `startup.allow_missing_s1_s4`, and store
invoices carry no hint.
