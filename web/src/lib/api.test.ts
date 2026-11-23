import { describe, expect, it } from 'vitest'
import { ApiError, PosClient, type Fetch } from './api'

type Call = { url: string; init: RequestInit }

function fakeFetch(responses: Response[]): { fetch: Fetch; calls: Call[] } {
	const calls: Call[] = []
	const fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
		calls.push({ url: String(input), init: init ?? {} })
		const next = responses.shift()
		if (!next) throw new Error('unexpected request')
		return next
	}) as Fetch
	return { fetch, calls }
}

function json(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } })
}

function header(call: Call, name: string): string | undefined {
	return (call.init.headers as Record<string, string> | undefined)?.[name]
}

const PUBLIC = { nym: 'cafe' }

describe('PosClient', () => {
	it('loads the store of its link without a token', async () => {
		const { fetch, calls } = fakeFetch([json({ store: { name: 'S' } })])
		const res = await new PosClient({ fetch, link: PUBLIC }).checkout()
		expect(res.store.name).toBe('S')
		expect(calls[0]!.url).toBe('/api/v1/checkout/cafe')
		expect(header(calls[0]!, 'Authorization')).toBeUndefined()
	})

	it('reads the rate of the link', async () => {
		const { fetch, calls } = fakeFetch([json({ currency: 'CRC', btc_price: '1', fetched_at: 'x' })])
		await new PosClient({ fetch, link: PUBLIC }).rate()
		expect(calls[0]!.url).toBe('/api/v1/checkout/cafe/rate')
	})

	it('creates a fiat sale with the contract body', async () => {
		const { fetch, calls } = fakeFetch([json({ id: 's1' })])
		await new PosClient({ fetch, link: PUBLIC }).createSale({ fiat_amount: '12.50', expected_currency: 'USD', memo: 'coffee' })
		expect(calls[0]!.url).toBe('/api/v1/checkout/cafe/sales')
		expect(JSON.parse(String(calls[0]!.init.body))).toEqual({ fiat_amount: '12.50', expected_currency: 'USD', memo: 'coffee' })
	})

	it('long-polls with the known state', async () => {
		const { fetch, calls } = fakeFetch([json({ id: 's1', state: 'paid' })])
		const sale = await new PosClient({ fetch, link: PUBLIC }).waitSale('s1', 'open')
		expect(sale.state).toBe('paid')
		expect(calls[0]!.url).toBe('/api/v1/sales/s1?wait=open')
	})

	it('adds Bitcoin to a sale', async () => {
		const { fetch, calls } = fakeFetch([json({ id: 's1', onchain_request: 'bitcoin:bc1q?amount=0.0006' })])
		const sale = await new PosClient({ fetch, link: PUBLIC }).addOnchain('s1')
		expect(sale.onchain_request).toBe('bitcoin:bc1q?amount=0.0006')
		expect(calls[0]!.url).toBe('/api/v1/sales/s1/onchain')
		expect(calls[0]!.init.method).toBe('POST')
	})

	it('cancels a sale', async () => {
		const { fetch, calls } = fakeFetch([json({ id: 's1', state: 'canceled' })])
		await new PosClient({ fetch, link: PUBLIC }).cancelSale('s1')
		expect(calls[0]!.url).toBe('/api/v1/sales/s1/cancel')
		expect(calls[0]!.init.method).toBe('POST')
		// No device or owner token: the sale id is all a cashier needs.
		expect(header(calls[0]!, 'Authorization')).toBeUndefined()
	})

	it('raises the server error message', async () => {
		const { fetch } = fakeFetch([json({ error: 'sale is not open' }, 409)])
		const err = await new PosClient({ fetch, link: PUBLIC }).cancelSale('s1').catch((e) => e)
		expect(err).toBeInstanceOf(ApiError)
		expect(err.status).toBe(409)
		expect(err.message).toBe('sale is not open')
	})

	it('checks the cashier PIN with the service', async () => {
		const { fetch, calls } = fakeFetch([json({ ok: true })])
		await new PosClient({ fetch, link: PUBLIC }).checkPin('2468')
		expect(calls[0]!.url).toBe('/api/v1/checkout/cafe/pin')
		expect(calls[0]!.init.method).toBe('POST')
		expect(JSON.parse(String(calls[0]!.init.body))).toEqual({ pin: '2468' })
	})

	it('requires affirmative service confirmation before unlocking', async () => {
		const { fetch } = fakeFetch([json({ ok: false }), new Response('store loading')])
		const client = new PosClient({ fetch, link: PUBLIC })
		await expect(client.checkPin('2468')).rejects.toHaveProperty('status', 502)
		await expect(client.checkPin('2468')).rejects.toHaveProperty('status', 502)
	})

	it('reads how long a refused PIN locks checks', async () => {
		const { fetch } = fakeFetch([json({ error: 'wrong PIN', retry_after: 4 }, 401)])
		const err = await new PosClient({ fetch, link: PUBLIC }).checkPin('0000').catch((e) => e)
		expect(err).toBeInstanceOf(ApiError)
		expect([err.status, err.message, err.retryAfter]).toEqual([401, 'wrong PIN', 4])
	})

	it('maps a network failure to status 0', async () => {
		const fetch = (async () => {
			throw new TypeError('Failed to fetch')
		}) as Fetch
		const err = await new PosClient({ fetch, link: PUBLIC }).rate().catch((e) => e)
		expect(err).toBeInstanceOf(ApiError)
		expect(err.status).toBe(0)
	})
})
