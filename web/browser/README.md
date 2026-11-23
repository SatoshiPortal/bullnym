# Cashier HTML, CSS and journey audit

From `web/`, with Node.js 22+, npm, `zbarimg` and PostgreSQL server binaries discoverable through `pg_config`, run

```sh
npm ci
npx playwright install chromium firefox
npm run check
npm test -- --maxWorkers=2 --minWorkers=1
npm run build
AUDIT=1 RESULTS="$PWD/browser-results" npm run test:browser
```

The runner starts Vite preview on ephemeral loopback HTTP, launches both engines and closes the server afterward. Desktop is 1280 × 800 in light mode. Mobile is a 390 × 844 dark viewport. Specific cases also set 320 × 568, 568 × 320, light and dark hover, print media or 302 CSS pixels of paper width. `DIST_URL` selects another built bundle by absolute file URL. `ENGINES`, `SIZES` and `CASES` select comma-separated subsets. `FIREFOX_EXECUTABLE` and `CHROMIUM_EXECUTABLE` select an installed binary. To test a separately served loopback app, run `APP_ORIGIN=http://127.0.0.1:4173 node browser/run.mjs` with its own /pwa-test fixture if selecting PWA_REOPEN.

With `AUDIT=1`, each case writes a DOM inventory at checkpoints and completion. Each row records element order, parent, tag, direct text, classes, labels, disabled state, box, overflow, font, foreground and background. SCREEN_STATES and the targeted UX cases also save screenshots. Chromium saves a PDF from print media. KEYPAD_TYPE checks the original rendered type sizes for keypad numerals, the entry total, and open, pending and paid amounts. NARROW_CONTENT also checks that a long payment amount fits a 320-pixel screen and recovers its full size when space returns. The runner records a result per case and a Playwright trace on failure. To open a trace, run `npx playwright show-trace path/to/trace.zip`.

## State and transition table

| Screen and owner | Entry and transition | What appears and what works | Reload and return | Cases |
|---|---|---|---|---|
| Document and router | Open root, invalid path, store link or malformed fragment | Store-link guidance, retry or keypad. Browser title identifies store and screen. Hash navigation moves focus into the new screen. | URL selects store and route. Receipt Back remembers an actual in-app entry. | ROUTES, TYPE_AND_NAV, RECEIPT_BACK, CASHIER_BACK |
| Checkout and installed app | Store GET pending, available, missing or unavailable. Refresh on focus, visibility and reconnect. | Loading mark, store, missing-store explanation, retry. Installed shell reopens offline and requests current data when connectivity returns. | Shell survives installation. Store data comes from the service. | ROUTES, PWA_REOPEN, SCREEN_STATES |
| Price and draft | Cashier selects fiat or sats and keys digits, separator or zeros. Rate loads, refreshes, fails or changes currency. | Total, approximate conversion, memo, rate and Charge. A fiat draft and quote belong to their denomination. | Reload clears draft. Store currency comes back from service. | ENTRY, QUOTE, CURRENCY_CHANGE, TYPE_AND_NAV, KEYPAD_TYPE, NARROW_CONTENT |
| Sale creation | Submit once, service creates or refuses. Navigate while pending, retry after error. | Preparing disables the keypad, memo and repeated submission. Success enters payment and remembers the sale on this device. | Created sale survives. Late creation records its ID without redirecting a cashier who left. | CREATE_RACE, CREATE_ERRORS, TWO_TABS |
| Payment reader | Direct link, store redirect, missing ID, first read, long poll, offline, reconnect, changed sale ID. | Preparing, missing or reconnecting. Amount, memo and actions follow the accepted revision of this sale. | Refetch by ID. Reject wrong ID, stale revision and terminal reversal. | DIRECT, STALE, IDENTITY, RECEIPT_MERCHANT, SCREEN_STATES |
| Open payment | Sale below or at minimum, unpaid, partial, reopened after dropped or replaced transaction | Amount, waiting time, exact QR, invoice copy, partial amount and notices. The Bitcoin button or the store minimum appears above the QR. Cancellation is explicit. | QR method selection resets. Assigned address and current stage come from service. | PAYMENT, BITCOIN_OPTION, QR_SWITCH, DIALOG, CANCEL_FAILURE, NARROW_CONTENT |
| Bitcoin selector | Eligible open sale requests confirmation. Escape, Cancel, Continue, allocation failure, retry, concurrent assignment or settlement. | Native modal, named choices, busy buttons. Focus returns to the opener on dismissal. Matching QR and copy detail change together. | Pending dialog is transient. One permanent issued address per sale. | DIALOG, DIALOG_SETTLEMENT, DIALOG_OTHER_TAB, ADDRESS_REVISION, ACTION_RETURN, PAYMENT_FEEDBACK |
| QR rendering and clipboard | QR generation or clipboard succeeds or fails. Switch between Lightning and Bitcoin. | Code has an accessible name, copy feedback or selectable manual field. A QR failure allows copying. | Derived again from the current payment request. | QR_SWITCH, QR_FAILURE, OS_ACTIONS, COPY_LEGIBILITY, DARK_CONTROLS |
| Payment observed | Sufficient Bitcoin seen in mempool or Ark delivery pending. Observations or connectivity change. | Amber in-progress explanation, warnings and New Sale. Zero confirmed credit remains distinct from observation. | In-progress survives refresh and the invoice expiry fixture while delivery is pending. | PAYMENT, ARK_PENDING, PAYMENT_FEEDBACK, SCREEN_STATES |
| Sale ended | Paid, overpaid, canceled, expired or late payment. Paid rail may arrive later. | Paid heading and receipt, explicit overpayment, cancellation or late notice. Expiry returns to keypad. A partial expiry reports credit. | Service owns finality, credit, rail, late payment and receipt. | PAYMENT, TERMINALS, PARTIAL_EXPIRY, RECEIPT_REFRESH, RECEIPT_REVISION |
| Receipt | Direct, from history or from pay. Live, final, missing or retrying. | Merchant, date, stage, received, excess, sale total, note and reference. View payment reopens a live sale. Paid receipts expose Print and available Share. | Authoritative merchant follows the sale's store. Back restores the predecessor of an internal receipt or the checkout for a direct link. | RECEIPT_DETAILS, RECEIPT_ACTIONS, RECEIPT_BACK, RECEIPT_MERCHANT, DIRECT, CASHIER_HISTORY |
| OS output | Print, share accepted, share canceled, share failed or unavailable | Only the receipt prints. Native sharing uses its receipt URL. User cancellation leaves the receipt usable. | OS interaction state is transient. PDF preserves the memo and monetary labels. | SHARE_PRINT, OS_ACTIONS, RECEIPT_ACTIONS, RECEIPT_DETAILS |
| History gate | No cashier PIN, locked, typing, checking, refused, backoff, hourly limit, unlocked, removal or leave | Device-only explanation, masked count, keypad, waiting/recovery, local rows on success. Departing invalidates an outstanding check. | Lock on route exit, reload and document departure. Attempt budgets belong to the service. | HISTORY, LIMIT, PIN_BACKOFF, PIN_LEAVE, DARK_CONTROLS, SCREEN_STATES |
| History list | Unlock, load rows, partial failure, retry, external tab creates sale, empty store/profile | Amount, text status, date, known rail and memo. Payment warnings use cashier language. Tap a row to inspect its receipt. | Per-store sale IDs live in profile storage. With storage denied the page retains IDs until reload. | HISTORY, HISTORY_RETRY, STORAGE, TWO_TABS, CASHIER_HISTORY |
| Settings and brand | Tap Settings or navigate directly. Back to checkout. | Read-only store name, currency, Lightning address and on-chain minimum, owner-wallet explanation and Bull Bitcoin link. Back returns to the cashier screen that opened Settings, including an active QR. | Refetched store data. External logo link opens a new browsing context. | ROUTES, TYPE_AND_NAV, NARROW_CONTENT, SCREEN_STATES, BRAND_LINK |

## State combinations and cross-screen transitions

| Owner and entry | What the cashier sees and can do | Exit, refresh and persistence | Browser cases |
|---|---|---|---|
| Checkout metadata fetch fails after a successful visit | Keep the last store settings during retry. | Retry refreshes the name, currency and PIN configuration even when the cashier stays on the same screen. Reload starts a fresh fetch. | STORE_REFRESH_RETRY |
| Quote poll returns a different denomination while typing | Discard the incompatible quote immediately and refresh the checkout. A confirmed denomination change clears the fiat draft and preserves the memo. | The next matching quote enables a new amount. Drafts are document-local. | QUOTE_DENOMINATION_RACE, CURRENCY_CHANGE |
| A fiat quote converts to exactly half a sat | The keypad rounds up to the same whole sat as the service, including 0.15 USD at 80,000 USD/BTC. | Creating the fixture sale preserves the 188-sat total on Payment. | QUOTE_HALF_SAT |
| Sale entered in each supported currency | The keypad offers whole units for CRC and COP and cents for USD, CAD, EUR, ARS and MXN. Issued sale amounts retain their recorded fractional value across Payment, Receipt and History. | The service records the amount submitted. The register starts a fresh draft on return. | CURRENCY_SCALE, RECORDED_FIAT |
| Creation response is lost | The cashier sees that creation could not be confirmed and can check the owner’s sales before retrying. | A reconnect never repeats the POST. Without a returned ID the browser cannot recover the sale through local history. | CREATE_RESPONSE_LOST |
| Memo reaches the service's 200 Unicode character limit | A valid memo can be submitted intact. Adding another character gives an inline error and disables Charge. | Shortening the draft re-enables Charge. The accepted memo survives in the sale record. | MEMO_BOUNDARY |
| Open sale reaches its displayed deadline | Show that the cashier is waiting for a status update. Close an unsubmitted Bitcoin allocation dialog. The already-issued request stays available while the service reports open. | Only the service moves the sale to pending or a terminal state. Reload reads the sale and recomputes its countdown. | OPEN_DEADLINE, SALE_OPEN |
| Cancel submission or a concurrent Bitcoin allocation | Cancel in flight disables the new-address action. On a refused cancellation, the cashier can select Bitcoin again. A late error cannot overwrite an address already assigned by another request. | An ordinary cancellation returns to the keypad. A cancellation with observed funds stays on its outcome with separate received and unconfirmed amounts. | CANCEL_COMPETING, CANCEL_WITH_FUNDS, ALLOCATION_SUPERSEDED |
| Unlocked history remains open, refreshes, or reconnects | Refresh while visible each minute. Transient failures mark the list incomplete and retain known rows. A missing or foreign record is unavailable in this store. | Successful refresh clears the error. History and Receipt share accepted revisions for the same sale. The PIN gate still locks on exit or reload. | HISTORY_REFRESH, HISTORY_MISSING, HISTORY_SCOPE, HISTORY_REVISION |
| Native share is open on a paid receipt | Share is disabled during the OS operation. A failed share gives feedback. | Completion, cancellation or failure releases the action. Reload opens an idle receipt. | SHARE_PENDING, RECEIPT_ACTIONS |
| Confirmed and unconfirmed funds on the same sale | The receipt distinguishes current credit and unconfirmed funds. Partial and pending payment screens show both where present. A paid sale whose credit falls reports both the recorded sale total and current credit. | Each accepted service revision updates the amount. The sale stays paid after a reorganization. | SALE_OPEN, SALE_PENDING, SALE_PAID, SALE_ENDED |
| Paid on-chain address at its observation deadline | An eligible extra payment updates the receipt. At the boundary the recorded receipt remains final. | SEVEN_DAY_RECEIPT evaluates the production address-selection SQL in PostgreSQL one microsecond before, exactly at, and one microsecond after seven days. A fixture applies a 10,000-sat update to the selected sale and reloads each receipt. | SEVEN_DAY_RECEIPT |

The sale matrix uses 53 named records. Every record is opened in Payment, Receipt and PIN-unlocked Recent sales. The individual fixture names appear in the payment and receipt checkpoints.

| Case | Records | Combinations |
|---|---:|---|
| SALE_OPEN | 24 | 12 combinations of below, at or above the current threshold, assigned or unassigned address, and future or elapsed deadline. Another 12 combine no notice, dropped or replaced with no funds, unconfirmed only, received only or both. |
| SALE_PENDING | 11 | All eight subsets of low-fee, unconfirmed-parent and replacement warnings for a pending Bitcoin payment. Three pending Ark cases have zero, partial or full Bitcoin observation after invoice expiry. |
| SALE_PAID | 10 | Unknown, Lightning, Ark or Bitcoin method with exact credit or excess. Two Bitcoin records represent a reorganization leaving zero or partial credit. |
| SALE_ENDED | 8 | Expired or canceled crossed with no funds, unconfirmed only, received only or both. |

The sale matrix covers these equivalence classes. The other journey cases exercise the route, lifecycle, keyboard, failure, storage and network orders. The frontend receives sale status from the service. A frontend fixture can establish how a status renders and which actions it enables. Actual wallet settlement requires the separate regtest.

## Element review

| HTML or CSS surface | What was inspected |
|---|---|
| App document and shell | Language, viewport, manifest selection, service-worker shell, background, title, route focus, loading label, retry, guidance and external logo link. |
| Keypad | Store heading and truncation, history/settings icon names, active unit and keyboard movement, currency precision, live draft, numerals/backspace/zeros/decimal/empty PIN cell, memo label, rate/error copy, disabled versus ready Charge, scroll container and pinned footer. |
| Pay | Header actions, countdown, large values, line wrapping, long memo, urgency colors, amount due/received, option eligibility, QR quiet zone, image bounds and screenshot decoding, invoice/address detail and manual selection, modal names, focus and action states. |
| Pending and terminal | Spinner paths with reduced motion, check or warning icon, explicit outcome, warning text, pending rail, credited totals, return timer, New Sale, Receipt, overpayment and late credit. |
| History | Link visibility, PIN mask, disabled keys, refusal messages, local-data explanation, backlog/retry, row target and hover, paid status text, order, rail, memo, warnings, receipt and the trip back. |
| Receipt | Heading, identifiers, field/value associations, method availability, charged versus received, excess, note wrapping, live-payment link, share/print availability, dark preview, toolbar suppression, paper margins and long unbroken text. |
| Settings | Back, heading, field labels, long store name/address, threshold and wallet-management explanation in light/dark layouts. |
| Shared components | Text sizing and cascade, typography, light/dark variants, active versus disabled, hover contrast, hit areas, modal Escape, tab selection, status/error feedback, image alternatives and reduced motion. |

The footer link destination is inspected locally. The external Bull Bitcoin website, physical devices, printer hardware, native share recipients, Haptic/Audio output and screen-reader speech require their own environments. Endpoint states apart from the actual loopback offline fixture are deterministic API fixtures. A screenshot decode proves the UI emitted the asserted string. Wallet acceptance, account settlement and owner ledger credit require a separate live regtest.
