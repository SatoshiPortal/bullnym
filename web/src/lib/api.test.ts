import { describe, expect, it } from 'vitest'
import { ApiError, CashierClient, OwnerClient, linkFromPath, localDayRange, type Fetch } from './api'

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

const PUBLIC = { kind: 'public', nym: 'cafe' } as const
const UNLISTED = { kind: 'unlisted', secret: 'ab12' } as const

describe('CashierClient', () => {
	it('loads the store of a public or an unlisted link, without a token', async () => {
		const { fetch, calls } = fakeFetch([json({ store: { name: 'S' } }), json({ store: { name: 'S' } })])
		expect((await new CashierClient(PUBLIC, { fetch }).store()).name).toBe('S')
		await new CashierClient(UNLISTED, { fetch }).store()
		expect(calls[0]!.url).toBe('/api/v1/checkout/cafe')
		expect(calls[1]!.url).toBe('/api/v1/links/ab12')
		expect(header(calls[0]!, 'Authorization')).toBeUndefined()
	})

	it('creates a fiat sale with the contract body', async () => {
		const { fetch, calls } = fakeFetch([json({ id: 's1' })])
		await new CashierClient(PUBLIC, { fetch }).createSale({ fiat_amount: '12.50', memo: 'coffee', cashier_name: 'Ana' })
		expect(calls[0]!.url).toBe('/api/v1/checkout/cafe/sales')
		expect(calls[0]!.init.method).toBe('POST')
		expect(JSON.parse(String(calls[0]!.init.body))).toEqual({ fiat_amount: '12.50', memo: 'coffee', cashier_name: 'Ana' })
	})

	it('reads the rate of the link', async () => {
		const { fetch, calls } = fakeFetch([json({ currency: 'CRC', btc_price: '1', fetched_at: 'x' })])
		await new CashierClient(UNLISTED, { fetch }).rate()
		expect(calls[0]!.url).toBe('/api/v1/links/ab12/rate')
	})

	it('follows and cancels a sale by its id', async () => {
		const { fetch, calls } = fakeFetch([json({ id: 's1', state: 'paid' }), json({ id: 's1', state: 'canceled' })])
		const client = new CashierClient(PUBLIC, { fetch })
		const sale = await client.waitSale('s1', 'open')
		expect(sale.state).toBe('paid')
		expect(calls[0]!.url).toBe('/api/v1/sales/s1?wait=open')
		await client.cancelSale('s1')
		expect(calls[1]!.url).toBe('/api/v1/sales/s1/cancel')
		expect(calls[1]!.init.method).toBe('POST')
	})

	it('raises the server error message', async () => {
		const { fetch } = fakeFetch([json({ error: 'the sale is in-progress' }, 409)])
		const err = await new CashierClient(PUBLIC, { fetch }).cancelSale('s1').catch((e) => e)
		expect(err).toBeInstanceOf(ApiError)
		expect(err.status).toBe(409)
		expect(err.message).toBe('the sale is in-progress')
	})

	it('reports a dead link', async () => {
		const { fetch } = fakeFetch([json({ error: 'this cashier link does not work any more' }, 404)])
		const err = await new CashierClient(UNLISTED, { fetch }).store().catch((e) => e)
		expect(err.isNotFound).toBe(true)
	})

	it('maps a network failure to status 0', async () => {
		const fetch = (async () => {
			throw new TypeError('Failed to fetch')
		}) as Fetch
		const err = await new CashierClient(PUBLIC, { fetch }).rate().catch((e) => e)
		expect(err).toBeInstanceOf(ApiError)
		expect(err.status).toBe(0)
	})
})

describe('OwnerClient', () => {
	it('sends the owner token for the history', async () => {
		const { fetch, calls } = fakeFetch([json({ sales: [] }), json({ sales: [{ id: 'a' }] })])
		const owner = new OwnerClient('tok', { fetch })
		await owner.sales()
		const page = await owner.sales({ before: 'x', limit: 50 })
		expect(calls[0]!.url).toBe('/api/v1/owner/sales')
		expect(calls[1]!.url).toBe('/api/v1/owner/sales?before=x&limit=50')
		expect(header(calls[0]!, 'Authorization')).toBe('Bearer tok')
		expect(page).toEqual([{ id: 'a' }])
	})

	it('requests the CSV export for a date range', async () => {
		const { fetch, calls } = fakeFetch([new Response('id\n', { status: 200 })])
		const blob = await new OwnerClient('tok', { fetch }).salesCsv('2026-11-01', '2026-11-20')
		const range = localDayRange('2026-11-01', '2026-11-20')
		expect(calls[0]!.url).toBe(`/api/v1/owner/sales.csv?${new URLSearchParams(range)}`)
		expect(await blob.text()).toBe('id\n')
	})

	it('rotates the unlisted link', async () => {
		const { fetch, calls } = fakeFetch([json({ url: 'https://x/c/ff', secret: 'ff' })])
		const res = await new OwnerClient('tok', { fetch }).rotateUnlistedLink()
		expect(res.url).toBe('https://x/c/ff')
		expect(calls[0]!.url).toBe('/api/v1/owner/store/unlisted-link')
		expect(calls[0]!.init.method).toBe('POST')
	})

	it('reports an expired login', async () => {
		const { fetch } = fakeFetch([json({ error: 'expired' }, 401)])
		const err = await new OwnerClient('tok', { fetch }).store().catch((e) => e)
		expect(err.isUnauthorized).toBe(true)
	})
})

describe('linkFromPath', () => {
	it('reads public and unlisted links', () => {
		expect(linkFromPath('/cafe')).toEqual({ kind: 'public', nym: 'cafe' })
		expect(linkFromPath('/my-cafe/')).toEqual({ kind: 'public', nym: 'my-cafe' })
		expect(linkFromPath('/c/0aF9')).toEqual({ kind: 'unlisted', secret: '0aF9' })
		expect(linkFromPath('/')).toBeNull()
		expect(linkFromPath('/owner')).toBeNull()
		expect(linkFromPath('/Cafe')).toBeNull()
		expect(linkFromPath('/-x')).toBeNull()
		expect(linkFromPath('/c/xyz')).toBeNull()
	})
})

describe('localDayRange', () => {
	it('covers whole local days, end included', () => {
		const { from, to } = localDayRange('2026-11-20', '2026-11-20')
		expect(new Date(to).getTime() - new Date(from).getTime()).toBe(24 * 3600 * 1000)
		expect(new Date(from).getDate()).toBe(20)
	})
})
