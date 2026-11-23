import assert from 'node:assert/strict'
import { origin, sale } from './fixture.mjs'
import { checkpoint } from './checkpoint.mjs'

// Each named row is a reusable sale fixture. Expected UI is checked in all
// three views, including the subset of actions that the current stage allows.
function examples(group) {
	const rows = []
	const add = (name, data) => rows.push(sale(name, { memo: name, ...data }))
	const address = { onchain_address: 'bcrt1qfixture', onchain_request: 'bitcoin:bcrt1qfixture?amount=0.0006' }
	const past = new Date(Date.now() - 60000).toISOString()
	if (group === 'OPEN') {
		for (const amount of [49999, 50000, 50001]) {
			for (const assigned of [false, true]) {
				for (const overdue of [false, true]) {
					add('threshold-' + [amount, assigned, overdue].join('-'), {
						amount_sat: amount,
						...(assigned ? { ...address, onchain_request: 'bitcoin:bcrt1qfixture?amount=' + (amount / 1e8).toFixed(8) } : {}),
						...(overdue ? { expires_at: past } : {}),
					})
				}
			}
		}
		for (const notice of [null, 'payment-replaced', 'payment-dropped']) {
			for (const [received_sat, seen_sat] of [[0, 0], [0, 5000], [10000, 0], [10000, 5000]]) {
				add('partial-' + [notice || 'none', received_sat, seen_sat].join('-'), { ...address, notice, received_sat, seen_sat })
			}
		}
	}
	if (group === 'PENDING') {
		const optional = ['low-fee', 'unconfirmed-parents', 'replaced']
		for (let mask = 0; mask < 8; mask++) {
			const warnings = ['not-guaranteed', ...optional.filter((_, i) => mask & (1 << i))]
			add('chain-warnings-' + mask, { ...address, state: 'in-progress', progress: 'onchain',
				seen_sat: 50000, received_sat: 10000, warnings, expires_at: past })
		}
		for (const seen_sat of [0, 15000, 60000]) {
			add('ark-observation-' + seen_sat, { ...address, state: 'in-progress', progress: 'ark',
				seen_sat, warnings: seen_sat ? ['not-guaranteed'] : [], expires_at: past })
		}
	}
	if (group === 'PAID') {
		for (const rail of [null, 'lightning', 'ark', 'onchain']) {
			for (const received_sat of [60000, 80000]) {
				add('paid-' + (rail || 'unlabeled') + '-' + received_sat, {
					...address, state: 'paid', paid_rail: rail, paid_at: new Date().toISOString(), received_sat,
				})
			}
		}
		for (const received_sat of [0, 40000]) {
			add('paid-reorg-' + received_sat, { ...address, state: 'paid', paid_rail: 'onchain',
				paid_at: new Date().toISOString(), received_sat, seen_sat: 60000 - received_sat })
		}
	}
	if (group === 'ENDED') {
		for (const state of ['expired', 'canceled']) {
			for (const [received_sat, seen_sat] of [[0, 0], [0, 5000], [10000, 0], [10000, 5000]]) {
				add('ended-' + [state, received_sat, seen_sat].join('-'), { ...address, state,
					received_sat, seen_sat, late_payment: seen_sat + received_sat > 0 })
			}
		}
	}
	return rows
}

const stage = (row) => {
	if (row.state === 'open') return 'Waiting for payment'
	if (row.state === 'in-progress') return row.progress === 'ark' ? 'In progress' : 'In progress, not guaranteed'
	if (row.state === 'paid') return 'Paid'
	return (row.state === 'expired' ? 'Expired' : 'Canceled') + (row.late_payment ? ' · late payment' : '')
}
const sats = (n) => n.toLocaleString('en-US') + ' sats'
const method = { lightning: 'Lightning', ark: 'Ark', onchain: 'Bitcoin' }

async function verifyPayment(page, row) {
	const control = (name) => page.getByRole('button', { name, exact: true })
	const live = row.state === 'open'
	if (live) {
		await control('Cancel sale').waitFor()
		await page.getByText(row.memo, { exact: true }).waitFor()
		const overdue = Date.parse(row.expires_at) <= Date.now()
		assert.equal(await page.getByText(/Checking the payment status/).count(), Number(overdue))
		assert.equal(await control('Bitcoin').count(), Number(!overdue && !row.onchain_request && row.amount_sat >= 50000))
		await page.getByRole('img', { name: 'Lightning payment code' }).waitFor()
		if (row.onchain_request) {
			await page.getByRole('tab', { name: 'Bitcoin' }).click()
			await page.getByRole('img', { name: 'Bitcoin payment code' }).waitFor()
			await control('Copy Bitcoin address').waitFor()
		}
		const observed = row.received_sat + row.seen_sat
		if (observed) {
			const partial = page.getByText(sats(row.amount_sat - observed) + ' short', { exact: false })
			await partial.waitFor()
			assert((await partial.innerText()).includes(sats(row.received_sat) + ' received.'))
		}
		if (row.notice) await page.getByText(row.notice === 'payment-dropped' ? /Payment dropped/ : /Payment replaced/).waitFor()
	} else if (row.state === 'in-progress') {
		await page.getByText(row.progress === 'ark' ? /Ark payment is being delivered/ : /in the mempool/).waitFor()
		if (row.progress === 'onchain') {
			for (const [warning, text] of [['low-fee',/fee is low/], ['unconfirmed-parents',/It spends coins/], ['replaced',/was replaced by one/]]) {
				assert.equal(await page.getByText(text).count(), Number(row.warnings.includes(warning)))
			}
		}
		if (row.seen_sat) await page.getByText(sats(row.seen_sat) + ' unconfirmed.', { exact: false }).waitFor()
	} else if (row.state === 'paid') {
		await page.getByRole('heading', { name: 'Paid', exact: true }).waitFor()
		await control('Receipt').waitFor()
		assert.equal(await page.getByText(/^via /).count(), Number(Boolean(row.paid_rail)))
		if (row.paid_rail) await page.getByText('via ' + method[row.paid_rail], { exact: true }).waitFor()
		if (row.received_sat < row.amount_sat) await page.getByText('Currently received ' + sats(row.received_sat), { exact: false }).waitFor()
		if (row.received_sat > row.amount_sat) await page.getByText(/Paid more than the sale/).waitFor()
	} else {
		await page.getByRole('heading', { name: row.late_payment ? 'Late payment' : stage(row), exact: true }).waitFor()
		if (row.late_payment) await page.getByText(sats(row.received_sat) + ' received.', { exact: false }).waitFor()
	}
	assert.equal(await control('Cancel sale').count(), Number(live))
	if (!live) assert.equal(await page.getByRole('img', { name: /payment code/i }).count(), 0)
}

async function verifyReceipt(page, row) {
	const paper = page.locator('article')
	await paper.getByText(row.memo, { exact: true }).waitFor()
	const field = (name) => paper.locator('dt').filter({ hasText: new RegExp('^' + name + '$') }).locator('+ dd')
	assert.equal(await field('Status').innerText(), stage(row))
	assert.equal(await field('Received').innerText(), sats(row.received_sat))
	assert.equal(await field('Unconfirmed').count(), Number(row.seen_sat > 0))
	if (row.seen_sat) assert.equal(await field('Unconfirmed').innerText(), sats(row.seen_sat))
	assert.equal(await field('Method').count(), Number(Boolean(row.paid_rail)))
	assert.equal(await field('Overpaid by').count(), Number(row.received_sat > row.amount_sat))
	assert.equal(await page.getByRole('button', { name: 'Print' }).count(), Number(row.state === 'paid'))
	assert.equal(await page.getByRole('link', { name: 'View payment' }).count(), Number(['open','in-progress'].includes(row.state)))
}

export const saleStates = Object.fromEntries(['OPEN','PENDING','PAID','ENDED'].map((group) => [
	'SALE_' + group, async ({ page, fixture }) => {
		const rows = examples(group)
		for (const row of rows) fixture.sales.set(row.id, row)
		fixture.store.cashier_pin_set = true
		await page.addInitScript((ids) => localStorage.setItem('bullnym:cafe:sales', JSON.stringify(ids)), rows.map((row) => row.id))
		await page.goto(origin + '/cafe#/history')
		for (const digit of '2468') await page.getByRole('button', { name: 'Add ' + digit, exact: true }).click()
		await page.getByRole('button', { name: 'Show sales' }).click()
		for (const row of rows) await page.locator('a[href="#/receipt/' + row.id + '"]').getByText(stage(row), { exact: true }).waitFor()
		for (const row of rows) {
			try {
				await page.goto(origin + '/cafe#/pay/' + row.id)
				await verifyPayment(page, row)
				await checkpoint(page, row.id + '-pay')
				await page.goto(origin + '/cafe#/receipt/' + row.id)
				await verifyReceipt(page, row)
				await checkpoint(page, row.id + '-receipt')
			} catch (e) { throw Error(row.id + ' (' + page.url() + '): ' + e) }
		}
	},
]))
