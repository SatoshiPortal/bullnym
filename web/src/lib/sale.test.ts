import { describe, expect, it } from 'vitest'
import type { Sale } from './api'
import {
	NOT_GUARANTEED,
	countdown,
	isLive,
	missingSats,
	noticeText,
	progressText,
	railLabel,
	receiptNumber,
	saleAmountLabel,
	stageLabel,
	stageTone,
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
		payment_request: 'bitcoin:?lightning=lnbc1',
		lightning_invoice: 'lnbc1',
		onchain_address: null,
		paid_at: null,
		paid_rail: null,
		received_sat: 0,
		seen_sat: 0,
		progress: null,
		warnings: [],
		notice: null,
		final: false,
		final_at: null,
		late_payment: false,
		link: 'public',
		cashier_name: 'Front till',
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
		expect(stateLabel('payment-issue')).toBe('Payment issue')
		expect(stateLabel('canceled')).toBe('Canceled')
	})

	it('never shows a payment in progress as paid or green', () => {
		const seen = sale({ state: 'in-progress', progress: 'onchain', seen_sat: 35000, warnings: ['not-guaranteed'] })
		expect(stageLabel(seen)).toBe('In progress, not guaranteed')
		expect(stageTone(seen)).toBe('warn')
		expect(stageTone(sale({ state: 'in-progress', progress: 'ark' }))).toBe('warn')
		expect(stageTone(sale({ state: 'paid' }))).toBe('ok')
		expect(stageTone(sale({ state: 'payment-issue' }))).toBe('err')
	})

	it('states the risk of an unconfirmed payment in plain words', () => {
		const text = progressText(sale({ state: 'in-progress', progress: 'onchain', warnings: ['not-guaranteed', 'low-fee', 'unconfirmed-parents'] }))
		expect(text.lines[0]).toBe(NOT_GUARANTEED)
		expect(text.lines).toHaveLength(3)
		// The not-guaranteed line is there even if the list lacks it.
		expect(progressText(sale({ state: 'in-progress', progress: 'onchain' })).lines).toEqual([NOT_GUARANTEED])
		expect(progressText(sale({ state: 'in-progress', progress: 'ark' })).title).toBe('Ark payment in progress')
	})

	it('marks final, late and replaced payments', () => {
		expect(stageLabel(sale({ state: 'paid', final: true }))).toBe('Paid · final')
		expect(stageLabel(sale({ state: 'expired', late_payment: true }))).toBe('Expired · late payment')
		expect(noticeText(sale({ notice: 'payment-replaced' }))).toMatch(/^Payment replaced/)
		expect(noticeText(sale())).toBeNull()
	})

	it('shows what is missing from a partial payment', () => {
		expect(missingSats(sale({ seen_sat: 20000 }))).toBe(15000)
		expect(missingSats(sale())).toBe(0)
		expect(missingSats(sale({ state: 'in-progress', seen_sat: 35000 }))).toBe(0)
	})

	it('treats open and in-progress sales as live', () => {
		expect(isLive('open')).toBe(true)
		expect(isLive('in-progress')).toBe(true)
		expect(isLive('paid')).toBe(false)
		expect(isLive('payment-issue')).toBe(false)
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
