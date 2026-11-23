import { readFile } from 'node:fs/promises'
import { extname } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'

export const origin = process.env.APP_ORIGIN ?? 'https://checkout.test'
if (process.env.APP_ORIGIN && !['127.0.0.1', 'localhost', '[::1]'].includes(new URL(origin).hostname))
	throw Error('Expected a local test app')
const dist = new URL(process.env.DIST_URL ?? '../dist/', import.meta.url)
const mime = {
	'.js': 'text/javascript',
	'.css': 'text/css',
	'.png': 'image/png',
	'.woff2': 'font/woff2',
	'.webmanifest': 'application/manifest+json',
}

export function sale(id, overrides = {}) {
	const invoice = 'lnbcrt1fixture' + id
	return {
		id,
		store_nym: 'cafe',
		state: 'open',
		amount_sat: 60000,
		received_sat: 0,
		seen_sat: 0,
		created_at: new Date().toISOString(),
		expires_at: new Date(Date.now() + 600000).toISOString(),
		memo: null,
		fiat_amount: null,
		currency: null,
		btc_price: null,
		paid_at: null,
		paid_rail: null,
		...(process.env.BASELINE_API ? { payment_request: 'bitcoin:?lightning=' + invoice } : {}),
		lightning_invoice: invoice,
		lightning_request: 'lightning:' + invoice,
		onchain_request: null,
		progress: null,
		warnings: [],
		notice: null,
		late_payment: false,
		onchain_address: null,
		...overrides,
	}
}

export class Fixture {
	// Memoize the service revision of each snapshot so a delayed response
	// preserves its original timestamp. Tests may supply exact revisions.
	revisions = new Map()
	lastRevision = Date.now()
	snapshot(body) {
		if (!body?.id || !body?.state || body.updated_at) return body
		const key = JSON.stringify(body)
		if (!this.revisions.has(key)) this.revisions.set(key, new Date(++this.lastRevision).toISOString())
		return { ...body, updated_at: this.revisions.get(key) }
	}
	store = {
		name: 'Coffee shop',
		nym: 'cafe',
		currency: 'USD',
		cashier_pin_set: false,
		lightning_address: 'cafe@example.test',
		onchain_min_sat: 50000,
	}
	sales = new Map()
	calls = []
	pinChecks = []
	time = Date.now()
	async attach(context) {
		await context.route(origin + (process.env.APP_ORIGIN ? '/api/**' : '/**'), async (route) => {
			const req = route.request()
			const url = new URL(req.url())
			const path = url.pathname
			if (!path.startsWith('/api/')) {
				if (process.env.APP_ORIGIN) return route.continue()
				const ext = extname(path)
				const asset = ext === '.webmanifest' ? 'manifest.webmanifest' : ext ? path.slice(1) : 'index.html'
				const body = await readFile(new URL(asset, dist))
				return route.fulfill({ body, contentType: mime[ext] ?? 'text/html' })
			}
			const body = req.postDataJSON()
			const call = { path, query: url.search, method: req.method(), body }
			this.calls.push(call)
			const result = await this.intercept?.(call)
			const reply = result ?? (await this.respond(call))
			if (reply.abort) return route.abort()
			return route.fulfill({
				status: reply.status ?? 200,
				json: this.snapshot(reply.json ?? {}),
				headers: reply.headers,
			})
		})
	}

	async respond({ path, method, body, query }) {
		const missing = { status: 404, json: { error: 'not found' } }
		if (path === '/api/v1/checkout/other') return { json: { store: { ...this.store, nym: 'other', name: 'Other shop' } } }
		if (path === '/api/v1/checkout/cafe') return { json: { store: this.store } }
		if (path === '/api/v1/checkout/cafe/rate')
			return { json: { currency: this.store.currency, btc_price: '100000', fetched_at: new Date().toISOString() } }
		if (path.endsWith('/pin')) {
			this.pinChecks = this.pinChecks.filter((at) => at > this.time - 3600000)
			if (this.pinChecks.length >= 10) return { status: 429, json: { error: 'too many requests' } }
			this.pinChecks.push(this.time)
			if (!this.store.cashier_pin_set) return missing
			return body.pin === '2468'
				? { json: { ok: true } }
				: { status: 401, json: { error: 'wrong PIN', retry_after: 0 } }
		}
		if (path.endsWith('/checkout/cafe/sales') && method === 'POST') {
			if (body.expected_currency && body.expected_currency !== this.store.currency)
				return { status: 409, json: { error: 'The store currency changed. Enter the amount again.' } }
			const id = 'sale-' + (this.sales.size + 1)
			const next = sale(id, {
				amount_sat: body.amount_sat ?? Math.round(Number(body.fiat_amount) * 1000),
				memo: body.memo ?? null,
				fiat_amount: body.fiat_amount ?? null,
				currency: body.fiat_amount ? this.store.currency : null,
			})
			this.sales.set(id, next)
			return { json: next }
		}
		const match = path.match(/^\/api\/v1\/sales\/([^/]+)(?:\/(cancel|onchain))?$/)
		if (!match || !this.sales.has(match[1])) return missing
		const row = this.sales.get(match[1])
		if (match[2] === 'onchain' && row.onchain_address) return { json: { ...row } }
		if (match[2] && row.state !== 'open') return { status: 409, json: { error: 'sale closed' } }
		if (match[2] === 'cancel') row.state = 'canceled'
		if (match[2] === 'onchain') {
			if (row.amount_sat < this.store.onchain_min_sat) return { status: 400, json: { error: 'Below minimum' } }
			row.onchain_address ??= 'bcrt1qfixture' + row.id.toLowerCase()
			row.onchain_request =
				'bitcoin:' +
				row.onchain_address +
				'?amount=' +
				(row.amount_sat / 1e8).toFixed(8).replace(/0+$/, '').replace(/\.$/, '')
		}
		if (query) await delay(80)
		return { json: { ...row } }
	}
}
