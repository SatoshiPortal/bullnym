# Cashier HTML, CSS and journey audit

From `web/`, with Node.js 22+, npm and `zbarimg` on PATH, run

```sh
npm ci
npx playwright install chromium firefox
npm run check
npm test -- --maxWorkers=2 --minWorkers=1
npm run build
AUDIT=1 RESULTS="$PWD/browser-results" npm run test:browser
```

The runner starts Vite preview on ephemeral loopback HTTP, launches both engines and closes the server afterward. Desktop is 1280 × 800 in light mode. Mobile is a 390 × 844 dark viewport. Specific cases also set 320 × 568, 568 × 320, light and dark hover, print media or 302 CSS pixels of paper width. `DIST_URL` selects another built bundle by absolute file URL. `ENGINES`, `SIZES` and `CASES` select comma-separated subsets. `FIREFOX_EXECUTABLE` and `CHROMIUM_EXECUTABLE` select an installed binary. To test a separately served loopback app, run `APP_ORIGIN=http://127.0.0.1:4173 node browser/run.mjs` with its own /pwa-test fixture if selecting PWA_REOPEN.

With `AUDIT=1`, each case writes a DOM inventory at checkpoints and completion. Each row records element order, parent, tag, direct text, classes, labels, disabled state, box, overflow, font, foreground and background. SCREEN_STATES and the targeted UX cases also save screenshots. Chromium saves a PDF from print media. The runner records a result per case and a Playwright trace on failure. To open a trace, run `npx playwright show-trace path/to/trace.zip`.

## State and transition table

| Screen and owner | Entry and transition | What appears and what works | Reload and return | Cases |
|---|---|---|---|---|
| Document and router | Open root, invalid path, store link or malformed fragment | Store-link guidance, retry or keypad. Browser title identifies store and screen. Hash navigation moves focus into the new screen. | URL selects store and route. Receipt Back remembers an actual in-app entry. | ROUTES, TYPE_AND_NAV, RECEIPT_BACK |
| Checkout and installed app | Store GET pending, available, missing or unavailable. Refresh on focus, visibility and reconnect. | Loading mark, store, missing-store explanation, retry. Installed shell reopens offline and requests current data when connectivity returns. | Shell survives installation. Store data comes from the service. | ROUTES, PWA_REOPEN, SCREEN_STATES |
| Price and draft | Cashier selects fiat or sats and keys digits, separator or zeros. Rate loads, refreshes, fails or changes currency. | Total, approximate conversion, memo, rate and Charge. A fiat draft and quote belong to their denomination. | Reload clears draft. Store currency comes back from service. | ENTRY, QUOTE, CURRENCY_CHANGE, TYPE_AND_NAV, KEYPAD_TYPE, NARROW_CONTENT |
| Sale creation | Submit once, service creates or refuses. Navigate while pending, retry after error. | Preparing disables the keypad, memo and repeated submission. Success enters payment and remembers the sale on this device. | Created sale survives. Late creation records its ID without redirecting a cashier who left. | CREATE_RACE, CREATE_ERRORS, TWO_TABS |
| Payment reader | Direct link, store redirect, missing ID, first read, long poll, offline, reconnect, changed sale ID. | Preparing, missing or reconnecting. Amount, memo and actions follow the accepted revision of this sale. | Refetch by ID. Reject wrong ID, stale revision and terminal reversal. | DIRECT, STALE, IDENTITY, RECEIPT_MERCHANT, SCREEN_STATES |
| Open payment | Sale below or at minimum, unpaid, partial, reopened after dropped or replaced transaction | Amount, waiting time, exact QR, invoice copy, partial amount and notices. Cancellation is explicit. | QR method selection resets. Assigned address and current stage come from service. | PAYMENT, QR_SWITCH, DIALOG, CANCEL_FAILURE, NARROW_CONTENT |
| Bitcoin selector | Eligible open sale requests confirmation. Escape, Cancel, Continue, allocation failure, retry, concurrent assignment or settlement. | Native modal, named choices, busy buttons. Focus returns to the opener on dismissal. Matching QR and copy detail change together. | Pending dialog is transient. One permanent issued address per sale. | DIALOG, DIALOG_SETTLEMENT, DIALOG_OTHER_TAB, ADDRESS_REVISION, ACTION_RETURN, PAYMENT_FEEDBACK |
| QR rendering and clipboard | QR generation or clipboard succeeds or fails. Switch between Lightning and Bitcoin. | Code has an accessible name, copy feedback or selectable manual field. A QR failure allows copying. | Derived again from the current payment request. | QR_SWITCH, QR_FAILURE, OS_ACTIONS, COPY_LEGIBILITY, DARK_CONTROLS |
| Payment observed | Sufficient Bitcoin seen in mempool or Ark delivery pending. Observations or connectivity change. | Amber in-progress explanation, warnings and New Sale. Zero confirmed credit remains distinct from observation. | In-progress survives refresh and the invoice expiry fixture while delivery is pending. | PAYMENT, ARK_PENDING, PAYMENT_FEEDBACK, SCREEN_STATES |
| Sale ended | Paid, overpaid, canceled, expired or late payment. Paid rail may arrive later. | Paid heading and receipt, explicit overpayment, cancellation or late notice. Expiry returns to keypad. A partial expiry reports credit. | Service owns finality, credit, rail, late payment and receipt. | PAYMENT, TERMINALS, PARTIAL_EXPIRY, RECEIPT_REFRESH, RECEIPT_REVISION |
| Receipt | Direct, from history or from pay. Live, final, missing or retrying. | Merchant, date, stage, received, excess, sale total, note and reference. View payment reopens a live sale. Paid receipts expose Print and available Share. | Authoritative merchant follows the sale's store. Back restores the predecessor of an internal receipt or the checkout for a direct link. | RECEIPT_DETAILS, RECEIPT_ACTIONS, RECEIPT_BACK, RECEIPT_MERCHANT, DIRECT, CASHIER_HISTORY |
| OS output | Print, share accepted, share canceled, share failed or unavailable | Only the receipt prints. Native sharing uses its receipt URL. User cancellation leaves the receipt usable. | OS interaction state is transient. PDF preserves the memo and monetary labels. | SHARE_PRINT, OS_ACTIONS, RECEIPT_ACTIONS, RECEIPT_DETAILS |
| History gate | No cashier PIN, locked, typing, checking, refused, backoff, hourly limit, unlocked, removal or leave | Device-only explanation, masked count, keypad, waiting/recovery, local rows on success. Departing invalidates an outstanding check. | Lock on route exit, reload and document departure. Attempt budgets belong to the service. | HISTORY, LIMIT, PIN_BACKOFF, PIN_LEAVE, DARK_CONTROLS, SCREEN_STATES |
| History list | Unlock, load rows, partial failure, retry, external tab creates sale, empty store/profile | Amount, text status, date, known rail and memo. Payment warnings use cashier language. Tap a row to inspect its receipt. | Per-store sale IDs live in profile storage. With storage denied the page retains IDs until reload. | HISTORY, HISTORY_RETRY, STORAGE, TWO_TABS, CASHIER_HISTORY |
| Settings and brand | Tap Settings or navigate directly. Back to checkout. | Read-only store name, currency, Lightning address and on-chain minimum, owner-wallet explanation and Bull Bitcoin link. | Refetched store data. External logo link opens a new browsing context. | ROUTES, TYPE_AND_NAV, NARROW_CONTENT, SCREEN_STATES |

## Element review

| HTML or CSS surface | What was inspected |
|---|---|
| App document and shell | Language, viewport, manifest selection, service-worker shell, background, title, route focus, loading label, retry, guidance and external logo link. |
| Keypad | Store heading and truncation, history/settings icon names, active unit and keyboard movement, currency precision, live draft, numerals/backspace/zeros/decimal/empty PIN cell, memo label, rate/error copy, disabled versus ready Charge, scroll container and pinned footer. |
| Pay | Header actions, countdown, large values, line wrapping, long memo, urgency colors, amount due/received, option eligibility, QR quiet zone, image bounds and screenshot decoding, invoice/address detail and manual selection, modal names, focus and action states. |
| Pending and terminal | Spinner paths with reduced motion, check or warning icon, explicit outcome, warning text, pending rail, credited totals, return timer, New Sale, Receipt, overpayment and late credit. |
| History | Link visibility, PIN mask, disabled keys, referee messages, local-data explanation, backlog/retry, row target and hover, paid status text, order, rail, memo, warnings, receipt and the trip back. |
| Receipt | Heading, identifiers, field/value associations, method availability, charged versus received, excess, note wrapping, live-payment link, share/print availability, dark preview, toolbar suppression, paper margins and long unbroken text. |
| Settings | Back, heading, field labels, long store name/address, threshold and wallet-management explanation in light/dark layouts. |
| Shared components | Text sizing and cascade, typography, light/dark variants, active versus disabled, hover contrast, hit areas, modal Escape, tab selection, status/error feedback, image alternatives and reduced motion. |

The footer link destination is inspected locally. The external Bull Bitcoin website, physical devices, printer hardware, native share recipients, Haptic/Audio output and screen-reader speech require their own environments. Endpoint states apart from the actual loopback offline fixture are deterministic API fixtures. A screenshot decode proves the UI emitted the asserted string. Wallet acceptance, account settlement and owner ledger credit require a separate live regtest.
