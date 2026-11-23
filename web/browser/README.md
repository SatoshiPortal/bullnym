# Cashier browser tests

Run from the web directory with Node.js 22 and zbarimg on PATH.

~~~sh
npm ci
npx playwright install chromium firefox
npm run check
npm test -- --maxWorkers=2 --minWorkers=1
npm run build
npm run test:browser
~~~

The default suite runs at 1280×800 and 390×844. It loads the built application and intercepts HTTP requests to supply deterministic cashier responses. The QR assertions decode screenshots. Each browser context starts with its own storage. The two-tab cases share a context intentionally. The runner saves results under browser-results and retains a Playwright trace for each failed case with a browser context.

To test a separately served local bundle, set APP_ORIGIN to its loopback origin. DIST_URL can point to a writer build as an absolute file URL with a trailing slash. ENGINES, SIZES and CASES accept comma-separated selections. FIREFOX_EXECUTABLE and CHROMIUM_EXECUTABLE select compatible installed binaries.

~~~sh
CASES=RECEIPT_REVISION,ADDRESS_REVISION,DIALOG_SETTLEMENT,DIALOG_OTHER_TAB,RECEIPT_MERCHANT npm run test:browser
~~~

| Case | Evidence |
|---|---|
| RECEIPT_REVISION | A paid receipt keeps 70,000 sats when a 60,000-sat snapshot from 800 microseconds earlier arrives. A later revision in that same millisecond raises the visible receipt to 80,000 sats. |
| ADDRESS_REVISION | After the cashier adds Bitcoin, a delayed open snapshot with the same revision and no address preserves the selected Bitcoin QR and its copy control. |
| DIALOG_SETTLEMENT | A sale settles with the confirmation open. The dialog closes, Receipt becomes available, and no address request is sent. |
| DIALOG_OTHER_TAB | Another tab obtains the address. The first tab closes its dialog and displays the same QR with one address request across both tabs. |
| RECEIPT_MERCHANT | A direct receipt for an unknown local sale uses a neutral heading. A receipt for a sale created here shows this store's name. The public sale response supplies no merchant identity. |

The fixture uses the current sale revision field. A general hourly PIN refusal can omit the retry deadline, while a timed PIN lock carries its remaining duration. Neither browser path requires a live payment or an owner credential.
