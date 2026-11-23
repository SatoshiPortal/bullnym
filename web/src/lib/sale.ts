// Presentation of a Sale: labels, tones, warnings and the receipt number.
//
// A payment the service has seen but cannot count yet is "in progress":
// amber, with plain words that it is not guaranteed. Only a paid sale is
// green.
import { currencyPrecision, formatFiat } from './money'
import type { PaidRail, Sale, SaleState, Warning } from './api'

export function railLabel(rail: PaidRail | null): string {
	switch (rail) {
		case 'lightning':
			return 'Lightning'
		case 'ark':
			return 'Ark'
		case 'onchain':
			return 'Bitcoin'
		default:
			return '—'
	}
}

export function stateLabel(state: SaleState): string {
	switch (state) {
		case 'open':
			return 'Waiting for payment'
		case 'in-progress':
			return 'In progress'
		case 'paid':
			return 'Paid'
		case 'expired':
			return 'Expired'
		case 'canceled':
			return 'Canceled'
	}
}

/** The stage of a sale, as the cashier, the payer and the owner see it. */
export function stageLabel(sale: Sale): string {
	switch (sale.state) {
		case 'in-progress':
			return sale.progress === 'ark' ? 'In progress' : 'In progress, not guaranteed'
		case 'expired':
		case 'canceled':
			return sale.late_payment ? `${stateLabel(sale.state)} · late payment` : stateLabel(sale.state)
		default:
			return stateLabel(sale.state)
	}
}

/** The always-on warning while an on-chain payment is unconfirmed. */
export const NOT_GUARANTEED =
	"Payment seen, not confirmed yet. It isn't guaranteed until 1 confirmation, which usually takes 10 to 60 minutes."

/** The warnings, in plain words. */
export function warningText(w: Warning): string {
	switch (w) {
		case 'not-guaranteed':
			return NOT_GUARANTEED
		case 'low-fee':
			return 'The fee is low: it may take hours to confirm, or drop out.'
		case 'unconfirmed-parents':
			return 'It spends coins that are not confirmed either: if they are replaced, this payment disappears too.'
		case 'replaced':
			return 'A payment to this sale was replaced by one that does not pay it.'
	}
}

/** What the amber in-progress panel says. */
export function progressText(sale: Sale): { title: string; lines: string[] } {
	if (sale.progress === 'ark') {
		return {
			title: 'The Ark payment is being delivered',
			lines: ['Waiting for the wallet service to confirm delivery.'],
		}
	}
	const lines = sale.warnings.map(warningText)
	if (!sale.warnings.includes('not-guaranteed')) lines.unshift(NOT_GUARANTEED)
	return { title: 'The bitcoin payment is in the mempool', lines }
}

/** What happened to a payment that went away. */
export function noticeText(sale: Sale): string | null {
	switch (sale.notice) {
		case 'payment-replaced':
			return 'Payment replaced: the payer replaced it with a transaction that does not pay this sale.'
		case 'payment-dropped':
			return 'Payment dropped: it left the mempool without confirming.'
		default:
			return null
	}
}

/** The sats still missing from an open sale with a partial payment in sight. */
export function missingSats(sale: Sale): number {
	if (sale.state !== 'open') return 0
	const inSight = sale.received_sat + sale.seen_sat
	return inSight > 0 && inSight < sale.amount_sat ? sale.amount_sat - inSight : 0
}

/** Left-border tone for a history row, as in nostr-pos's TransactionSheet. */
export function stateBorderTone(state: SaleState): string {
	switch (state) {
		case 'paid':
			return 'border-[#1f513a]'
		case 'in-progress':
		case 'open':
			return 'border-[#c99a2e]'
		default:
			return 'border-[#a9362f]'
	}
}

/** The sale is still waiting for its payment. */
export function isLive(state: SaleState): boolean {
	return state === 'open' || state === 'in-progress'
}

/**
 * Whether a snapshot of sale `id` replaces the one shown. A screen never
 * goes back: a snapshot of another sale, an older one, or a live one after
 * the sale ended, from a late or reordered response, is ignored.
 */
export function acceptSale(current: Sale | null, next: Sale, id: string): boolean {
	if (next.id !== id) return false
	if (!current) return true
	if (!isLive(current.state) && next.state !== current.state) return false
	if (current.onchain_address && !next.onchain_address) return false
	if (!current.updated_at) return true
	if (!next.updated_at) return false
	// The service emits UTC timestamps. Removing Z orders fractional seconds
	// at their full PostgreSQL precision, including submillisecond updates.
	return next.updated_at.replace(/Z$/, '') > current.updated_at.replace(/Z$/, '')
}

/** Whether Bitcoin can be added to the sale now: open, without an address, and at the store's minimum. */
export function canAddBitcoin(sale: Sale | null, onchainMin: number | null): boolean {
	return sale !== null && sale.state === 'open' && !sale.onchain_request && onchainMin !== null && sale.amount_sat >= onchainMin
}

/** "12-3456" from the last six characters of the sale ID, as in bullnym. */
export function receiptNumber(id: string): string {
	const clean = id.replace(/-/g, '').toUpperCase()
	const last6 = clean.slice(-6).padStart(6, '0')
	return `${last6.slice(0, 2)}-${last6.slice(2)}`
}

/** The headline amount: fiat when the sale was keyed in fiat, else sats. */
export function saleAmountLabel(sale: Sale): string {
	if (sale.fiat_amount !== null && sale.currency) {
		const fraction = sale.fiat_amount.split('.')[1] ?? ''
		const places = Math.max(currencyPrecision(sale.currency), /[1-9]/.test(fraction) ? fraction.length : 0)
		return formatFiat(sale.fiat_amount, sale.currency, places)
	}
	return `${new Intl.NumberFormat().format(sale.amount_sat)} sats`
}

/** Remaining time as "m:ss", or null once expired. */
export function countdown(expiresAt: string, nowMs: number): string | null {
	const seconds = Math.ceil((Date.parse(expiresAt) - nowMs) / 1000)
	if (!Number.isFinite(seconds) || seconds <= 0) return null
	return `${Math.floor(seconds / 60)}:${(seconds % 60).toString().padStart(2, '0')}`
}

/** "YYYY-MM-DD" for a local date. */
export function isoDate(d: Date): string {
	const pad = (n: number) => n.toString().padStart(2, '0')
	return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
}
