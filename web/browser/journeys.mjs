import { checkpoint } from './checkpoint.mjs'
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdtemp, open as openFile, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { origin, sale } from './fixture.mjs'

const url = (hash = '', nym = 'cafe') => origin + '/' + nym + hash
const button = (page, name) => {
	const control = page.getByRole('button', { name, exact: true })
	return ['Lightning', 'Bitcoin'].includes(name)
		? control.or(page.getByRole('tab', { name, exact: true }))
		: control
}
const visible = async (page, text) => {
	await page.getByText(text, { exact: false }).first().waitFor()
	await checkpoint(page, String(text))
}
const click = async (page, name) => {
	await button(page, name).click()
	await checkpoint(page, 'click ' + name)
}
const open = (page, hash = '') => page.goto(url(hash))
const digits = async (page, number) => {
	for (const n of number) await click(page, 'Add ' + n)
}
const pin = async (page, number) => {
	await digits(page, number)
	await click(page, 'Show sales')
}
const ready = async (page) => {
	await page.getByRole('tab', { name: 'Sats' }).waitFor()
	await checkpoint(page, 'keypad')
}
const go = async (page, route) =>
	page.evaluate((value) => {
		location.hash = value
	}, route)
const paid = (amount = 60000, rail = 'ark') => ({
	state: 'paid',
	paid_rail: rail,
	received_sat: amount,
	paid_at: new Date().toISOString(),
})
const grantClipboard = (page) =>
	page.addInitScript(() => {
		Object.defineProperty(navigator, 'clipboard', {
			configurable: true,
			value: {
				writeText: async (text) => {
					window.copiedText = text
				},
			},
		})
	})
const held = () => {
	let release
	const wait = new Promise((r) => {
		release = r
	})
	return { wait, release }
}
export async function decodeQr(page) {
	const image = page.getByRole('img', { name: /code|QR/i })
	const dir = await mkdtemp(join(tmpdir(), 'checkout-qr-'))
	try {
		const path = join(dir, 'rendered.png')
		await image.screenshot({ path })
		const output = join(dir, 'decoded.txt')
		const handle = await openFile(output, 'w')
		try {
			execFileSync('zbarimg', ['--nodbus', '--quiet', '--raw', path], {
				stdio: ['ignore', handle.fd, 'inherit'],
				timeout: 10000,
			})
		} finally {
			await handle.close()
		}
		return (await readFile(output, 'utf8')).replace(/\r?\n$/, '')
	} finally {
		await rm(dir, { recursive: true, force: true })
	}
}

// The next poll starts only after the previous response has been processed.
async function nextTwoPolls(page, id) {
	const isPoll = (r) =>
		new URL(r.url()).pathname === '/api/v1/sales/' + id && new URL(r.url()).searchParams.has('wait')
	await page.waitForRequest(isPoll)
	await page.waitForRequest(isPoll)
}

export const journeys = {
	async ROUTES({ page, fixture }) {
		await page.goto(url('', ''))
		await visible(page, "Open the store's link")
		await page.goto(url('', 'absent'))
		await visible(page, 'There is no store')
		await click(page, 'Try again')
		await visible(page, 'There is no store')
		let fail = true
		fixture.intercept = () => (fail ? { abort: true } : null)
		await open(page)
		await visible(page, 'server is not reachable')
		fail = false
		await click(page, 'Try again')
		await ready(page)
		await go(page, '/settings')
		await visible(page, 'cafe@example.test')
		await page.getByRole('link', { name: /Back/ }).click()
		await ready(page)
		await page.goBack()
		await visible(page, 'cafe@example.test')
		await page.goForward()
		await ready(page)
		await go(page, '/pay/%zz')
		await ready(page)
		assert.equal(fixture.sales.size, 0)
	},
	async ENTRY({ page, fixture }) {
		await open(page)
		await ready(page)
		assert(await button(page, 'Charge').isDisabled())
		await digits(page, '1')
		await click(page, 'Add cents')
		await digits(page, '249')
		await click(page, 'Delete digit')
		await digits(page, '5')
		await page.getByRole('textbox').fill(' Coffee ')
		await click(page, 'Charge')
		await visible(page, 'Waiting for payment')
		const record = [...fixture.sales.values()][0]
		assert.equal(record.fiat_amount, '1.25')
		assert.equal(record.memo, 'Coffee')
		await click(page, 'Cancel sale')
		await ready(page)
		assert.equal(fixture.sales.get(record.id).state, 'canceled')
		await page.getByRole('tab', { name: 'Sats' }).click()
		await digits(page, '5')
		await click(page, 'Add zeros')
		await click(page, 'Charge')
		await visible(page, 'Waiting for payment')
		assert.equal([...fixture.sales.values()][1].amount_sat, 500)
	},
	async CREATE_RACE({ page, fixture }) {
		const gate = held()
		fixture.intercept = async (c) => {
			if (c.method === 'POST') await gate.wait
		}
		await open(page)
		await ready(page)
		await digits(page, '2')
		await button(page, 'Charge').evaluate((el) => {
			el.click()
			el.click()
		})
		await button(page, 'Preparing').waitFor()
		await page.getByRole('link', { name: 'Settings' }).click()
		await visible(page, 'cafe@example.test')
		gate.release()
		await page.waitForFunction(() =>
			Object.keys(localStorage).some(
				(key) => key === 'bullnym:cafe:sale:sale-1' || localStorage.getItem(key)?.includes('sale-1'),
			),
		)
		await visible(page, 'cafe@example.test')
		assert.equal(fixture.sales.size, 1)
	},
	async CREATE_ERRORS({ page, fixture }) {
		await open(page)
		await ready(page)
		await digits(page, '9')
		for (const [status, message] of [
			[0, 'server is not reachable'],
			[429, 'Too many sales'],
			[503, 'Ark hint missing'],
			[400, 'amount invalid'],
		]) {
			fixture.intercept = (c) =>
				c.method === 'POST'
					? status
						? { status, json: { error: status === 503 ? 'Ark hint missing' : 'amount invalid' } }
						: { abort: true }
					: null
			await click(page, 'Charge')
			await visible(page, message)
		}
		fixture.intercept = undefined
		await click(page, 'Delete digit')
		await digits(page, '3')
		await click(page, 'Charge')
		await visible(page, 'Waiting for payment')
		assert.equal(fixture.sales.size, 1)
	},
	async PAYMENT({ page, fixture }) {
		const s = sale('A')
		fixture.sales.set(s.id, s)
		await open(page, '#/pay/A')
		await visible(page, 'Waiting for payment')
		Object.assign(s, { seen_sat: 5000 })
		await visible(page, '55,000 sats short')
		Object.assign(s, {
			state: 'in-progress',
			seen_sat: 60000,
			progress: 'onchain',
			warnings: ['not-guaranteed', 'low-fee', 'unconfirmed-parents', 'replaced'],
		})
		await visible(page, 'The fee is low')
		await visible(page, 'It spends coins')
		await visible(page, "isn't guaranteed")
		assert.equal(s.received_sat, 0)
		assert.equal(await button(page, 'Cancel sale').count(), 0)
		await page.reload()
		await visible(page, 'in the mempool')
		Object.assign(s, { state: 'open', progress: null, seen_sat: 0, notice: 'payment-replaced' })
		await visible(page, 'Payment replaced:')
		s.notice = 'payment-dropped'
		await visible(page, 'Payment dropped:')
		Object.assign(s, paid(60000, 'onchain'))
		await visible(page, 'via Bitcoin')
		s.received_sat += 1000
		await page.evaluate(() => window.dispatchEvent(new Event('focus')))
		await visible(page, 'received 61,000')
		await click(page, 'Receipt')
		await visible(page, 'Receipt ready')
		await visible(page, '61,000 sats')
		await page.goBack()
		await click(page, 'New Sale')
		await ready(page)
	},
	async CANCEL_FAILURE({ page, fixture }) {
		fixture.sales.set('A', sale('A'))
		await open(page, '#/pay/A')
		await visible(page, 'Waiting for payment')
		for (const status of [503, 409, 0]) {
			fixture.intercept = (c) =>
				c.path.endsWith('/cancel')
					? status
						? { status, json: { error: 'Cancellation refused' } }
						: { abort: true }
					: null
			await click(page, 'Cancel sale')
			await visible(page, /could not cancel|payment is arriving|cancellation refused/i)
			assert(page.url().endsWith('/pay/A'))
			assert.equal(fixture.sales.get('A').state, 'open')
		}
		fixture.intercept = undefined
		await click(page, 'Cancel sale')
		await ready(page)
	},
	async DIRECT({ page, fixture }) {
		for (const [r, t] of [
			['pay', 'This sale could not be found'],
			['receipt', 'Receipt not found'],
		]) {
			await open(page, '#/' + r + '/missing')
			await visible(page, t)
		}
		fixture.sales.set('A', sale('A'))
		let offline = true
		fixture.intercept = (c) => (offline && c.path.endsWith('/A') ? { abort: true } : null)
		await open(page, '#/receipt/A')
		await visible(page, /reconnecting|could not load/i)
		offline = false
		await page.evaluate(() => window.dispatchEvent(new Event('online')))
		await visible(page, 'Waiting for payment')
		Object.assign(fixture.sales.get('A'), paid())
		await visible(page, 'Receipt ready')
		assert.equal(
			await button(page, 'Share').count(),
			await page.evaluate(() => (typeof navigator.share === 'function' ? 1 : 0)),
		)
	},
	async TERMINALS({ page, fixture }) {
		for (const [id, state, late] of [
			['A', 'canceled', false],
			['B', 'expired', false],
			['C', 'expired', true],
			['D', 'canceled', true],
		]) {
			fixture.sales.set(id, sale(id, { state, late_payment: late, received_sat: late ? 10 : 0 }))
			await open(page, '#/pay/' + id)
			await visible(page, late ? 'Late payment' : state === 'expired' ? 'Returning' : 'This sale was canceled')
			if (state === 'expired' && !late) {
				await ready(page)
			} else await page.getByRole('button', { name: late ? 'New Sale' : 'Back', exact: true }).click()
		}
	},
	async HISTORY({ page, fixture, newContext }) {
		await open(page)
		await ready(page)
		assert.equal(await page.getByRole('link', { name: 'Recent sales' }).count(), 0)
		await go(page, '/history')
		await visible(page, 'owner has not set')
		fixture.store.cashier_pin_set = true
		await page.reload()
		await visible(page, 'Cashier PIN')
		await pin(page, '0000')
		await visible(page, 'Wrong PIN')
		await pin(page, '2468')
		await visible(page, /sales.*will appear|no sales/i)
		await go(page, '/')
		await ready(page)
		await digits(page, '3')
		await click(page, 'Charge')
		await visible(page, 'Waiting for payment')
		await go(page, '/history')
		await visible(page, 'Cashier PIN')
		await pin(page, '2468')
		await page.locator('a[href="#/receipt/sale-1"]').click()
		await visible(page, 'Waiting for payment')
		await page.goBack()
		await visible(page, 'Cashier PIN')
		const other = await (await newContext()).newPage()
		await open(other, '#/history')
		await pin(other, '2468')
		await visible(other, /sales.*will appear|no sales/i)
	},
	async LIMIT({ page, fixture }) {
		fixture.store.cashier_pin_set = true
		fixture.pinChecks = Array(9).fill(fixture.time - 3599000)
		await page.clock.install()
		await open(page, '#/history')
		await pin(page, '2468')
		await visible(page, /sales.*will appear|no sales/i)
		await go(page, '/')
		await ready(page)
		await go(page, '/history')
		await pin(page, '2468')
		await visible(page, /too many PIN checks/i)
		await visible(page, 'Try again later')
		assert(await button(page, 'Add 1').isEnabled())
		fixture.time += 1000
		await page.clock.fastForward(1000)
		await pin(page, '2468')
		await visible(page, /sales.*will appear|no sales/i)
	},
	async BITCOIN_OPTION({ page, fixture }) {
		for (const amount of [250, fixture.store.onchain_min_sat]) {
			await open(page)
			await page.getByRole('tab', { name: 'Sats' }).click()
			await digits(page, String(amount))
			await click(page, 'Charge')
			const qr = page.getByRole('img', { name: 'Lightning payment code' })
			await qr.waitFor()
			const bitcoin = page.getByRole('button', { name: 'Bitcoin', exact: true })
			const eligible = amount >= fixture.store.onchain_min_sat
			const option = eligible ? bitcoin : page.getByText('Bitcoin on-chain is offered from 50,000 sats.')
			await option.waitFor()
			assert.equal(await bitcoin.count(), Number(eligible))
			const viewport = page.viewportSize()
			for (const size of [viewport, { width: 320, height: 568 }]) {
				await page.setViewportSize(size)
				const box = await option.boundingBox()
				const code = await qr.boundingBox()
				assert(box && code && box.y >= 0 && box.y + box.height <= Math.min(size.height, code.y),
					'The Bitcoin option or minimum must be visible above the payment QR on entry')
			}
			await page.setViewportSize(viewport)
			if (eligible) {
				await bitcoin.click()
				await page.getByRole('dialog', { name: 'Bitcoin on-chain' }).waitFor()
				assert.equal(fixture.calls.filter(c => c.path.endsWith('/onchain')).length, 0)
				await click(page, 'Continue')
				await page.getByRole('img', { name: 'Bitcoin payment code' }).waitFor()
				assert.equal(await page.getByRole('tab', { name: 'Bitcoin' }).getAttribute('aria-selected'), 'true')
				assert.equal(fixture.calls.filter(c => c.path.endsWith('/onchain')).length, 1)
			}
		}
	},
	async QR_SWITCH({ page, fixture }) {
		await grantClipboard(page)
		const s = sale('A')
		fixture.sales.set('A', s)
		await open(page, '#/pay/A')
		assert.equal(await decodeQr(page), s.lightning_request)
		await click(page, 'Copy')
		await visible(page, 'Copied')
		assert.equal(await page.evaluate(() => window.copiedText), s.lightning_request)
		await click(page, 'Bitcoin')
		await visible(page, /60 minutes/i)
		await click(page, 'Cancel')
		assert.equal(fixture.calls.filter((c) => c.path.endsWith('/onchain')).length, 0)
		await click(page, 'Bitcoin')
		await click(page, 'Continue')
		await visible(page, /Bitcoin address/i)
		assert.equal(await decodeQr(page), s.onchain_request)
		await click(page, 'Copy Bitcoin address')
		assert.equal(await page.evaluate(() => window.copiedText), s.onchain_address)
		await click(page, 'Lightning')
		assert.equal(await decodeQr(page), s.lightning_request)
		await click(page, 'Copy Lightning invoice')
		assert.equal(await page.evaluate(() => window.copiedText), s.lightning_invoice)
		assert.equal(fixture.calls.filter((c) => c.path.endsWith('/onchain')).length, 1)
	},
	async SHARE_PRINT({ page, fixture }) {
		await page.addInitScript(() => {
			Object.defineProperty(navigator, 'share', {
				value: async (input) => {
					window.shared = input.url
				},
			})
			window.print = () => {
				window.printed = true
			}
		})
		fixture.sales.set('A', sale('A', paid()))
		await open(page, '#/receipt/A')
		await click(page, 'Print')
		await click(page, 'Share')
		assert(await page.evaluate(() => window.printed))
		assert.equal(await page.evaluate(() => window.shared), page.url())
		await click(page, '← Back')
		await ready(page)
	},

	async ARK_PENDING({ page, fixture }) {
		const s = sale('A', {
			state: 'in-progress',
			progress: 'ark',
			expires_at: new Date(Date.now() - 86400000).toISOString(),
		})
		fixture.sales.set('A', s)
		await open(page, '#/pay/A')
		await visible(page, 'Ark payment is being delivered')
		assert.equal(await button(page, 'Cancel sale').count(), 0)
		await click(page, 'New Sale')
		await ready(page)
		await page.goBack()
		await visible(page, 'Ark payment is being delivered')
		fixture.intercept = (c) => (c.path.endsWith('/A') ? { status: 503 } : null)
		await visible(page, 'Reconnecting')
		await visible(page, 'Ark payment is being delivered')
		Object.assign(s, { seen_sat: 60000, warnings: ['not-guaranteed'] })
		fixture.intercept = undefined
		await page.evaluate(() => window.dispatchEvent(new Event('online')))
		await page.getByText('Reconnecting…').waitFor({ state: 'hidden' })
		s.seen_sat = 0
		s.notice = 'payment-dropped'
		await page.reload()
		await visible(page, 'Ark payment is being delivered')
		assert.equal(s.received_sat, 0)
		assert.equal(await button(page, 'Receipt').count(), 0)
		Object.assign(s, paid(60000, null))
		await button(page, 'Receipt').waitFor()
		assert.equal(await page.getByText(/via (Ark|Lightning|—)/).count(), 0)
		await click(page, 'Receipt')
		await visible(page, 'Receipt ready.')
	},
	async STALE({ page, fixture }) {
		const a = sale('A', { memo: 'Sale A' }),
			b = sale('B', { memo: 'Sale B' })
		fixture.sales.set('A', a)
		fixture.sales.set('B', b)
		const started = held(),
			gate = held()
		fixture.intercept = async (c) => {
			if (c.path.endsWith('/A') && c.query) {
				const stale = { ...a }
				started.release()
				await gate.wait
				return { json: stale }
			}
		}
		await open(page, '#/pay/A')
		await visible(page, 'Sale A')
		await started.wait
		await go(page, '/pay/B')
		await visible(page, 'Sale B')
		Object.assign(b, paid(60000, 'lightning'))
		await visible(page, 'via Lightning')
		gate.release()
		await click(page, 'Receipt')
		await visible(page, 'Receipt ready.')
		assert.equal(await page.getByText('Sale A', { exact: true }).count(), 0)
		fixture.intercept = (c) => (c.path.endsWith('/B') ? { json: { ...b, state: 'open', paid_rail: null } } : null)
		await page.evaluate(() => window.dispatchEvent(new Event('focus')))
		await visible(page, 'Receipt ready.')
		assert.equal(fixture.sales.get('A').state, 'open')
	},
	async IDENTITY({ page, fixture }) {
		fixture.sales.set('B', sale('B', paid()))
		fixture.intercept = (c) => (c.path.endsWith('/A') ? { json: fixture.sales.get('B') } : null)
		await open(page, '#/pay/A')
		await visible(page, 'Reconnecting')
		assert.equal(await button(page, 'Receipt').count(), 0)
		fixture.sales.set('A', sale('A', { memo: 'Sale A' }))
		fixture.intercept = undefined
		await page.evaluate(() => window.dispatchEvent(new Event('online')))
		await visible(page, 'Sale A')
	},
	async TWO_TABS({ context, page, fixture }) {
		fixture.store.cashier_pin_set = true
		const other = await context.newPage()
		for (const p of [page, other]) {
			await open(p)
			await ready(p)
			await digits(p, '2')
		}
		await Promise.all([click(page, 'Charge'), click(other, 'Charge')])
		for (const p of [page, other]) await visible(p, 'Waiting for payment')
		assert.notEqual(page.url(), other.url())
		await go(page, '/history')
		await pin(page, '2468')
		await page.locator('a[href="#/receipt/sale-1"]').waitFor()
		await page.locator('a[href="#/receipt/sale-2"]').waitFor()
		await click(other, 'Cancel sale')
		await ready(other)
		await digits(other, '3')
		await click(other, 'Charge')
		await page.locator('a[href="#/receipt/sale-3"]').waitFor()
		await page.reload()
		await visible(page, 'Cashier PIN')
		assert.equal(await page.locator('a[href^="#/receipt"]').count(), 0)
	},
	async STORAGE({ page, fixture }) {
		fixture.store.cashier_pin_set = true
		await page.addInitScript(() => {
			Object.defineProperty(window, 'localStorage', {
				get() {
					throw Error('storage unavailable')
				},
			})
		})
		await open(page)
		await ready(page)
		await digits(page, '2')
		await click(page, 'Charge')
		await visible(page, 'Waiting for payment')
		await go(page, '/history')
		await pin(page, '2468')
		await page.locator('a[href="#/receipt/sale-1"]').waitFor()
		await page.reload()
		await pin(page, '2468')
		await visible(page, /sales.*will appear|no sales/i)
	},
	async PIN_BACKOFF({ page, fixture }) {
		fixture.store.cashier_pin_set = true
		await open(page, '#/history')
		await visible(page, 'Cashier PIN')
		await page.clock.install()
		fixture.intercept = (c) =>
			c.path.endsWith('/pin') ? { status: 401, json: { error: 'wrong PIN', retry_after: 120 } } : null
		await digits(page, '0000')
		await click(page, 'Delete digit')
		assert(await button(page, 'Show sales').isDisabled())
		await digits(page, '0')
		await click(page, 'Show sales')
		await visible(page, '2 min')
		assert(await button(page, 'Add 1').isDisabled())
		await page.clock.fastForward(120001)
		fixture.intercept = (c) => (c.path.endsWith('/pin') ? { abort: true } : null)
		await pin(page, '2468')
		await visible(page, 'server is not reachable')
		fixture.intercept = (c) => (c.path.endsWith('/pin') ? { json: { ok: false } } : null)
		await pin(page, '2468')
		await visible(page, 'Could not check the PIN')
		fixture.intercept = (c) => (c.path.endsWith('/pin') ? { status: 500 } : null)
		await pin(page, '2468')
		await visible(page, 'Could not check the PIN')
		fixture.intercept = (c) => (c.path.endsWith('/pin') ? { status: 404 } : null)
		fixture.store.cashier_pin_set = false
		await pin(page, '2468')
		await visible(page, 'owner has not set')
	},
	async HISTORY_RETRY({ page, fixture }) {
		fixture.store.cashier_pin_set = true
		fixture.sales.set('A', sale('A', { memo: 'First' }))
		fixture.sales.set('B', sale('B', paid()))
		await open(page)
		await ready(page)
		await page.evaluate(() => localStorage.setItem('bullnym:cafe:sales', JSON.stringify(['A', 'B'])))
		fixture.intercept = (c) => (c.path.endsWith('/B') ? { abort: true } : null)
		await go(page, '/history')
		await pin(page, '2468')
		await visible(page, 'Could not load all sales')
		await page.locator('a[href="#/receipt/A"]').waitFor()
		fixture.intercept = undefined
		await click(page, 'Try again')
		await page.locator('a[href="#/receipt/B"]').waitFor()
	},
	async QUOTE({ page, fixture }) {
		fixture.store.currency = 'CRC'
		fixture.intercept = (c) => (c.path.endsWith('/rate') ? { abort: true } : null)
		await open(page)
		await visible(page, 'Rate unavailable')
		await digits(page, '3')
		assert(await button(page, 'Charge').isDisabled())
		await page.getByRole('tab', { name: 'Sats' }).click()
		await digits(page, '2')
		await click(page, 'Add zeros')
		assert(await button(page, 'Charge').isEnabled())
		fixture.intercept = undefined
		await page.evaluate(() => window.dispatchEvent(new Event('online')))
		await visible(page, '/BTC')
		await page.getByRole('tab', { name: 'CRC' }).click()
		await digits(page, '5')
		assert.equal(await button(page, 'Add cents').count(), 0)
		await click(page, 'Add zeros')
		fixture.intercept = (c) => (c.path.endsWith('/rate') ? { status: 500 } : null)
		await page.evaluate(() => window.dispatchEvent(new Event('online')))
		await visible(page, 'not updated')
		assert(await button(page, 'Charge').isEnabled())
		await click(page, 'Charge')
		await visible(page, 'Waiting for payment')
		assert.equal(fixture.sales.get('sale-1').fiat_amount, '500')
	},
	async DIALOG({ page, context, fixture }) {
		const low = sale('low', { amount_sat: 49999 }),
			s = sale('A', { amount_sat: 50000 })
		fixture.sales.set('low', low)
		fixture.sales.set('A', s)
		await open(page, '#/pay/low')
		await visible(page, /not offered for this amount|offered from/)
		assert.equal(await button(page, 'Bitcoin').count(), 0)
		await go(page, '/pay/A')
		await click(page, 'Bitcoin')
		await page.keyboard.press('Escape')
		assert.equal(fixture.calls.filter((c) => c.path.endsWith('/onchain')).length, 0)
		fixture.intercept = (c) => (c.path.endsWith('/onchain') ? { status: 503 } : null)
		await click(page, 'Bitcoin')
		await click(page, 'Continue')
		await visible(page, 'Could not add Bitcoin')
		fixture.intercept = undefined
		const other = await context.newPage()
		await open(other, '#/pay/A')
		await click(other, 'Bitcoin')
		await click(page, 'Bitcoin')
		const allocation = held()
		const requested = (r) => r.method() === 'POST' && new URL(r.url()).pathname.endsWith('/onchain')
		fixture.intercept = async (c) => {
			if (c.method === 'POST' && c.path.endsWith('/onchain')) await allocation.wait
		}
		try {
			await Promise.all([
				page.waitForRequest(requested),
				other.waitForRequest(requested),
				click(page, 'Continue'),
				click(other, 'Continue'),
			])
			for (const tab of [page, other]) assert(await button(tab, 'Adding').isDisabled())
		} finally {
			allocation.release()
		}
		fixture.intercept = undefined
		for (const p of [page, other]) {
			await button(p, 'Copy Bitcoin address').waitFor()
			assert.equal(await decodeQr(p), s.onchain_request)
		}
		await page.reload()
		await click(page, 'Bitcoin')
		assert.equal(await decodeQr(page), s.onchain_request)
		assert.equal(fixture.calls.filter((c) => c.path.endsWith('/onchain')).length, 3)
		Object.assign(s, paid(50000))
		await visible(page, 'via Ark')
		assert.equal(await button(page, 'Bitcoin').count(), 0)
	},
	async OS_ACTIONS({ page, fixture }) {
		await page.addInitScript(() => {
			Object.defineProperty(navigator, 'clipboard', {
				value: {
					writeText: async () => {
						throw Error('denied')
					},
				},
			})
			Object.defineProperty(navigator, 'share', {
				value: async () => {
					throw new DOMException('cancel', 'AbortError')
				},
			})
		})
		const row = sale('A')
		fixture.sales.set('A', row)
		await open(page, '#/pay/A')
		await click(page, 'Copy')
		assert.equal(await page.getByLabel('Copy this payment request manually').inputValue(), row.lightning_request)
		await click(page, 'Copy Lightning invoice')
		const manual = page.getByLabel('Copy Lightning invoice manually')
		assert.equal(await manual.inputValue(), row.lightning_invoice)
		await manual.focus()
		assert.deepEqual(await manual.evaluate((input) => [input.selectionStart, input.selectionEnd]), [0, row.lightning_invoice.length])
		Object.assign(row, paid())
		await click(page, 'Receipt')
		await click(page, 'Share')
		await visible(page, 'Receipt ready')
	},

	async RECEIPT_REFRESH({ page, fixture }) {
		const row = sale('A', paid(60000, null))
		fixture.sales.set('A', row)
		await open(page, '#/receipt/A')
		await visible(page, 'Receipt ready.')
		assert.equal(await page.locator('dd', { hasText: 'Lightning' }).count(), 0)
		row.paid_rail = 'lightning'
		await page.locator('dd', { hasText: 'Lightning' }).waitFor()
		const late = sale('B', { state: 'expired' })
		fixture.sales.set('B', late)
		await go(page, '/receipt/B')
		await visible(page, 'Expired')
		Object.assign(late, { late_payment: true, received_sat: 60000 })
		await visible(page, 'Late payment')
	},

	async QR_FAILURE({ page, fixture }) {
		await grantClipboard(page)
		const row = sale('A', { lightning_request: 'lightning:' + 'x'.repeat(6000) })
		fixture.sales.set('A', row)
		await open(page, '#/pay/A')
		await visible(page, 'Could not display this code')
		assert.equal(await page.getByRole('img', { name: /code|QR/i }).count(), 0)
		await click(page, 'Copy')
		assert.equal(await page.evaluate(() => window.copiedText), row.lightning_request)
		row.lightning_request = 'lightning:' + row.lightning_invoice
		assert.equal(await decodeQr(page), row.lightning_request)
	},
	async RECEIPT_ACTIONS({ page, fixture }) {
		await page.addInitScript(() => {
			Object.defineProperty(navigator, 'share', {
				value: async () => {
					throw Error('share target unavailable')
				},
			})
		})
		const row = sale('A')
		fixture.sales.set('A', row)
		await open(page, '#/receipt/A')
		await visible(page, 'Waiting for payment')
		assert.equal(await button(page, 'Print').count(), 0)
		await click(page, 'New sale')
		await ready(page)
		await page.goBack()
		Object.assign(row, paid())
		await click(page, 'Share')
		await visible(page, 'Could not share the receipt')
		await click(page, '← Back')
		await ready(page)
	},
	async ACTION_RETURN({ page, fixture }) {
		for (const action of ['cancel', 'onchain']) {
			const started = held(),
				gate = held()
			const row = sale(action)
			fixture.sales.set(row.id, row)
			fixture.intercept = async (c) => {
				if (c.method === 'POST' && c.path.endsWith('/' + action)) {
					started.release()
					await gate.wait
				}
			}
			await open(page, '#/pay/' + row.id)
			if (action === 'cancel') await click(page, 'Cancel sale')
			else {
				await click(page, 'Bitcoin')
				await click(page, 'Continue')
			}
			await started.wait
			await go(page, '/settings')
			await visible(page, 'cafe@example.test')
			const completed = page.waitForResponse(
				(r) => r.request().method() === 'POST' && r.url().endsWith('/' + action),
			)
			gate.release()
			await completed
			await visible(page, 'cafe@example.test')
			await page.goBack()
			if (action === 'cancel') await visible(page, 'This sale was canceled')
			else {
				await click(page, 'Bitcoin')
				assert.equal(await decodeQr(page), row.onchain_request)
			}
		}
	},

	async RECEIPT_MERCHANT({ page, fixture }) {
		// The service identifies the store. A foreign receipt must use its canonical checkout.
		fixture.sales.set('foreign', sale('foreign', { ...paid(), store_nym: 'other' }))
		await open(page, '#/receipt/foreign')
		await page.getByRole('heading', { name: 'Other shop' }).waitFor()
		assert.equal(new URL(page.url()).pathname, '/other')
		await open(page)
		await ready(page)
		await digits(page, '2')
		await click(page, 'Charge')
		await visible(page, 'Waiting for payment')
		const local = [...fixture.sales.values()].find((s) => s.id !== 'foreign')
		Object.assign(local, paid(local.amount_sat))
		await click(page, 'Receipt')
		await page.getByRole('heading', { name: 'Coffee shop' }).waitFor()
	},
	async ADDRESS_REVISION({ page, fixture }) {
		const row = sale('addr', { updated_at: '2026-11-22T23:00:00.123456Z' })
		const original = { ...row }
		fixture.sales.set(row.id, row)
		await open(page, '#/pay/addr')
		await click(page, 'Bitcoin')
		await click(page, 'Continue')
		await button(page, 'Copy Bitcoin address').waitFor()
		const expected = row.onchain_request
		assert.equal(await decodeQr(page), expected)
		fixture.intercept = (c) => (c.method === 'GET' && c.path.endsWith('/addr') ? { json: original } : null)
		await nextTwoPolls(page, 'addr')
		assert.equal(
			await button(page, 'Copy Bitcoin address').count(),
			1,
			'stale open snapshot removed the assigned address',
		)
		assert.equal(await decodeQr(page), expected)
		fixture.intercept = undefined
		Object.assign(row, paid(60000, 'onchain'), { updated_at: '2026-11-22T23:00:01Z' })
		await click(page, 'Receipt')
		await visible(page, 'Receipt ready')
	},
	async RECEIPT_REVISION({ page, fixture }) {
		const row = sale('precise', {
			...paid(70000, 'onchain'),
			onchain_address: 'bcrt1qfixture',
			updated_at: '2026-11-22T23:00:00.123900Z',
		})
		fixture.sales.set(row.id, row)
		await open(page, '#/receipt/precise')
		await visible(page, '70,000 sats')
		fixture.intercept = (c) =>
			c.path.endsWith('/precise')
				? { json: { ...row, received_sat: 60000, updated_at: '2026-11-22T23:00:00.123100Z' } }
				: null
		await nextTwoPolls(page, 'precise')
		assert.equal(
			await page.getByText('70,000 sats', { exact: true }).count(),
			1,
			'an older response removed the overpayment',
		)
		fixture.intercept = undefined
		Object.assign(row, { received_sat: 80000, updated_at: '2026-11-22T23:00:00.123950Z' })
		await visible(page, '80,000 sats')
	},
	async DIALOG_SETTLEMENT({ page, fixture }) {
		const row = sale('dialog')
		fixture.sales.set(row.id, row)
		await open(page, '#/pay/dialog')
		await click(page, 'Bitcoin')
		await page.getByRole('dialog').waitFor()
		Object.assign(row, paid())
		await page.getByRole('dialog').waitFor({ state: 'hidden' })
		await button(page, 'Receipt').waitFor()
		assert.equal(fixture.calls.filter((c) => c.method === 'POST').length, 0)
		await click(page, 'Receipt')
		await visible(page, 'Receipt ready')
	},
	async DIALOG_OTHER_TAB({ context, page, fixture }) {
		const row = sale('shared')
		fixture.sales.set(row.id, row)
		const second = await context.newPage()
		await Promise.all([open(page, '#/pay/shared'), open(second, '#/pay/shared')])
		await click(page, 'Bitcoin')
		await click(second, 'Bitcoin')
		await click(second, 'Continue')
		await second.getByRole('dialog').waitFor({ state: 'hidden' })
		await page.getByRole('dialog').waitFor({ state: 'hidden' })
		await click(page, 'Bitcoin')
		assert.equal(await decodeQr(page), row.onchain_request)
		assert.equal(fixture.calls.filter((c) => c.method === 'POST' && c.path.endsWith('/onchain')).length, 1)
	},
}
