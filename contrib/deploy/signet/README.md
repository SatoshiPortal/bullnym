# Private signet deployment

One Debian 13 VM runs a private custom signet, a Scuttle Ark server and
bullnym, for testing the Lightning Address, the store and wallets end to
end. These scripts started with posd's signet deployment. To try a
deployment without access to its VM, use `wallets.sh` (see "Trying it"
below). `<host>` stands for the deployment's public host name.

| Service | Unit | Address |
|---|---|---|
| bitcoind 32.1, custom signet | `scuttle-bitcoind` | RPC 127.0.0.1:38332, no peers |
| Block miner, every 10 minutes | `scuttle-miner.timer` | |
| Postgres 17 (`captaind`, `bullnym`) | `postgresql` | 127.0.0.1:5432 |
| CLN with the hold plugin (the Ark server's node) | `scuttle-cln-internal` | p2p 9735, gRPC 9736, hold 9737 |
| CLN without plugins (an outside payer) | `scuttle-cln-external` | p2p 9745, gRPC 9746 |
| captaind (expiry payouts, address hints, zero fees), a release with S1 and S4 | `scuttle-captaind` | gRPC 0.0.0.0:3535 |
| watchmand | `scuttle-watchmand` | |
| Forwarder barkd | `scuttle-barkd` | 127.0.0.1:3000 |
| bullnym and its cashier and checkout app | `scuttle-bullnym` | 127.0.0.1:3080 |
| electrs (Esplora API) | `scuttle-electrs` | 127.0.0.1:3002 |

## The chain

- **The signet challenge is `OP_TRUE` (`51`).** Any block with valid proof of
  work is valid. bitcoind does not listen for peers, so only this VM mines.
- **Blocks come every 10 minutes.** Signet retargets every 2,016 blocks like
  mainnet. Faster blocks would raise the difficulty fourfold every period,
  until a CPU can no longer keep up. At 10 minutes the difficulty stays at
  signet's minimum, and a block takes about a second to mine.
- **Block heights match mainnet.** The Ark settings are in blocks, so the
  production values behave as they would on mainnet. A VTXO lives 4,320
  blocks, which is 30 days.
- **Every block carries a few fee-paying transactions.** Without them,
  bitcoind has no fee history, `estimatesmartfee` returns nothing, and CLN
  refuses to open channels. Both CLN nodes also set `force-feerates=2000`.
- **Mine extra blocks** with `sudo -u scuttle /opt/scuttle/bin/signet-mine N`.
  Many quick blocks raise the difficulty at the next retarget, so mine sparingly.

## Build, set up, fund, check

On the VM, from a checkout of this repository, with the Scuttle release
binaries (`captaind`, `watchmand`, `barkd`, `bark`) in a directory and the
bullnym binary built (`contrib/build-static.sh`, or `cargo build --release`
after building `web/dist`):

```sh
export SCUTTLE_BIN=~/scuttle-bin BULLNYM_BIN=target/static/bullnym
./contrib/deploy/signet/setup.sh    # installs binaries, configs and units; safe to rerun
./contrib/deploy/signet/fund.sh     # funds captaind and opens a 2 BTC channel; safe to rerun
./contrib/deploy/signet/smoke.sh    # Lightning, Ark and on-chain sales and the Lightning Address
./contrib/deploy/signet/public.sh   # Tailscale Funnel: bullnym, Ark server, Esplora
```

To update a deployment, run `setup.sh` again with the new binaries, then
restart the units it changed (`sudo systemctl restart scuttle-captaind
scuttle-bullnym …`).

## Secrets and data

- **`/etc/scuttle/secrets.env`** holds the generated bitcoind and Postgres
  passwords. It is created once and readable only by root and the `scuttle`
  group.
- **Configs** are in `/etc/scuttle/`. Data is in `/var/lib/scuttle/<service>`,
  owned by the `scuttle` user.
- **Seeds:** the captaind seed is `/var/lib/scuttle/captaind/mnemonic`, and
  watchmand has a copy of it.
- **Smoke test wallets** are in `~/smoke` of the user that ran it.

## Reaching it

`public.sh` publishes three services on the internet with Tailscale Funnel:

| URL | Service |
|---|---|
| https://&lt;host&gt; | bullnym: cashier and checkout app, API, Lightning Address |
| https://&lt;host&gt;:8443 | the Ark server (captaind gRPC) for wallets |
| https://&lt;host&gt;:10000 | Esplora (electrs) |

A wallet uses `--signet --ark https://…:8443 --esplora https://…:10000`.

- **Ports:** Funnel allows only 443, 8443 and 10000.
- **gRPC on 8443:** gRPC needs HTTP/2 negotiated in TLS (ALPN). Funnel's TLS
  termination does not negotiate it and cannot proxy HTTP/2 to a backend.
  So 8443 is raw TCP to nginx on 127.0.0.1:8444, which serves the node's
  Tailscale certificate and passes gRPC to captaind. A weekly timer
  (`scuttle-renew-cert`) renews the certificate.
- **electrs** imports blocks over RPC (`--jsonrpc-import`). It cannot read
  bitcoind's block files, because a custom signet has its own network magic.
- **Test helpers:** `/dev/` on the bullnym host, from `devtools.py`, gives
  faucet coins, mines blocks, creates a store's on-chain wallet and gives a
  test Silent Payment address. Each has a cap and a rate limit.

## Trying it

`wallets.sh` runs on your own machine and needs only `bark` (the Scuttle
release), `curl` and `jq`. It drives an owner's wallet, a customer's wallet,
the owner API and the cashier link. Run it with no arguments for the full
command list.

```sh
export SIGNET_HOST=<host>
alias sw=./contrib/deploy/signet/wallets.sh
sw create owner; sw create customer
sw faucet customer 0.05; sw board customer
sw nym mycafe                          # mycafe@<host>
sw register "My Cafe" CRC              # the store, at https://<host>/mycafe
sw sell 6250 "2 coffees"; sw pay customer; sw wait
```

## Known limits

- **The Bull app as it ships cannot use this chain.** It runs upstream bark
  from pub.dev. Its Esplora setting can point here once it runs Scuttle.
- **The Lightning network is just these two CLN nodes.**
