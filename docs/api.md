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
| 401 | The owner token is missing, not valid or expired |
| 404 | The resource does not exist, or the cashier link does not work |
| 409 | The request conflicts with the current state |
| 429 | A rate limit or cap was reached |
| 503 | `This payment method is temporarily unavailable. Try again later.` A backend is down, or no payment option is left |

## Signed messages

The owner's wallet signs with bark's message signing: BIP 340 Schnorr over
`SHA256("bark/message" || message)`, with the key of the Ark address that
receives the payments (`Wallet::sign_message`, or `POST
/api/v1/message/sign` on barkd). A message is a versioned header, then one
`field:value` line per field, joined by `\n`, with no trailing newline. A
field with a line break is refused, and the timestamp must be within 10
minutes of the service's clock.

The registered Ark address must be an address of the service's Ark server,
paying a public key, with delivery to the server mailbox. The service never
asks for the unblinded mailbox id.

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

### Lookup

`GET /api/v1/nym/<nym>` returns `{"nym", "registered", "available",
"lightning_address", "has_store", "checkout_url"}`, or `{"nym",
"registered": false, "available": false, "reason"}` for a name that cannot
be registered.

### LNURL (LUD-06, LUD-16, LUD-22)

- `GET /.well-known/lnurlp/<nym>` returns the pay request, with
  `"payment_methods": ["ARK", "SP"]`. `ARK` is left out while the Ark server
  is unhealthy.
- `GET /api/v1/lnurlp/<nym>/callback?amount=<msat>` returns `{"pr":
  "<invoice>", "routes": []}`: a barkd forwarding invoice without an Ark
  hint, paying the registered Ark address. Any Lightning wallet pays it;
  barkd forwards the payment to the owner.
- `…&payment_method=ARK` returns `{"ARK": {"address": "<registered Ark
  address>"}}`, and `…&payment_method=SP` returns `{"SP": {"address":
  "<Silent Payment address>"}}`. A method reply carries only that method
  and creates no invoice. `payment_method` may list several methods; the
  first one the service offers wins.

The Ark server cannot put a description hash in an invoice, so the invoice
carries the metadata's text description. Wallets that enforce the LUD-06
hash check refuse these invoices.

## Store

### Registration

`POST /api/v1/stores`, with no token. This is posd's registration with
`nym` added and `mailbox_id` removed:

```json
{
  "registration": {
    "name": "Café Uvita",
    "ark_address": "ark1…",
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
onchain_descriptor:<onchain_descriptor>
currency:<currency>
timestamp:<timestamp>
nym:<nym>
```

- The nym must exist (`404` otherwise) and `ark_address` must be its
  registered address.
- A nym has at most one store (`409`).
- The descriptor is public, ranged, single-path, without a hardened
  wildcard, with one extended key that no other store uses (`409`). It
  should come from an account used only by the store.
- The store's Lightning address is always `<nym>@<domain>`.

### Store

```json
{
  "name": "Café Uvita",
  "nym": "cafe",
  "lightning_address": "cafe@pay.example.com",
  "currency": "CRC",
  "onchain_min_sat": 50000
}
```

The owner's view adds `public_checkout`, `checkout_url` and `unlisted_link`
(whether one exists).

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
| `GET /api/v1/owner/nym/payments?before=<id>&limit=<n>` | | `{"payments": [...]}`: the Lightning payments to the Lightning Address |
| `GET /api/v1/owner/store` | | Owner store |
| `PATCH /api/v1/owner/store` | any of `name`, `currency`, `onchain_min_sat`, `public_checkout` | Owner store |
| `POST /api/v1/owner/store/unlisted-link` | | `{"url": "https://…/c/<secret>", "secret"}`; the previous link stops working |
| `DELETE /api/v1/owner/store/unlisted-link` | | `204` |
| `GET /api/v1/owner/sales?before=<id>&limit=<n>` | | `{"sales": [Sale]}`, newest first, at most 100 |
| `GET /api/v1/owner/sales.csv?from=<date>&to=<date>` | | CSV, one row per sale |

`from` and `to` are dates (`2026-11-20`, in UTC) or RFC 3339 times. A date
as `to` includes that whole day. The CSV columns are: `id, created_at,
paid_at, state, paid_rail, amount_sat, received_sat, fiat_amount, currency,
btc_price, memo, link, cashier_name, seen_sat, progress, warnings, notice,
final_at, late_payment`.

The web app's `/owner#token=<owner token>` shows the history, the CSV export
and the cashier links.

## Cashier link and public checkout

Any device with a store's link acts as a cashier. There is no pairing, no
device token and no login. The same page is the public checkout.

- Public link: `https://<host>/<nym>`, open to anyone unless the owner turns
  it off. API prefix `/api/v1/checkout/<nym>`.
- Unlisted link: `https://<host>/c/<secret>`, which the owner shares with
  staff and can replace. API prefix `/api/v1/links/<secret>`.

| Method and path | Body | Returns |
|---|---|---|
| `GET <prefix>` | | `{"store": Store, "link": "public"\|"unlisted"}`. Viewing allocates nothing |
| `GET <prefix>/rate` | | `{"currency", "btc_price", "fetched_at"}` |
| `POST <prefix>/sales` | `{"fiat_amount": "12.50"}` or `{"amount_sat": 35000}`, with optional `memo` (200 characters) and `cashier_name` (64 characters, never verified) | `Sale` |
| `GET /api/v1/sales/<id>` | | `Sale` |
| `GET /api/v1/sales/<id>?wait=<state>` | | `Sale`, when the state is not `<state>` or after 25 s |
| `POST /api/v1/sales/<id>/cancel` | | `Sale`; `409` unless the sale is open and no payment is arriving |

The link cannot list the store's sales, export them or see other sales.
Holding a sale's unguessable id is what lets a cashier follow and cancel it.

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
  "payment_request": "bitcoin:bc1p…?amount=0.0006&lightning=lnbc…",
  "lightning_invoice": "lnbc…",
  "onchain_address": "bc1p…",
  "paid_at": null,
  "paid_rail": null,
  "received_sat": 0,
  "seen_sat": 60000,
  "progress": "onchain",
  "warnings": ["not-guaranteed", "low-fee"],
  "notice": null,
  "final": false,
  "final_at": null,
  "late_payment": false,
  "link": "public",
  "cashier_name": "Front till"
}
```

- `payment_request` is one BIP 321 URI. Its `lightning` invoice is a barkd
  forwarding invoice that names the owner's registered Ark address as its
  route hint (S1): a Lightning wallet pays it over Lightning, a Scuttle
  wallet with address hints on pays it over Ark. The address and `amount`
  are left out below the store's on-chain minimum: `bitcoin:?lightning=…`.
  A rail whose backend is down is left out.
- `state`:
  - `open`: waiting for a payment.
  - `in-progress`: a payment is seen and does not count yet. `progress` is
    `onchain` (in the mempool: not guaranteed until 1 confirmation) or `ark`
    (the Ark server settled the invoice and has not reported the coins
    delivered to the owner, S4). It cannot be canceled, and it stays open
    past `expires_at` until the payment confirms or explicitly goes away.
  - `paid`: Lightning delivered by barkd, Ark delivered (S4), or on-chain
    with 1 confirmation. `final` is true at once for an invoice, and at 3
    confirmations on-chain.
  - `payment-issue`: a counted on-chain payment went away in a reorg. It
    returns to `paid` when it confirms again.
  - `expired` or `canceled`. A payment after that is a late payment
    (`late_payment`): it shows on this sale and never pays another one.
- `received_sat` counts only delivered invoices and confirmed outputs;
  `seen_sat` is the unconfirmed amount, never counted. Nothing sets the
  paid time or rail before a payment counts, and nothing marks a sale paid
  by hand.
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
