// Typed clients for the bullnym HTTP API. The contract is docs/api.md.
//
// - CashierClient: the cashier link, which is also the public checkout. It
//   needs no token: the link names the store, and a sale's id lets the
//   holder follow and cancel it.
// - OwnerClient: the history, the CSV and the store settings, with the
//   owner's session token.

export type SaleState = 'open' | 'in-progress' | 'paid' | 'payment-issue' | 'expired' | 'canceled'
export type PaidRail = 'lightning' | 'ark' | 'onchain'
export type Progress = 'onchain' | 'ark'
export type Warning = 'not-guaranteed' | 'low-fee' | 'unconfirmed-parents' | 'replaced'
export type Notice = 'payment-replaced' | 'payment-dropped'

export interface Sale {
	id: string
	created_at: string
	expires_at: string
	state: SaleState
	amount_sat: number
	fiat_amount: string | null
	currency: string | null
	btc_price: string | null
	memo: string | null
	payment_request: string
	lightning_invoice: string | null
	onchain_address: string | null
	paid_at: string | null
	paid_rail: PaidRail | null
	/** Counted payments only: delivered invoices and confirmed outputs. */
	received_sat: number
	/** Unconfirmed on-chain payments: shown, never counted. */
	seen_sat: number
	progress: Progress | null
	warnings: Warning[]
	notice: Notice | null
	final: boolean
	final_at: string | null
	late_payment: boolean
	link: 'public' | 'unlisted'
	cashier_name: string | null
}

export interface Store {
	name: string
	nym: string
	lightning_address: string
	currency: string
	onchain_min_sat: number
}

export interface OwnerStore extends Store {
	public_checkout: boolean
	checkout_url: string
	unlisted_link: boolean
}

export interface Rate {
	currency: string
	btc_price: string
	fetched_at: string
}

/** A cashier link: the store's public link under its nym, or its unlisted link. */
export type Link = { kind: 'public'; nym: string } | { kind: 'unlisted'; secret: string }

export type NewSale = ({ fiat_amount: string } | { amount_sat: number }) & { memo?: string; cashier_name?: string }

export class ApiError extends Error {
	constructor(
		readonly status: number,
		message: string,
	) {
		super(message)
		this.name = 'ApiError'
	}

	/** The token is missing, not valid, or expired. */
	get isUnauthorized(): boolean {
		return this.status === 401
	}

	/** The link does not work: the store does not exist, or the link was rotated. */
	get isNotFound(): boolean {
		return this.status === 404
	}
}

export type Fetch = typeof fetch

export interface ClientOptions {
	/** Prefix for every path; '' when the service serves this app. */
	baseUrl?: string
	fetch?: Fetch
}

/** Long-poll requests wait up to 25 s on the server. */
const WAIT_TIMEOUT_MS = 35_000
const DEFAULT_TIMEOUT_MS = 15_000

type RequestOpts = { token?: string | null; timeoutMs?: number; signal?: AbortSignal }

class Http {
	private readonly baseUrl: string
	private readonly fetchFn: Fetch

	constructor(opts: ClientOptions = {}) {
		this.baseUrl = opts.baseUrl ?? ''
		this.fetchFn = opts.fetch ?? ((input, init) => fetch(input, init))
	}

	async json<T>(method: string, path: string, body?: unknown, opts: RequestOpts = {}): Promise<T> {
		const res = await this.request(method, path, body, opts)
		return (await res.json()) as T
	}

	async request(method: string, path: string, body?: unknown, opts: RequestOpts = {}): Promise<Response> {
		const headers: Record<string, string> = { Accept: 'application/json' }
		if (body !== undefined) headers['Content-Type'] = 'application/json'
		if (opts.token) headers.Authorization = `Bearer ${opts.token}`

		const timeout = new AbortController()
		const timer = setTimeout(() => timeout.abort(), opts.timeoutMs ?? DEFAULT_TIMEOUT_MS)
		const onAbort = () => timeout.abort()
		opts.signal?.addEventListener('abort', onAbort)

		let res: Response
		try {
			res = await this.fetchFn(`${this.baseUrl}${path}`, {
				method,
				headers,
				body: body === undefined ? undefined : JSON.stringify(body),
				signal: timeout.signal,
			})
		} catch (e) {
			if (opts.signal?.aborted) throw e
			throw new ApiError(0, 'The server is not reachable')
		} finally {
			clearTimeout(timer)
			opts.signal?.removeEventListener('abort', onAbort)
		}

		if (!res.ok) throw new ApiError(res.status, await errorMessage(res))
		return res
	}
}

/** The API path of a link. */
export function linkPath(link: Link): string {
	return link.kind === 'public'
		? `/api/v1/checkout/${encodeURIComponent(link.nym)}`
		: `/api/v1/links/${encodeURIComponent(link.secret)}`
}

export class CashierClient {
	private readonly http: Http

	constructor(
		readonly link: Link,
		opts: ClientOptions = {},
	) {
		this.http = new Http(opts)
	}

	async store(): Promise<Store> {
		const res = await this.http.json<{ store: Store }>('GET', linkPath(this.link))
		return res.store
	}

	rate(): Promise<Rate> {
		return this.http.json('GET', `${linkPath(this.link)}/rate`)
	}

	createSale(sale: NewSale): Promise<Sale> {
		return this.http.json('POST', `${linkPath(this.link)}/sales`, sale)
	}

	sale(id: string): Promise<Sale> {
		return this.http.json('GET', `/api/v1/sales/${encodeURIComponent(id)}`)
	}

	/** Returns when the sale is no longer in `state`, or after the server's wait limit. */
	waitSale(id: string, state: SaleState, signal?: AbortSignal): Promise<Sale> {
		const path = `/api/v1/sales/${encodeURIComponent(id)}?wait=${encodeURIComponent(state)}`
		return this.http.json('GET', path, undefined, { timeoutMs: WAIT_TIMEOUT_MS, signal })
	}

	/** Cancels a sale that is still open. A sale in progress cannot be canceled. */
	cancelSale(id: string): Promise<Sale> {
		return this.http.json('POST', `/api/v1/sales/${encodeURIComponent(id)}/cancel`)
	}
}

export class OwnerClient {
	private readonly http: Http

	constructor(
		private readonly token: string,
		opts: ClientOptions = {},
	) {
		this.http = new Http(opts)
	}

	store(): Promise<OwnerStore> {
		return this.http.json('GET', '/api/v1/owner/store', undefined, { token: this.token })
	}

	updateStore(change: Partial<Pick<OwnerStore, 'name' | 'currency' | 'onchain_min_sat' | 'public_checkout'>>): Promise<OwnerStore> {
		return this.http.json('PATCH', '/api/v1/owner/store', change, { token: this.token })
	}

	/** Makes a new unlisted link. The previous one stops working. */
	rotateUnlistedLink(): Promise<{ url: string; secret: string }> {
		return this.http.json('POST', '/api/v1/owner/store/unlisted-link', undefined, { token: this.token })
	}

	async removeUnlistedLink(): Promise<void> {
		await this.http.request('DELETE', '/api/v1/owner/store/unlisted-link', undefined, { token: this.token })
	}

	async sales(opts: { before?: string; limit?: number } = {}): Promise<Sale[]> {
		const q = new URLSearchParams()
		if (opts.before) q.set('before', opts.before)
		if (opts.limit) q.set('limit', String(opts.limit))
		const qs = q.toString()
		const res = await this.http.json<{ sales: Sale[] }>('GET', `/api/v1/owner/sales${qs ? `?${qs}` : ''}`, undefined, {
			token: this.token,
		})
		return res.sales
	}

	/**
	 * CSV export for the local dates `from` to `to` (YYYY-MM-DD), both
	 * included. The range is sent as times, so the store's own days count,
	 * not UTC days.
	 */
	async salesCsv(from: string, to: string): Promise<Blob> {
		const q = new URLSearchParams(localDayRange(from, to))
		const res = await this.http.request('GET', `/api/v1/owner/sales.csv?${q}`, undefined, { token: this.token })
		return res.blob()
	}
}

async function errorMessage(res: Response): Promise<string> {
	try {
		const body = (await res.json()) as { error?: unknown }
		if (typeof body.error === 'string' && body.error) return body.error
	} catch {
		/* not JSON */
	}
	return `Request failed (HTTP ${res.status})`
}

/** The start of the local day `from` and the start of the day after `to`. */
export function localDayRange(from: string, to: string): { from: string; to: string } {
	const start = new Date(`${from}T00:00:00`)
	const end = new Date(`${to}T00:00:00`)
	end.setDate(end.getDate() + 1)
	return { from: start.toISOString(), to: end.toISOString() }
}

/** The cashier link in a page path: "/<nym>" or "/c/<secret>". */
export function linkFromPath(pathname: string): Link | null {
	const unlisted = pathname.match(/^\/c\/([0-9a-fA-F]{1,64})\/?$/)
	if (unlisted) return { kind: 'unlisted', secret: unlisted[1]! }
	const pub = pathname.match(/^\/([a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?)\/?$/)
	if (pub && pub[1] !== 'owner') return { kind: 'public', nym: pub[1]! }
	return null
}
