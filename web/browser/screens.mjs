import assert from 'node:assert/strict'
import { sale, origin } from './fixture.mjs'

// Hold a real browser fetch at the named phase, including states too brief for a screenshot.
function latch() {
	let release
	const promise = new Promise((resolve) => { release = resolve })
	return { promise, release }
}

export async function screenStates({ page, fixture }, inspect) {
	const path = (hash = '') => origin + '/cafe' + hash
	await page.goto(origin)
	await page.getByRole('heading', { name: /store's link/ }).waitFor()
	await inspect('no-store-link')
	await page.goto(origin + '/unknown')
	await page.getByText(/no store at this link/).waitFor()
	await inspect('unknown-store')
	const first = latch()
	fixture.intercept = async (call) => {
		if (call.path === '/api/v1/checkout/cafe') await first.promise
	}
	await page.goto(path())
	await page.getByText('Loading', { exact: true }).waitFor()
	await inspect('store-loading')
	first.release()
	await page.getByText(/\/BTC/).waitFor()
	await inspect('keypad')
	await page.getByRole('link', { name: 'Settings' }).click()
	await page.getByRole('heading', { name: 'Settings' }).waitFor()
	await inspect('store-settings')

	await page.goto(path('#/history'))
	await page.getByText(/owner has not set/).waitFor()
	await inspect('history-unconfigured')
	fixture.store.cashier_pin_set = true
	await page.reload()
	await page.getByText('Cashier PIN').waitFor()
	await inspect('pin-locked')
	const key = (c) => page.getByRole('button', { name: 'Add ' + c, exact: true }).click()
	for (const c of '999999999999') await key(c)
	await inspect('pin-max-digits')
	const checking = latch()
	fixture.intercept = async (call) => {
		if (call.path.endsWith('/pin')) { await checking.promise; return { status: 401, json: { error: 'wrong PIN', retry_after: 60 } } }
	}
	await page.getByRole('button', { name: 'Show sales' }).click()
	await page.getByRole('button', { name: 'Checking' }).waitFor()
	await inspect('pin-checking')
	checking.release()
	await page.getByText(/Wrong PIN/).waitFor()
	await inspect('pin-wait')
	fixture.intercept = undefined

	const states = [
		['small', { amount_sat: 400 }, /offered from/],
		['partial', { seen_sat: 100 }, /short of the amount/],
		['reopened', { notice: 'payment-dropped' }, /Payment dropped/],
		['bitcoin', { onchain_address: 'bcrt1qexample', onchain_request: 'bitcoin:bcrt1qexample?amount=0.0006' }, /Waiting for payment/],
		['chain', { state: 'in-progress', progress: 'onchain', warnings: ['low-fee', 'unconfirmed-parents'] }, /fee is low/],
		['settled', { state: 'paid', paid_rail: 'lightning', received_sat: 60000 }, /via Lightning/],
		['expired', { state: 'expired' }, /Returning/],
		['canceled', { state: 'canceled' }, /sale was canceled/],
		['late', { state: 'expired', late_payment: true, received_sat: 60000 }, /payment arrived after/],
	]
	for (const [id, data, text] of states) {
		fixture.sales.set(id, sale(id, data))
		await page.goto(path('#/pay/' + id))
		await page.getByText(text).first().waitFor()
		if (id === 'bitcoin') await page.getByRole('tab', { name: 'Bitcoin' }).click()
		if (['small', 'partial', 'reopened', 'bitcoin'].includes(id)) await page.getByRole('img', { name: /payment code/ }).waitFor()
		await inspect('pay-' + id)
	}
	for (const [id, expected] of [['missing', 'Receipt not found.'], ['small', 'Waiting for payment'], ['chain', 'In progress, not guaranteed'], ['late', 'Expired · late payment'], ['settled', 'Receipt ready.']]) {
		await page.goto(path('#/receipt/' + id))
		await page.getByText(expected, { exact: true }).first().waitFor()
		await inspect('receipt-' + id)
	}
	const slow = latch()
	fixture.intercept = async (call) => {
		if (call.path.endsWith('/slow')) { await slow.promise; return { status: 503 } }
	}
	await page.goto(path('#/receipt/slow'))
	await page.getByText('Loading receipt').waitFor()
	await inspect('receipt-loading')
	slow.release()
	await page.getByText('Reconnecting…').waitFor()
	await inspect('receipt-reconnecting')
	fixture.intercept = undefined
	await page.goto(path('#/pay/not-found'))
	await page.getByText('This sale could not be found.').waitFor()
	await inspect('sale-missing')
	assert.equal(fixture.calls.filter((c) => c.path.endsWith('/sales') && c.method === 'POST').length, 0)
}
