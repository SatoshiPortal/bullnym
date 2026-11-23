# bullnym

bullnym gives an Ark wallet a permanent Lightning Address and, optionally, a
store with a public checkout, on a Scuttle Ark server. Every payment goes
to the owner's wallet, which does not need to be online. The service holds
no funds and no private keys.

## How payers pay

| | Lightning Address | Store |
|---|---|---|
| Lightning | A barkd forwarding invoice that pays the registered Ark address | One barkd forwarding invoice that names the registered Ark address as its hint |
| Ark | LUD-22 `SCUTTLE`: the registered Ark address | Through the invoice's hint |
| Bitcoin | LUD-22 `SP`: the static Silent Payment address | On the merchant's request: an address per sale from the store's descriptor |
| Who starts a payment | Any payer, through LNURL | Anyone with the store's public link |

## How it works

- **The nym** is permanent and is the Lightning Address: `<nym>@<domain>`.
  The wallet registers it with a message signed by the Ark address that
  receives its payments.
- **The store** is a separate registration from the same wallet. It is made
  after the Lightning Address and uses the same Ark address. A nym has one
  store.
- **The public link:**
  - It is both the store's checkout and its cashier screen. Any device that
    opens it can take payments, with no pairing.
  - The store's history and CSV export are behind the owner's login.
  - A cashier screen lists the sales made on that device only after the
    owner's PIN.
- **Bitcoin on-chain:**
  - It is added to a sale only when the merchant asks for it, after a
    warning that it can take up to 60 minutes to confirm.
  - Each sale gets its own address from the store's descriptor. No address
    is reused, not even by another store with the same descriptor.
  - The owner's wallet scans the store account up to the store's next index,
    which the owner API returns.
  - A QR code never combines Lightning and Bitcoin.
- **Paid** means one of these:
  - barkd reports the invoice settled, whether it was paid over Lightning or
    by an Ark transfer delivered to the owner's mailbox;
  - an on-chain payment has 1 confirmation.

  Paid is final. A payment that has been seen but not counted shows as "in
  progress, not guaranteed", never as paid.

The API is documented in [docs/api.md](docs/api.md), and
`bullnym.default.toml` documents every setting.

## Requirements

- **A Scuttle Ark server:**
  - in expiry payout mode, with `ln_receive_ark_address_hints = true`;
  - on a release whose forwarded invoices can name their destination's Ark
    address as a hint, and whose receive status reports when an Ark payment
    has been delivered.

  The service refuses to start unless the server is in expiry payout mode.
  It refuses a sale whose invoice does not name the store's Ark address as
  its hint.
- **A private barkd on that server.** It issues the forwarding invoices and
  forwards the payments to their owners, and it needs no funds. Run it with
  `daemon_sync_interval_secs = 5` in its `config.toml`: a sale paid over Ark
  counts once barkd settles the receive, and barkd checks at that interval.
- **A bitcoind.** The service creates a watch-only wallet on it.
- **A Postgres database.**
- **Behind a reverse proxy,** set `[limits] trusted_proxies` to the proxy's
  addresses, so that rate limits apply to each client rather than to the
  proxy.

The Ark operator sets the Lightning receive fee, which forwarded receives
pay, and the server's limits. One barkd that makes every receive is a single
heavy client, so agree its traffic with the Ark operator.

## Build

```sh
(cd web && npm ci && npm run build)   # builds web/dist, which the binary embeds
cargo build --release
bullnym --config bullnym.toml start
```

`contrib/build-static.sh` builds a static x86_64 Linux binary in a Docker
container. The Scuttle crates come from the `SatoshiPortal/scuttle`
repository, at the release tag set in `Cargo.toml`.

## Tests

```sh
cargo test --lib                      # unit tests
(cd web && npm test && npm run check)
```

The integration tests (`tests/integration.rs`) run the service in the test
process, through the Scuttle testing crate, against a real Ark server,
barkd, bitcoind and lightningd. They need the same environment as Scuttle's
own integration tests (see Scuttle's `nix` shell and `justfile`):

- `CAPTAIND_EXEC`, `BARKD_EXEC`, `BARK_EXEC`, `BARK_CLIENT_EXEC` and
  `WATCHMAND_EXEC`;
- `BITCOIND_EXEC`, `LIGHTNINGD_EXEC` and `LIGHTNINGD_PLUGIN_DIR`;
- `POSTGRES_BINS` and `TEST_DIRECTORY`.

```sh
cargo nextest run --features test-overrides --test integration --test-threads 6
```

The `test-overrides` feature lets the tests start the service with
`TestOverrides`. No config can set them, and the service binary is never
built with them.

Some tests need a Scuttle release whose forwarded invoices carry hints and
whose receive status reports delivery.

- **With `BULLNYM_TEST_S1_S4=1`:** set `CAPTAIND_EXEC` and `BARKD_EXEC` to
  such a release, and those tests run.
- **Without it:**
  - those tests are skipped;
  - the other tests run the service with the override that accepts store
    invoices without the Ark hint;
  - one test runs the service without that override and checks that the
    sale is refused.

## License

MIT. See [LICENSE](LICENSE).

## Credits

bullnym's store, sale engine and cashier web app began as posd, the
point-of-sale service Lee Salminen wrote for Scuttle. Thank you, Lee.
[NOTICE](NOTICE) lists what came from posd.
