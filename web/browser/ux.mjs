import { decodeQr } from './journeys.mjs'
import { screenStates } from './screens.mjs'
import assert from 'node:assert/strict'
import { join } from 'node:path'
import { origin, sale } from './fixture.mjs'
import { checkpoint } from './checkpoint.mjs'

const open = (page, route) => page.goto(origin + '/cafe' + route)
const key = (page, value) => page.getByRole('button', { name: 'Add ' + value, exact: true }).click()
const longName = 'Mercado'.repeat(9)
const note = 'X'.repeat(200)
const paid = { state: 'paid', received_sat: 70000, paid_rail: 'onchain' }

async function layout(page, label, errors) {
	await page.evaluate(() => document.fonts.ready)
	await checkpoint(page, label, true)
	const defects = await page.evaluate(() => {
		const problems = []
		for (const element of document.querySelectorAll('main *')) {
			if (!element.checkVisibility() || element.closest('svg, .sr-only')) continue
			// A scrollable memo or an intentionally truncated preview has its own boundary.
			if (element.matches('textarea,input') || element.closest('.truncate')) continue
			for (const node of element.childNodes) {
				if (node.nodeType !== Node.TEXT_NODE || !node.textContent.trim()) continue
				const r = document.createRange()
				r.selectNodeContents(node)
				for (const box of r.getClientRects()) {
					if (!box.width) continue
					if (box.left < -1 || box.right > document.documentElement.clientWidth + 1) {
						problems.push(element.localName + ' "' + node.textContent.trim().slice(0, 25) + '" extends outside the page')
						break
					}
				}
			}
		}
		return problems
	})
	errors.push(...defects.map((d) => label + ': ' + d))
}
function finish(errors) {
	assert.deepEqual(errors, [])
}
export const uxJourneys = {
	async BRAND_LINK({ page, context }) {
		await context.route('https://bullbitcoin.com/**', (r) => r.fulfill({ contentType: 'text/html', body: '<title>Brand destination</title>' }))
		for (const url of [origin, origin + '/cafe', origin + '/cafe#/settings', origin + '/cafe#/history']) {
			await page.goto(url)
			const logo = page.getByRole('link', { name: 'Bull Bitcoin', exact: true })
			assert.equal(await logo.getAttribute('href'), 'https://bullbitcoin.com')
			const pending = context.waitForEvent('page')
			await logo.click()
			const destination = await pending
			await destination.waitForURL('https://bullbitcoin.com/')
			assert(await destination.evaluate(() => window.opener === null))
			await destination.close()
			assert.equal(page.url(), new URL(url).href)
		}
	},
	async SCREEN_STATES(options) {
		const errors = []
		await screenStates(options, (state) => layout(options.page, state, errors))
		finish(errors)
	},
	async PWA_REOPEN({ page, context }) {
		await page.goto(origin + '/pwa-test')
		await page.getByText(/\/BTC/).waitFor()
		await page.waitForFunction(() => !!navigator.serviceWorker.controller, undefined, { timeout: 30000 })
		await context.setOffline(true)
		await page.reload({ waitUntil: 'domcontentloaded' })
		await page.getByText('The server is not reachable.').waitFor()
		await checkpoint(page, 'offline-shell', true)
		await context.setOffline(false)
		await page.getByRole('tab', { name: 'Sats' }).waitFor()
	},
	async RECEIPT_BACK({ page, fixture }) {
		fixture.sales.set('direct', sale('direct'))
		await page.goto(origin)
		await open(page, '#/receipt/direct')
		await page.locator('article').waitFor()
		await page.evaluate(() => { location.hash = '#/pay/direct' })
		await page.getByText('Waiting for payment').waitFor()
		await page.goBack()
		await page.locator('article').waitFor()
		await page.getByRole('button', { name: /Back/ }).click()
		await page.getByRole('tab', { name: 'Sats' }).waitFor()
		assert.equal(new URL(page.url()).pathname, '/cafe')
	},
	async PIN_LEAVE({ page, fixture }) {
		fixture.store.cashier_pin_set = true
		let release
		const held = new Promise((resolve) => { release = resolve })
		fixture.intercept = async (c) => { if (c.path.endsWith('/pin')) await held }
		await open(page, '#/history')
		for (const digit of '2468') await key(page, digit)
		await page.getByRole('button', { name: 'Show sales' }).click()
		await page.getByRole('button', { name: 'Checking' }).waitFor()
		await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pagehide', { persisted: true })))
		const done = page.waitForResponse((r) => r.url().endsWith('/pin'))
		release()
		await done
		await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true })))
		await page.getByRole('button', { name: 'Show sales' }).waitFor()
		assert(await page.getByRole('button', { name: 'Show sales' }).isDisabled())
	},
	async DARK_CONTROLS({ page, fixture }) {
		const errors = []
		await page.emulateMedia({ colorScheme: 'dark' })
		fixture.sales.set('dark', sale('dark'))
		await open(page, '#/pay/dark')
		const copy = page.getByRole('button', { name: 'Copy', exact: true })
		await copy.hover()
		const background = await copy.evaluate(async (el) => {
			await Promise.all(el.getAnimations().map((animation) => animation.finished))
			return getComputedStyle(el).backgroundColor
		})
		if (Number(background.match(/\d+/)?.[0]) > 140) errors.push('Dark hover turned the Copy background pale')
		fixture.store.cashier_pin_set = true
		fixture.intercept = (c) => c.path.endsWith('/pin') ? { status: 401, json: { error: 'wrong PIN', retry_after: 120 } } : null
		await open(page, '#/history')
		await page.reload()
		for (const n of '1234') await key(page, n)
		await page.getByRole('button', { name: 'Show sales' }).click()
		await page.getByText('Wrong PIN.', { exact: false }).waitFor()
		const digit = page.getByRole('button', { name: 'Add 1', exact: true })
		assert(await digit.isDisabled())
		if (await digit.evaluate((el) => Number(getComputedStyle(el).opacity)) >= 1) errors.push('Locked keypad still looks enabled')
		finish(errors)
	},
	async KEYPAD_TYPE({ page }) {
		await open(page, '')
		const size = await page.getByRole('button', { name: 'Add 1', exact: true })
			.evaluate((el) => parseFloat(getComputedStyle(el).fontSize))
		assert(size >= 28, 'Keypad digit rendered at ' + size + 'px')
		const chargeFont = await page.getByRole('button', { name: 'Charge' }).evaluate((el) => getComputedStyle(el).fontFamily)
		assert.match(chargeFont, /Bebas Neue/)
	},
	async COPY_LEGIBILITY({ page, fixture }) {
		await page.emulateMedia({ colorScheme: 'light' })
		const row = sale('copy')
		fixture.sales.set(row.id, row)
		await page.addInitScript(() => {
			Object.defineProperty(navigator, 'clipboard', { value: { writeText: async () => {} } })
		})
		await open(page, '#/pay/copy')
		const control = page.getByRole('button', { name: 'Copy Lightning invoice' })
		const ratio = await control.evaluate((button) => {
			function luminance(rgb) {
				const [r, g, b] = rgb.match(/[\d.]+/g).map(Number).map((c) => {
					const x = c / 255
					return x <= 0.04045 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4
				})
				return r * 0.2126 + g * 0.7152 + b * 0.0722 + 0.05
			}
			return luminance(getComputedStyle(button).backgroundColor) /
				luminance(getComputedStyle(button.querySelector('span span')).color)
		})
		assert(ratio >= 4.5, 'Copy detail label contrast ' + ratio.toFixed(2))
		await control.click()
		await page.getByRole('status').filter({ hasText: 'Lightning invoice copied' }).waitFor()
	},
	async TYPE_AND_NAV({ page }) {
		await open(page, '')
		const fiat = page.getByRole('tab', { name: 'USD' })
		await fiat.focus()
		await page.keyboard.press('ArrowRight')
		assert.equal(await page.getByRole('tab', { name: 'Sats' }).getAttribute('aria-selected'), 'true')
		await page.keyboard.press('Home')
		assert.equal(await fiat.getAttribute('aria-selected'), 'true')
		await page.getByRole('link', { name: 'Settings' }).click()
		assert.match(await page.title(), /Settings/)
		await page.getByRole('link', { name: /Back/ }).click()
		assert.match(await page.title(), /Coffee shop/)
	},
	async NARROW_CONTENT({ page, fixture }) {
		const errors = []
		await page.setViewportSize({ width: 320, height: 568 })
		fixture.store.name = longName
		const row = sale('long', { amount_sat: 999999999, memo: note, ...paid, received_sat: 999999999 })
		fixture.sales.set('long', row)
		await open(page, '#/settings')
		await page.getByText(longName).waitFor()
		await layout(page, 'settings', errors)
		await open(page, '#/receipt/long')
		await page.locator('article').waitFor()
		await layout(page, 'receipt-long', errors)
		const live = sale('live', { amount_sat: row.amount_sat, memo: note })
		fixture.sales.set(live.id, live)
		await open(page, '#/pay/live')
		await page.getByText(note).waitFor()
		await layout(page, 'pay-long', errors)
		const qr = await page.getByRole('img', { name: 'Lightning payment code' }).boundingBox()
		if (!qr || qr.x < 0 || qr.x + qr.width > 321) errors.push('Payment QR clipped')
		assert.equal(await decodeQr(page), live.lightning_request)
		live.state = 'in-progress'
		live.progress = 'onchain'
		await page.getByText(/in the mempool/).waitFor()
		await layout(page, 'pending-long', errors)
		await open(page, '')
		await page.getByRole('tab', { name: 'Sats' }).click()
		for (let i = 0; i < 9; i++) await key(page, '9')
		await layout(page, 'keypad-long', errors)
		await page.setViewportSize({ width: 568, height: 320 })
		const charge = page.getByRole('button', { name: 'Charge' })
		const bounds = await charge.boundingBox()
		if (!bounds || bounds.y < 0 || bounds.y + bounds.height > 321) errors.push('Charge outside landscape viewport')
		await page.getByRole('textbox').fill('Landscape memo')
		await charge.click()
		await page.getByText('Landscape memo').waitFor()
		assert.equal(fixture.sales.get('sale-3').amount_sat, 999999999)
		finish(errors)
	},
	async PARTIAL_EXPIRY({ page, fixture }) {
		const row = sale('partial-expiry', { state: 'open', amount_sat: 60000, received_sat: 15000 })
		fixture.sales.set(row.id, row)
		await open(page, '#/pay/partial-expiry')
		await page.getByText(/45,000 sats short/).waitFor()
		row.state = 'expired'
		await page.getByRole('heading', { name: 'Expired' }).or(page.getByText('Expired', { exact: true })).first().waitFor()
		const detail = await page.locator('main').innerText()
		assert.match(detail, /15,000 sats/)
		assert.doesNotMatch(detail, /without payment/)
		await page.getByRole('tab', { name: 'Sats' }).waitFor()
		await open(page, '#/receipt/partial-expiry')
		await page.locator('dd', { hasText: '15,000 sats' }).waitFor()
	},
	async RECEIPT_DETAILS({ page, fixture }) {
		const errors = []
		fixture.sales.set('detail', sale('detail', { ...paid, fiat_amount: '60.00', currency: 'USD', memo: note }))
		await open(page, '#/receipt/detail')
		const paper = page.locator('article')
		await paper.waitFor()
		for (const [label, value] of [['Received', '70,000 sats'], ['Overpaid by', '10,000 sats']]) {
			const field = paper.locator('dl > div').filter({ has: page.locator('dt', { hasText: label }) })
			if (!(await field.count()) || !(await field.innerText()).includes(value)) errors.push(label + ' absent or incorrect')
		}
		if (!(await paper.innerText()).includes('$60.00')) errors.push('Missing sale total')
		await page.emulateMedia({ media: 'print', colorScheme: 'dark' })
		await page.setViewportSize({ width: 302, height: 700 })
		await layout(page, 'receipt-print', errors)
		if (await page.getByRole('button', { name: 'Print' }).isVisible()) errors.push('Print toolbar on paper')
		const background = await page.locator('main').evaluate((el) => getComputedStyle(el).backgroundColor)
		if (!['rgb(255, 255, 255)', 'rgba(0, 0, 0, 0)'].includes(background)) errors.push('Screen background printed: ' + background)
		const dir = page.context().auditDirectory
		if (dir && page.context().browser().browserType().name() === 'chromium') {
			await page.pdf({ path: join(dir, 'roll.pdf'), width: '80mm', height: '200mm', printBackground: true })
		}
		finish(errors)
	},
	async CASHIER_HISTORY({ page, fixture }) {
		const errors = []
		fixture.store.cashier_pin_set = true
		for (const [id, state] of [['paid', 'paid'], ['waiting', 'open'], ['seen', 'in-progress']]) {
			fixture.sales.set(id, sale(id, { state, warnings: state === 'in-progress' ? ['low-fee'] : [] }))
		}
		await open(page, '')
		await page.getByRole('link', { name: 'Recent sales' }).click()
		await page.evaluate(() => localStorage.setItem('bullnym:cafe:sales', JSON.stringify(['paid', 'waiting', 'seen'])))
		for (const n of '2468') await key(page, n)
		await page.getByRole('button', { name: 'Show sales' }).click()
		const row = page.locator('a[href="#/receipt/paid"]')
		await row.waitFor()
		if (!(await row.innerText()).includes('Paid')) errors.push('Settled sale has no status text')
		if ((await page.locator('main').innerText()).includes('low-fee')) errors.push('Internal warning code visible')
		await page.emulateMedia({ colorScheme: 'dark' })
		await row.hover()
		const fill = await row.evaluate(async (el) => {
			await Promise.all(el.getAnimations().map((animation) => animation.finished))
			return getComputedStyle(el).backgroundColor
		})
		if (Number(fill.match(/\d+/)?.[0]) > 140) errors.push('Dark history hover has a pale background')
		await checkpoint(page, 'history-rows', true)
		await page.locator('a[href="#/receipt/waiting"]').click()
		await page.getByText('Waiting for payment').first().waitFor()
		const resume = page.getByRole('link', { name: 'View payment' })
		if (!await resume.count()) errors.push('Open receipt has no path to its payment code')
		else {
			await resume.click()
			await page.getByRole('img', { name: /payment code/ }).waitFor()
			assert.equal(new URL(page.url()).hash, '#/pay/waiting')
		}
		finish(errors)
	},
	async PAYMENT_FEEDBACK({ page, fixture }) {
		const errors = []
		const row = sale('feedback')
		fixture.sales.set(row.id, row)
		await open(page, '#/pay/feedback')
		await page.getByRole('button', { name: 'Bitcoin', exact: true }).click()
		await checkpoint(page, 'confirmation', true)
		await page.keyboard.press('Escape')
		await page.getByRole('dialog').waitFor({ state: 'hidden' })
		if ((await page.evaluate(() => document.activeElement.textContent.trim())) !== 'Bitcoin') errors.push('Escape lost keyboard focus')
		Object.assign(row, { state: 'in-progress', progress: 'ark' })
		await page.getByText(/Ark payment is being delivered/).waitFor()
		const motion = await page.locator('main svg path').evaluateAll((paths) => paths.some((p) => getComputedStyle(p).animationName !== 'none'))
		if (motion) errors.push('Pending spinner animates with reduced motion')
		assert.doesNotMatch(await page.locator('main').innerText(), /completes on its own/)
		await checkpoint(page, 'ark-pending', true)
		Object.assign(row, paid)
		await page.getByRole('button', { name: 'Receipt' }).waitFor()
		if (!await page.getByRole('heading', { name: 'Paid', exact: true }).count()) errors.push('Settled screen has no Paid heading')
		await checkpoint(page, 'paid', true)
		finish(errors)
	},
	async CURRENCY_CHANGE({ page, fixture }) {
		await open(page, '')
		await page.getByText(/\/BTC/).waitFor()
		await key(page, '5')
		await page.getByRole('textbox').fill('Keep this memo')
		fixture.store.currency = 'CRC'
		fixture.intercept = (c) => c.path.endsWith('/rate') ? { json: { currency: 'USD', btc_price: '100000' } } : null
		await page.evaluate(() => window.dispatchEvent(new Event('focus')))
		await page.getByRole('tab', { name: 'CRC' }).waitFor()
		assert(await page.getByRole('button', { name: 'Charge' }).isDisabled(), 'Old USD amount became a charge in CRC')
		await key(page, '2')
		assert(await page.getByRole('button', { name: 'Charge' }).isDisabled(), 'A USD quote enabled CRC')
		assert.equal(await page.getByRole('textbox').inputValue(), 'Keep this memo')
		fixture.intercept = undefined
		await page.evaluate(() => window.dispatchEvent(new Event('online')))
		await page.getByRole('button', { name: 'Charge' }).click()
		await page.getByText('Waiting for payment').waitFor()
		assert.equal(fixture.sales.get('sale-1').fiat_amount, '2')
	},
}
