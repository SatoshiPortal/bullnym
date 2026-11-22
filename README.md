# bullnym

bullnym is a Lightning Address and store service for Ark wallets on a
Scuttle Ark server. Every payment goes to the owner's wallet, which does not
need to be online. The service holds no funds and no private keys.

It started from posd, Lee Salminen's point-of-sale service for Scuttle. See
[NOTICE](NOTICE).

The API is in [docs/api.md](docs/api.md). `bullnym.default.toml` documents
every setting.

## Build

```sh
(cd web && npm ci && npm run build)   # builds web/dist, which the binary embeds
cargo build --release
bullnym --config bullnym.toml start
```

The Scuttle crates come from the `SatoshiPortal/scuttle` repository at a
release tag, set in `Cargo.toml`.
