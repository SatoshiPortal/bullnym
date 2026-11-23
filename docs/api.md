# bullnym HTTP API

bullnym serves the Lightning Address of a nym, the store tied to it, and the
web app that is both the store's cashier screen and its public checkout. All
bodies are JSON. Enum values use kebab-case. Amounts in sats are integers.
Fiat amounts are decimal strings in the store currency, for example
`"12.50"`. Times are RFC 3339 strings in UTC.

Errors return a non-2xx status with `{"error": "<message>"}`, except LNURL
endpoints, which use `{"status": "ERROR", "reason": "<message>"}`:

| Status | Meaning |
|---|---|
| 400 | The request is not valid |
| 401 | The owner token is missing, not valid or expired; or a wrong cashier PIN |
| 404 | The resource does not exist, or the cashier link does not work |
| 409 | The request conflicts with the current state |
| 429 | A rate limit or cap was reached, or wrong cashier PINs locked the check |
| 503 | `This payment method is temporarily unavailable. Try again later.` A backend is down, or no payment option is left. Or a message saying what to change, when the setup refuses the request (see Sale) |

Amounts are at most 2,100,000,000,000,000 sats (21,000,000 BTC), and a fiat
amount is below 10^15 units: larger ones are refused (`400`) before anything
is allocated.

## Signed messages

The owner's wallet signs with bark's message signing: BIP 340 Schnorr over
`SHA256("bark/message" || message)`, with the key of the Ark address that
receives the payments (`Wallet::sign_message`, or `POST
/api/v1/message/sign` on barkd). A message is a versioned header, then one
`field:value` line per field, joined by `\n`, with no trailing newline. A
field with a line break is refused, and the timestamp must be within 10
minutes of the service's clock.

The registered Ark address must be one the Ark server accepts as the
destination hint of a forwarded invoice: an address of the service's Ark
server and network (`ark1…` on bitcoin, `tark1…` on the test networks),
paying a public key, with exactly one delivery, to the server mailbox. The
server also unblinds the mailbox with its own key, which the service cannot
check.

## Lightning Address

### Registration

`POST /api/v1/nym`, with no token:

```json
{
  "registration": {
    "nym": "cafe",
    "ark_address": "ark1…",
    "silent_payment_address": "sp1…",
    "timestamp": 1795190400
  },
  "signature": "<hex>"
}
```

The signed message:

```text
bullnym registration v1
nym:<nym>
ark_address:<ark_address>
silent_payment_address:<silent_payment_address>
timestamp:<timestamp>
```

- A nym has 1 to 32 lowercase letters, digits and inner hyphens. Route and
  brand names are reserved, plus the operator's list.
- The Silent Payment address must be a version 0 address for the network
  (`sp` or `tsp`).
- One nym per Ark address and one Ark address per nym (`409` otherwise).
- A nym is permanent: it can never be renamed, re-pointed, released or
  deleted. Only its display name changes.

The response is `{"nym": Nym}`.

### LNURL (LUD-06, LUD-16, LUD-22)

- `GET /.well-known/lnurlp/<nym>` returns the pay request, with
  `"payment_methods": ["SCUTTLE", "SP"]`. `SCUTTLE` is left out while the
  Ark server is unhealthy.
- `GET /api/v1/lnurlp/<nym>/callback?amount=<msat>`, with no
  `payment_method`, returns `{"pr": "<invoice>", "routes": []}`: a barkd
  forwarding invoice without an Ark hint, paying the registered Ark address.
  Any Lightning wallet pays it; barkd forwards the payment to the owner.
- `…&payment_method=SCUTTLE` returns `{"SCUTTLE": {"address": "<registered
  Ark address>"}}`, and `…&payment_method=SP` returns `{"SP": {"address":
  "<Silent Payment address>"}}`. `…&payment_method=SCUTTLE,SP` returns both
  objects. A reply carries only the requested methods, and no `pr`, and
  creates no invoice. A method whose backend is down is left out of the
  reply, and `503` comes when none is left.
- An unknown method, anywhere in the list, is refused with `400` and
  `"reason": "unsupported payment method <m>; this Lightning Address offers
  SCUTTLE and SP"`. There is no `ARK` alias.

The Ark server cannot put a description hash in an invoice, so the invoice
carries the metadata's text description. Wallets that enforce the LUD-06
hash check refuse these invoices.

## Store

### Registration

`POST /api/v1/stores`, with no token. This is posd's registration, with
`nym` added last:

```json
{
  "registration": {
    "name": "Café Uvita",
    "ark_address": "ark1…",
    "mailbox_id": "02…",
    "onchain_descriptor": "tr([fingerprint/86'/0'/0']xpub…/0/*)",
    "currency": "CRC",
    "timestamp": 1795190400,
    "nym": "cafe"
  },
  "signature": "<hex>"
}
```

```text
scuttle-pos registration v1
name:<name>
ark_address:<ark_address>
mailbox_id:<mailbox_id>
onchain_descriptor:<onchain_descriptor>
currency:<currency>
timestamp:<timestamp>
nym:<nym>
```

- The nym must exist (`404` otherwise) and `ark_address` must be its
  registered address.
- `mailbox_id` is the unblinded mailbox of the owner's wallet, as in posd
  (`bark owner debug-info`, or `Wallet::mailbox_identifier()`). It must
  parse as a mailbox id. The service stores it with the registration and
  uses it for nothing; as in posd, it does not check that it belongs to the
  address.
- A nym has at most one store (`409`).
- `onchain_descriptor` is the account the wallet chose. The service checks
  only what posd checks: it is a public descriptor, ranged, single-path,
  without a hardened wildcard. Refused: a private key, a fixed index, a
  multipath `<0;1>` step, a `*'` wildcard. Accepted without any other
  check: the network of the key (a `xpub` gives addresses of the service's
  network), the script type, and whether the wallet uses the account for
  anything else. Two stores may register the same descriptor (see Sale).
- A descriptor without an address form, such as a bare `pk(…)` or
  `multi(…)`, is accepted as in posd, but no address can be derived from it,
  not even the first. Adding Bitcoin to its sales then fails with `503`,
  the sale keeps its invoice, and the index stays free; posd refused such a
  sale.
- The store's Lightning address is always `<nym>@<domain>`.

### Store

```json
{
  "name": "Café Uvita",
  "nym": "cafe",
  "lightning_address": "cafe@pay.example.com",
  "currency": "CRC",
  "onchain_min_sat": 50000,
  "cashier_pin_set": true
}
```

`cashier_pin_set` says whether the owner set a cashier PIN; the PIN and its
hash are never shown.

The owner's view adds `checkout_url`, the store's cashier link, and
`next_onchain_index`: each sale that offers Bitcoin takes the store's next
derivation index, as in posd, and no address is handed out twice. When
another store registered the same descriptor, an address that store's sale
already has is skipped and its index used up. The owner's wallet scans the
store account up to this index, so a payment after many empty addresses is
still found.

## Owner

1. `POST /api/v1/owner/challenge` with `{"ark_address": "ark1…"}` returns
   `{"challenge": "<text>"}`, single-use, valid for 5 minutes.
2. `POST /api/v1/owner/login` with `{"ark_address", "challenge",
   "signature"}` returns `{"token", "expires_at"}`. The token lasts 24 hours;
   only its hash is stored.

The owner sends `Authorization: Bearer <owner token>`. The same login
serves the nym and its store.

| Method and path | Body | Returns |
|---|---|---|
| `GET /api/v1/owner/nym` | | `{"nym": Nym}` |
| `PATCH /api/v1/owner/nym` | `display_name` (empty removes it) | `{"nym": Nym}` |
| `GET /api/v1/owner/store` | | Owner store |
| `PATCH /api/v1/owner/store` | any of `name`, `currency`, `onchain_min_sat` (at most 2,100,000,000,000,000) | Owner store |
| `PUT /api/v1/owner/store/cashier-pin` | `{"pin": "<4 to 12 digits>"}` | Owner store. Sets or changes the cashier PIN |
| `GET /api/v1/owner/sales?before=<id>&limit=<n>` | | `{"sales": [Sale]}`, newest first, at most 100 |
| `GET /api/v1/owner/sales.csv?from=<date>&to=<date>` | | CSV, one row per sale |

`from` and `to` are dates (`2026-11-20`, in UTC) or RFC 3339 times. A date
as `to` includes that whole day. The CSV columns are: `id, created_at,
paid_at, state, paid_rail, amount_sat, received_sat, fiat_amount, currency,
btc_price, memo, seen_sat, progress, warnings, notice, late_payment`.

The owner's wallet calls these endpoints; the service has no owner page.

## Cashier link and public checkout

A store has one cashier link, `https://<host>/<nym>`, always on and open to
anyone. Any device with it acts as a cashier: there is no pairing, no device
token and no login. The same page is the public checkout. Its API prefix is
`/api/v1/checkout/<nym>`.

| Method and path | Body | Returns |
|---|---|---|
| `GET <prefix>` | | `{"store": Store}`. Viewing allocates nothing |
| `GET <prefix>/rate` | | `{"currency", "btc_price", "fetched_at"}` |
| `POST <prefix>/sales` | `{"fiat_amount": "12.50"}` or `{"amount_sat": 35000}`, with optional `memo` (200 characters) | `Sale` |
| `GET /api/v1/sales/<id>` | | `Sale` |
| `GET /api/v1/sales/<id>?wait=<state>` | | `Sale`, when the state is not `<state>` or after 25 s |
| `POST /api/v1/sales/<id>/onchain` | | `Sale` with a Bitcoin address; see below |
| `POST /api/v1/sales/<id>/cancel` | | `Sale`; `409` unless the sale is open and no payment is arriving |
| `POST <prefix>/pin` | `{"pin": "…"}` | `{"ok": true}` when it is the cashier PIN |

The link cannot list the store's sales, export them or see other sales.
Holding a sale's unguessable id is what lets a cashier follow it, add
Bitcoin to it and cancel it. Selling, following, adding Bitcoin and
canceling need no PIN.

### Bitcoin on request

A sale is created with its Lightning invoice only: the service never gives
a sale a Bitcoin address unless the merchant asks for it. The cashier
screen asks first, with a warning that an on-chain payment can take up to
60 minutes to confirm and is not complete until it is.
`POST /api/v1/sales/<id>/onchain` then adds the address:

- The sale must be open (`409` otherwise), at or above the store's
  `onchain_min_sat` (`400`), and bitcoind healthy (`503`).
- The address is the store's next one, as in posd: no address is handed out
  twice, even to another store with the same descriptor.
- A second call returns the same address: a sale never takes a second
  index.
- The invoice stays payable, and either rail pays the sale.

### Cashier PIN

The cashier screen keeps the ids of the sales it created, and lists them
only after the service accepted the cashier PIN typed on its keypad. Without
a PIN set (`cashier_pin_set` false) the list stays hidden and the check
returns `404`. The owner sets the PIN; the service keeps only its Argon2id
hash with a random salt, and the answer to a check says only whether it
matched:

- A wrong PIN returns `401` with `{"error": "wrong PIN", "retry_after":
  <seconds>}`.
- Wrong PINs are counted per source and per store. Five are free; then each
  wrong PIN locks further checks for twice as long as the one before (2 s,
  4 s, …), up to 15 minutes. While locked, a check returns `429` with
  `retry_after` and a `Retry-After` header, and the PIN is not checked. A
  right PIN clears no count. A count is forgotten a day after its last wrong
  PIN, and a store's count when the owner sets a new PIN.
- Checks are also limited per source per hour (`pin_checks_per_hour`, 10).

The list is a convenience of the screen: anyone with the browser's storage
can read the remembered sale ids, and a sale id alone opens its sale.

### Sale

```json
{
  "id": "4f1c9a2e…",
  "created_at": "2026-11-20T15:04:05Z",
  "expires_at": "2026-11-20T15:14:05Z",
  "state": "in-progress",
  "amount_sat": 60000,
  "fiat_amount": "37.50",
  "currency": "CRC",
  "btc_price": "62500000.00",
  "memo": "2 coffees",
  "lightning_request": "lightning:lnbc…",
  "onchain_request": "bitcoin:bc1p…?amount=0.0006",
  "lightning_invoice": "lnbc…",
  "onchain_address": "bc1p…",
  "paid_at": null,
  "paid_rail": null,
  "updated_at": "2026-11-20T15:06:40Z",
  "received_sat": 0,
  "seen_sat": 60000,
  "progress": "onchain",
  "warnings": ["not-guaranteed", "low-fee"],
  "notice": null,
  "late_payment": false
}
```

- A sale has two QR texts, and no QR combines Lightning and Bitcoin:
  - `lightning_request` is `lightning:<invoice>`. The invoice is a barkd
    forwarding invoice that names the owner's registered Ark address as its
    route hint (S1): a Lightning wallet pays it over Lightning, a Scuttle
    wallet with address hints on pays it over Ark.
  - `onchain_request` is `bitcoin:<address>?amount=<btc>`, and `null` until
    Bitcoin is added.
- Without an invoice, because barkd or the Ark server is down, the sale is
  refused with the fixed `503`. Until its invoice is stored a sale is only
  reserved: no route shows it, nor the owner's history or CSV, and nothing
  of it remains when the invoice fails.
- `updated_at` is when the sale last changed. A client keeps the newer of
  two snapshots, and never shows an ended sale as live again.
- As in posd, each sale checks that its invoice names the store's Ark
  address as its hint. An invoice without it refuses the sale with `503`
  and `"the Ark server must allow Ark address hints: barkd returned the
  sale's invoice without the store's Ark address in its route hints"`; an
  invoice naming another address is refused the same way. The invoice is
  canceled in barkd, and the sale takes no address.
- Every on-chain address belongs to one sale, even when two stores
  registered the same descriptor, so a payment to it counts for that sale
  only.
- `state`:
  - `open`: waiting for a payment.
  - `in-progress`: a payment is seen and does not count yet. `progress` is
    `onchain` (in the mempool: not guaranteed until 1 confirmation) or `ark`
    (an Ark transfer settled the invoice, and barkd shows it awaiting
    delivery, S4). It cannot be canceled, and it stays open past
    `expires_at` until the payment confirms or explicitly goes away. An Ark
    payment stays `in-progress` until barkd settles it, with no deadline.
  - `paid`: an invoice barkd reports settled, or on-chain with 1
    confirmation. barkd is the one source of truth for the invoice: the
    paid state is recorded first, and the Ark server labels the rail after,
    `lightning` if it granted HTLCs, `ark` if not. `paid_rail` is `null`
    until then, and the label is retried for 7 days when the server cannot
    be read. Paid is final, as in posd: the sale stays paid, and an output
    that confirmed stays credited, whatever its address shows later.
  - `expired` or `canceled`. A payment after that is a late payment
    (`late_payment`): it shows on this sale and never pays another one,
    because no address is handed out twice. An open sale past its expiry
    for which barkd showed a payment arriving or awaiting delivery never
    expires on a failed read or a restart, only on barkd's answer that the
    invoice ended unpaid.
- As in posd, a sale's address is watched for 7 days after the sale was
  created, whatever its state: an extra payment to a paid sale counts in
  its `received_sat`, above `amount_sat` (an overpayment), and a payment
  to an ended sale is a late payment.
- `received_sat` counts only delivered invoices and confirmed outputs;
  `seen_sat` is the unconfirmed amount, never counted. Nothing sets the
  paid time or rail before a payment counts, and nothing marks a sale paid
  by hand. A new unconfirmed payment shows only once bitcoind answered that
  it is in its mempool: while the mempool query fails, nothing is recorded
  for it.
- `warnings`: `not-guaranteed` (always, while on-chain in progress),
  `low-fee` (well below bitcoind's estimate), `unconfirmed-parents` (spends
  unconfirmed outputs), `replaced` (a transaction that does not pay the
  sale replaced one that did).
- `notice`: `payment-replaced` or `payment-dropped` when an in-progress
  payment went away and the sale returned to open or expired.
- `paid_rail`: `lightning`, `ark` or `onchain`.

## Limits

Per-source limits count IPv4 callers by address and IPv6 callers by /56.
`X-Forwarded-For` is read only from the configured trusted proxies. The
limits and caps are in `bullnym.default.toml`.
