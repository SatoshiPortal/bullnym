import { describe, expect, it } from 'vitest'
import type { Sale } from './api'
import {
	NOT_GUARANTEED,
	acceptSale,
	canAddBitcoin,
	countdown,
	isLive,
	missingSats,
	noticeText,
	progressText,
	railLabel,
	receiptNumber,
	saleAmountLabel,
	stageLabel,
	stateLabel,
} from './sale'

function sale(overrides: Partial<Sale> = {}): Sale {
	return {
		id: '4f1c9a2e-0000-4000-8000-1234567890ab',
		created_at: '2026-11-20T15:04:05Z',
		expires_at: '2026-11-20T15:14:05Z',
		state: 'open',
		amount_sat: 35000,
		fiat_amount: '12.50',
		currency: 'USD',
		btc_price: '62500.00',
		memo: null,
		lightning_request: 'lightning:lnbc1',
		onchain_request: null,
		lightning_invoice: 'lnbc1',
		onchain_address: null,
		paid_at: null,
		paid_rail: null,
		received_sat: 0,
		seen_sat: 0,
		progress: null,
		warnings: [],
		notice: null,
		updated_at: '2026-11-22T10:00:00Z',
		late_payment: false,
		...overrides,
	}
}

describe('sale presentation', () => {
	it('labels each rail', () => {
		expect(railLabel('lightning')).toBe('Lightning')
		expect(railLabel('ark')).toBe('Ark')
		expect(railLabel('onchain')).toBe('Bitcoin')
		expect(railLabel(null)).toBe('—')
	})

	it('labels each state', () => {
		expect(stateLabel('in-progress')).toBe('In progress')
		expect(stateLabel('paid')).toBe('Paid')
		expect(stateLabel('canceled')).toBe('Canceled')
	})

	it('never calls a payment in progress paid', () => {
		expect(stageLabel(sale({ state: 'in-progress', progress: 'onchain' }))).toBe('In progress, not guaranteed')
		expect(stageLabel(sale({ state: 'in-progress', progress: 'ark' }))).toBe('In progress')
		// Paid at 1 confirmation is final: there is no later stage.
		expect(stageLabel(sale({ state: 'paid', paid_rail: 'onchain' }))).toBe('Paid')
		expect(stageLabel(sale({ state: 'expired', late_payment: true }))).toBe('Expired · late payment')
	})

	it('states the risk of an unconfirmed payment in plain words', () => {
		const text = progressText(sale({ state: 'in-progress', progress: 'onchain', warnings: ['not-guaranteed', 'low-fee', 'unconfirmed-parents'] }))
		expect(text.lines[0]).toBe(NOT_GUARANTEED)
		expect(text.lines).toHaveLength(3)
		// The not-guaranteed line is there even if the list lacks it.
		expect(progressText(sale({ state: 'in-progress', progress: 'onchain' })).lines).toEqual([NOT_GUARANTEED])
		expect(noticeText(sale({ notice: 'payment-replaced' }))).toMatch(/^Payment replaced/)
		expect(noticeText(sale())).toBeNull()
	})

	it('shows what a partial payment is missing', () => {
		expect(missingSats(sale({ seen_sat: 20000 }))).toBe(15000)
		expect(missingSats(sale())).toBe(0)
		expect(missingSats(sale({ state: 'in-progress', seen_sat: 35000 }))).toBe(0)
	})

	it('treats open and in-progress sales as live', () => {
		expect(isLive('open')).toBe(true)
		expect(isLive('in-progress')).toBe(true)
		expect(isLive('paid')).toBe(false)
		expect(isLive('expired')).toBe(false)
	})

	it('derives the receipt number from the sale ID', () => {
		expect(receiptNumber(sale().id)).toBe('78-90AB')
	})

	it('headlines fiat for a fiat sale and sats otherwise', () => {
		const fmt = (a: string, c: string) => `${c} ${a}`
		expect(saleAmountLabel(sale(), fmt)).toBe('USD 12.50')
		expect(saleAmountLabel(sale({ fiat_amount: null, currency: null }), fmt)).toBe(
			`${new Intl.NumberFormat().format(35000)} sats`,
		)
	})

	it('counts down to expiry', () => {
		const expires = Date.parse('2026-11-20T15:14:05Z')
		expect(countdown('2026-11-20T15:14:05Z', expires - 125_000)).toBe('2:05')
		expect(countdown('2026-11-20T15:14:05Z', expires)).toBeNull()
	})
})

describe('following a sale', () => {
	const id = sale().id
	const at = (t: string) => `2026-11-22T10:00:${t}Z`

	it('never goes back on a late or reordered response', () => {
		const open = sale({ updated_at: at('00') })
		const progress = sale({ state: 'in-progress', progress: 'onchain', updated_at: at('05') })
		expect(acceptSale(null, open, id)).toBe(true)
		expect(acceptSale(open, progress, id)).toBe(true)
		// A delayed snapshot of the open state, older than the one shown.
		expect(acceptSale(progress, open, id)).toBe(false)
		// A payment replaced: back to open, but newer, so shown.
		expect(acceptSale(progress, sale({ notice: 'payment-replaced', updated_at: at('09') }), id)).toBe(true)
		// An ended sale never shows live again, whatever the response.
		const paid = sale({ state: 'paid', updated_at: at('10') })
		expect(acceptSale(paid, sale({ updated_at: at('11') }), id)).toBe(false)
		// A later snapshot of the paid sale, with its rail or more credit, is.
		expect(acceptSale(paid, sale({ state: 'paid', paid_rail: 'ark', updated_at: at('12') }), id)).toBe(true)
		// Another sale's response, for a screen that moved on.
		expect(acceptSale(open, sale({ id: 'another', updated_at: at('20') }), id)).toBe(false)
	})

	it('offers Bitcoin only while the sale is open, above the minimum, without an address', () => {
		expect(canAddBitcoin(sale({ amount_sat: 50_000 }), 50_000)).toBe(true)
		expect(canAddBitcoin(sale({ amount_sat: 49_999 }), 50_000)).toBe(false)
		expect(canAddBitcoin(sale({ amount_sat: 60_000, onchain_request: 'bitcoin:x?amount=1' }), 50_000)).toBe(false)
		// The sale settled while the confirmation was open: it closes.
		for (const state of ['in-progress', 'paid', 'expired', 'canceled'] as const) {
			expect(canAddBitcoin(sale({ amount_sat: 60_000, state }), 50_000)).toBe(false)
		}
		expect(canAddBitcoin(null, 50_000)).toBe(false)
		expect(canAddBitcoin(sale({ amount_sat: 60_000 }), null)).toBe(false)
	})
})
