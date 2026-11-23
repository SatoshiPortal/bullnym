import assert from 'node:assert/strict'
import { watchedAtSevenDays } from './watch-window.mjs'
import { origin, sale } from './fixture.mjs'

const open = (page, route = '') => page.goto(origin + '/cafe' + route)
const button = (page, name) => page.getByRole('button', { name, exact: true })
const focus = (page) => page.evaluate(() => window.dispatchEvent(new Event('focus')))
const paid = { state: 'paid', paid_rail: 'lightning', received_sat: 60000 }
const rowFor = (page, id) => page.locator('a[href="#/receipt/' + id + '"]')

async function history(page, fixture, ids) {
	fixture.store.cashier_pin_set = true
	await page.addInitScript((list) => localStorage.setItem('bullnym:cafe:sales', JSON.stringify(list)), ids)
	await open(page, '#/history')
	for (const digit of '2468') await button(page, 'Add ' + digit).click()
	await button(page, 'Show sales').click()
}

export const stateEdges = {
	async QUOTE_HALF_SAT({ page, fixture }) {
		fixture.intercept = (call) => {
			if (call.path.endsWith('/rate')) return { json: { currency: 'USD', btc_price: '80000.00', fetched_at: new Date().toISOString() } }
			if (call.method === 'POST' && call.path.endsWith('/sales')) {
				assert.equal(call.body.fiat_amount, '0.15')
				const row = sale('half-sat', { fiat_amount: '0.15', currency: 'USD', btc_price: '80000.00', amount_sat: 188 })
				fixture.sales.set(row.id, row)
				return { json: row }
			}
		}
		await open(page)
		await page.getByText(/\/BTC/).waitFor()
		for (const label of ['Add cents', 'Add 1', 'Add 5']) await button(page, label).click()
		assert.match(await page.locator('main').innerText(), /≈ 188 sats/)
		await button(page, 'Charge').click()
		await page.getByText('188 sats', { exact: true }).waitFor()
	},
	async CREATE_RESPONSE_LOST({ page, fixture }) {
		await open(page)
		await page.getByRole('tab', { name: 'Sats' }).click()
		await button(page, 'Add 9').click()
		fixture.intercept = async (call) => {
			if (call.method === 'POST' && call.path.endsWith('/sales')) {
				await fixture.respond(call)
				return { abort: true }
			}
		}
		await button(page, 'Charge').click()
		const alert = page.getByRole('alert')
		await alert.waitFor()
		assert.match(await alert.innerText(), /could not confirm whether the sale was created/i)
		assert.equal(fixture.sales.size, 1)
		await focus(page)
		assert.equal(fixture.calls.filter((c) => c.method === 'POST' && c.path.endsWith('/sales')).length, 1)
		assert.equal(await page.getByRole('img', { name: /payment code/ }).count(), 0)
	},
	async SEVEN_DAY_RECEIPT({ page, fixture }) {
		const watched = watchedAtSevenDays()
		for (const [i, id] of ['inside', 'boundary', 'outside'].entries()) {
			const row = sale(id, { ...paid, paid_rail: 'onchain', onchain_address: 'bcrt1qfixture' })
			fixture.sales.set(id, row)
			await open(page, '#/receipt/' + id)
			const paper = page.locator('article')
			const received = paper.locator('dt').filter({ hasText: /^Received$/ }).locator('+ dd')
			await paper.getByText('Paid', { exact: true }).waitFor()
			assert.equal(await received.innerText(), '60,000 sats')
			if (watched.has(i + 1)) row.received_sat += 10000
			await page.reload()
			await paper.getByText('Paid', { exact: true }).waitFor()
			assert.equal(await received.innerText(), i === 0 ? '70,000 sats' : '60,000 sats')
			assert.equal(await paper.getByText('Overpaid by').count(), Number(i === 0))
		}
	},
	async CURRENCY_SCALE({ page, fixture }) {
		for (const [currency, places] of [['USD', 2], ['CAD', 2], ['EUR', 2], ['ARS', 2], ['MXN', 2], ['CRC', 0], ['COP', 0]]) {
			fixture.store.currency = currency
			await open(page)
			await page.getByRole('tab', { name: currency }).waitFor()
			assert.equal(await button(page, 'Add cents').count(), Number(places > 0), currency)
			await button(page, 'Add 5').click()
			if (places) {
				await button(page, 'Add cents').click()
				await button(page, 'Add 1').click()
			}
			await button(page, 'Charge').click()
			await page.getByText(/Waiting for payment/).waitFor()
			const record = [...fixture.sales.values()].at(-1)
			assert.equal(record.currency, currency)
			assert.equal(record.fiat_amount, places ? '5.10' : '5')
		}
	},
	async RECORDED_FIAT({ page, fixture }) {
		const records = [
			['colon', 'CRC', '12.50', /12\.50/],
			['large', 'USD', '999999999999999.01', /999,999,999,999,999\.01/],
		]
		for (const [id, currency, amount] of records) {
			const amount_sat = id === 'large' ? 999999999999999 : 12500
			fixture.sales.set(id, sale(id, { ...paid, currency, fiat_amount: amount, amount_sat, received_sat: amount_sat }))
		}
		await history(page, fixture, records.map(([id]) => id))
		const errors = []
		for (const [id, , , label] of records) {
			const row = rowFor(page, id)
			await row.waitFor()
			if (!label.test(await row.innerText())) errors.push(id + ' history')
		}
		for (const [id, , , label] of records) {
			await open(page, '#/pay/' + id)
			await page.getByRole('heading', { name: 'Paid' }).waitFor()
			if (!label.test(await page.locator('main').innerText())) errors.push(id + ' payment')
			await button(page, 'Receipt').click()
			const paper = page.locator('article')
			await paper.waitFor()
			if (!label.test(await paper.innerText())) errors.push(id + ' receipt')
		}
		assert.deepEqual(errors, [], 'The recorded fiat amount must retain every cent')
	},
	async ALLOCATION_SUPERSEDED({ page, fixture }) {
		const row = sale('superseded')
		fixture.sales.set(row.id, row)
		let rejectRequest
		const held = new Promise((resolve) => { rejectRequest = resolve })
		fixture.intercept = async (c) => {
			if (c.path.endsWith('/onchain')) { await held; return { status: 503 } }
		}
		try {
			await open(page, '#/pay/' + row.id)
			await button(page, 'Bitcoin').click()
			await button(page, 'Continue').click()
			await button(page, 'Adding').waitFor()
			Object.assign(row, { onchain_address: 'bcrt1qfixture', onchain_request: 'bitcoin:bcrt1qfixture?amount=0.0006' })
			await page.getByRole('tab', { name: 'Bitcoin' }).click()
			await button(page, 'Copy Bitcoin address').waitFor()
		} finally { rejectRequest() }
		const poll = (r) => r.url().includes('/superseded?wait=')
		await page.waitForRequest(poll)
		await page.waitForRequest(poll)
		assert.equal(await page.getByText(/Could not add Bitcoin/).count(), 0, 'failure of a superseded request should not contradict an issued address')
	},
	async QUOTE_DENOMINATION_RACE({ page, fixture }) {
		await page.clock.install()
		await open(page)
		await page.getByText(/\/BTC/).waitFor()
		await button(page, 'Add 5').click()
		fixture.store.currency = 'CRC'
		const changed = page.waitForResponse((r) => r.url().endsWith('/rate'))
		await page.clock.fastForward(60001)
		await changed
		await page.getByRole('tab', { name: 'CRC' }).waitFor()
		assert(await button(page, 'Charge').isDisabled())
		await page.getByText(/currency changed/).waitFor()
		await button(page, 'Add 2').click()
		await button(page, 'Charge').click()
		await page.getByText(/Waiting for payment/).waitFor()
		assert.equal(fixture.sales.get('sale-1').currency, 'CRC')
		assert.equal(fixture.sales.get('sale-1').fiat_amount, '2')
	},
	async CURRENCY_CHANGED_AT_CHARGE({ page, fixture }) {
		await page.clock.install()
		await open(page)
		await page.getByText(/\/BTC/).waitFor()
		await button(page, 'Add 5').click()

		const delayed = Promise.withResolvers()
		let hold = true
		fixture.intercept = async (call) => {
			if (hold && call.path.endsWith('/rate')) {
				hold = false
				await delayed.promise
				return { json: { currency: 'USD', btc_price: '100000', fetched_at: new Date().toISOString() } }
			}
		}
		const polling = page.waitForRequest((r) => r.url().endsWith('/rate'))
		await page.clock.fastForward(60001)
		await polling
		try {
			fixture.store.currency = 'CRC'
			const rejected = page.waitForResponse((r) => r.url().endsWith('/sales') && r.status() === 409)
			await button(page, 'Charge').click()
			await rejected
			await page.getByRole('tab', { name: 'CRC' }).waitFor()
			await page.getByText(/store currency changed/i).waitFor()
			assert.equal(fixture.sales.size, 0)
			assert(await button(page, 'Charge').isDisabled())
		} finally { delayed.resolve() }
		await button(page, 'Add 2').click()
		await button(page, 'Charge').click()
		await page.getByText(/Waiting for payment/).waitFor()
		const requests = fixture.calls.filter((c) => c.method === 'POST' && c.path.endsWith('/sales'))
		assert.deepEqual(requests.map((c) => c.body), [
			{ fiat_amount: '5.00', expected_currency: 'USD' },
			{ fiat_amount: '2', expected_currency: 'CRC' },
		])
		assert.equal(fixture.sales.size, 1)
		assert.equal(fixture.sales.get('sale-1').currency, 'CRC')
	},
	async STORE_REFRESH_RETRY({ page, fixture }) {
		await open(page, '#/settings')
		await page.getByText('Coffee shop').waitFor()
		let fail = true
		fixture.intercept = (c) => {
			if (fail && c.path.endsWith('/checkout/cafe')) {
				fail = false
				return { status: 503 }
			}
		}
		fixture.store.name = 'Renamed shop'
		const failed = page.waitForResponse((r) => r.url().endsWith('/checkout/cafe') && r.status() === 503)
		await focus(page)
		await failed
		await page.getByText('Coffee shop').waitFor()
		await page.getByText('Renamed shop').waitFor()
	},
	async OPEN_DEADLINE({ page, fixture }) {
		const now = Date.now()
		await page.clock.install({ time: new Date(now) })
		const row = sale('deadline', { expires_at: new Date(now + 60000).toISOString() })
		fixture.sales.set(row.id, row)
		await open(page, '#/pay/deadline')
		await button(page, 'Bitcoin').click()
		await page.getByRole('dialog').waitFor()
		await page.clock.fastForward(61000)
		assert.equal(await page.getByRole('dialog').count(), 0, 'deadline should dismiss an unconfirmed allocation')
		assert.equal(await button(page, 'Bitcoin').count(), 0)
		await page.getByText(/Checking the payment status/).waitFor()
		Object.assign(row, { state: 'in-progress', progress: 'ark' })
		await page.getByText(/Ark payment is being delivered/).waitFor()
		assert.equal(await page.getByRole('button', { name: 'Cancel sale' }).count(), 0)
		Object.assign(row, paid, { paid_rail: 'ark' })
		await page.getByText('via Ark').waitFor()
		assert.equal(fixture.calls.filter((c) => c.path.endsWith('/onchain')).length, 0)
	},
	async CANCEL_WITH_FUNDS({ page, fixture }) {
		for (const [index, credit] of [{ seen_sat: 1000 }, { received_sat: 2000 }].entries()) {
			const row = sale('partial-' + index, credit)
			fixture.sales.set(row.id, row)
			fixture.intercept = (c) => {
				if (c.path.endsWith('/cancel')) {
					Object.assign(row, { state: 'canceled', late_payment: true })
					return { json: row }
				}
			}
			await open(page, '#/pay/' + row.id)
			await button(page, 'Cancel sale').click()
			await page.getByRole('heading', { name: 'Late payment' }).waitFor()
			await page.getByText(index === 0 ? /1,000 sats unconfirmed/ : /2,000 sats received/).waitFor()
			await button(page, 'New Sale').click()
			await page.getByRole('tab', { name: 'Sats' }).waitFor()
		}
	},
	async CANCEL_COMPETING({ page, fixture }) {
		const row = sale('cancel-race')
		fixture.sales.set(row.id, row)
		let finish
		const waiting = new Promise((resolve) => { finish = resolve })
		fixture.intercept = async (c) => {
			if (c.path.endsWith('/cancel')) { await waiting; return { status: 409 } }
		}
		try {
			await open(page, '#/pay/' + row.id)
			await button(page, 'Cancel sale').click()
			await button(page, 'Canceling…').waitFor()
			assert(await button(page, 'Bitcoin').isDisabled(), 'cannot begin allocating an address while canceling')
		} finally { finish() }
		await page.getByText(/Cancellation refused/).waitFor()
		await button(page, 'Bitcoin').click()
		await page.getByRole('dialog').waitFor()
	},
	async HISTORY_REFRESH({ page, fixture }) {
		const row = sale('history-live')
		fixture.sales.set(row.id, row)
		await page.clock.install()
		await history(page, fixture, [row.id])
		const link = rowFor(page, row.id)
		await link.getByText('Waiting for payment').waitFor()
		Object.assign(row, paid)
		await page.clock.fastForward(61000)
		await link.getByText('Paid', { exact: true }).waitFor()
		fixture.intercept = (c) => c.path.endsWith('/' + row.id) ? { status: 503 } : null
		await focus(page)
		await page.getByRole('alert').waitFor()
		assert(await link.getByText('Paid', { exact: true }).isVisible(), 'last known sale should remain on transient failure')
		fixture.intercept = undefined
		await button(page, 'Try again').click()
		await page.getByRole('alert').waitFor({ state: 'hidden' })
	},
	async HISTORY_SCOPE({ page, fixture }) {
		fixture.sales.set('mine', sale('mine'))
		fixture.sales.set('foreign', sale('foreign', { store_nym: 'other', memo: 'Other register' }))
		await history(page, fixture, ['mine','foreign'])
		await rowFor(page, 'mine').waitFor()
		assert.equal(await rowFor(page, 'foreign').count(), 0, 'another store appeared in this cashier history')
	},
	async HISTORY_MISSING({ page, fixture }) {
		fixture.sales.set('available', sale('available'))
		await history(page, fixture, ['available', 'gone'])
		await rowFor(page, 'available').waitFor()
		await page.getByText('1 saved sale is unavailable.').waitFor()
		assert.equal(await button(page, 'Try again').count(), 0)
	},
	async HISTORY_REVISION({ page, fixture }) {
		const row = sale('history-revision', { ...paid, updated_at: '2026-11-22T20:00:01Z' })
		fixture.sales.set(row.id, row)
		await history(page, fixture, [row.id])
		const link = rowFor(page, row.id)
		await link.getByText('Paid', { exact: true }).waitFor()
		Object.assign(row, { state: 'open', paid_rail: null, received_sat: 0, updated_at: '2026-11-22T20:00:00Z' })
		const stale = page.waitForResponse((r) => r.url().endsWith('/' + row.id))
		await focus(page)
		await stale
		await link.click()
		await page.locator('article').waitFor()
		assert.equal(await page.locator('article dd').filter({ hasText: /^Paid$/ }).count(), 1, 'receipt lost the paid revision already seen in history')
	},
	async SHARE_PENDING({ page, fixture }) {
		await page.addInitScript(() => {
			window.shareCalls = 0
			Object.defineProperty(navigator, 'share', { value: () => {
				window.shareCalls++
				return new Promise((resolve, reject) => { window.endShare = (fail) => fail ? reject(new Error('unavailable')) : resolve() })
			} })
		})
		fixture.sales.set('shared', sale('shared', paid))
		await open(page, '#/receipt/shared')
		const share = button(page, 'Share')
		await share.click()
		assert(await share.isDisabled(), 'native share is already open')
		await share.evaluate((el) => el.click())
		assert.equal(await page.evaluate(() => window.shareCalls), 1)
		await page.evaluate(() => window.endShare(true))
		await page.getByRole('alert').waitFor()
		await share.click()
		await page.evaluate(() => window.endShare(false))
		await page.getByRole('alert').waitFor({ state: 'hidden' })
		assert(await share.isEnabled())
	},
	async MEMO_BOUNDARY({ page, fixture }) {
		await open(page)
		await page.getByRole('tab', { name: 'Sats' }).click()
		await button(page, 'Add 1').click()
		const memo = page.getByRole('textbox')
		const accepted = '🍊'.repeat(200)
		await memo.fill(accepted)
		assert.equal(await memo.inputValue(), accepted, '200 Unicode characters should fit')
		await memo.fill(accepted + 'x')
		assert(await button(page, 'Charge').isDisabled())
		await page.getByRole('alert').waitFor()
		await memo.fill(accepted)
		await button(page, 'Charge').click()
		await page.getByText(accepted, { exact: true }).waitFor()
		assert.equal(fixture.sales.get('sale-1').memo, accepted)
	},
}
