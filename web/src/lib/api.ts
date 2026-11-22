// Typed client for the bullnym HTTP API. The contract is docs/api.md.
//
// A cashier link names the store: no device token. The sales history and
// CSV need the owner's session token.

export type SaleState = 'open' | 'in-progress' | 'paid' | 'payment-issue' | 'expired' | 'canceled'
export type PaidRail = 'lightning' | 'ark' | 'onchain'
export type Warning = 'not-guaranteed' | 'low-fee' | 'unconfirmed-parents' | 'replaced'

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
	/** Why the sale is in progress: an on-chain payment, or an Ark payment not delivered yet. */
	progress: 'onchain' | 'ark' | null
	warnings: Warning[]
	notice: 'payment-replaced' | 'payment-dropped' | null
	final: boolean
	late_payment: boolean
	link: 'public' | 'unlisted'
	cashier_name: string | null
}

export interface Store {
	name: string
	nym: string
	currency: string
	onchain_min_sat: number
	lightning_address: string
}

/** The store behind a cashier link. */
export interface Checkout {
	store: Store
	link: 'public' | 'unlisted'
}

/** A cashier link: the store's public link under its nym, or its unlisted link. */
export type Link = { kind: 'public'; nym: string } | { kind: 'unlisted'; secret: string }

export interface Rate {
	currency: string
	btc_price: string
	fetched_at: string
}

export type NewSale = ({ fiat_amount: string } | { amount_sat: number }) & { memo?: string; cashier_name?: string }

export class ApiError extends Error {
	constructor(
		readonly status: number,
		message: string,
	) {
		super(message)
		this.name = 'ApiError'
	}

	/** The owner token is missing, not valid, or expired. */
	get isUnauthorized(): boolean {
		return this.status === 401
	}
}

export type Fetch = typeof fetch

export interface ClientOptions {
	/** Prefix for every path; '' when the service serves this app. */
	baseUrl?: string
	/** The cashier link of the store. */
	link?: Link | null
	/** The owner's session token, for the history and the CSV. */
	token?: string | null
	fetch?: Fetch
	/** Called on every 401, before the error is thrown. */
	onUnauthorized?: () => void
}

/** Long-poll requests wait up to 25 s on the server. */
const WAIT_TIMEOUT_MS = 35_000
const DEFAULT_TIMEOUT_MS = 15_000

/** The API path of a cashier link. */
export function linkPath(link: Link): string {
	return link.kind === 'public'
		? `/api/v1/checkout/${encodeURIComponent(link.nym)}`
		: `/api/v1/links/${encodeURIComponent(link.secret)}`
}

export class PosClient {
	private readonly baseUrl: string
	private readonly link: Link | null
	private readonly token: string | null
	private readonly fetchFn: Fetch
	private readonly onUnauthorized?: () => void

	constructor(opts: ClientOptions = {}) {
		this.baseUrl = opts.baseUrl ?? ''
		this.link = opts.link ?? null
		this.token = opts.token ?? null
		this.fetchFn = opts.fetch ?? ((input, init) => fetch(input, init))
		this.onUnauthorized = opts.onUnauthorized
	}

	private linkPath(): string {
		if (!this.link) throw new Error('no cashier link')
		return linkPath(this.link)
	}

	checkout(): Promise<Checkout> {
		return this.json('GET', this.linkPath())
	}

	/** The owner's store, with the owner token. */
	ownerStore(): Promise<Store> {
		return this.json('GET', '/api/v1/owner/store')
	}

	rate(): Promise<Rate> {
		return this.json('GET', `${this.linkPath()}/rate`)
	}

	createSale(sale: NewSale): Promise<Sale> {
		return this.json('POST', `${this.linkPath()}/sales`, sale)
	}

	sale(id: string): Promise<Sale> {
		return this.json('GET', `/api/v1/sales/${encodeURIComponent(id)}`)
	}

	/** Returns when the sale is no longer in `state`, or after the server's wait limit. */
	waitSale(id: string, state: SaleState, signal?: AbortSignal): Promise<Sale> {
		const path = `/api/v1/sales/${encodeURIComponent(id)}?wait=${encodeURIComponent(state)}`
		return this.json('GET', path, undefined, { timeoutMs: WAIT_TIMEOUT_MS, signal })
	}

	cancelSale(id: string): Promise<Sale> {
		return this.json('POST', `/api/v1/sales/${encodeURIComponent(id)}/cancel`)
	}

	async sales(opts: { before?: string; limit?: number } = {}): Promise<Sale[]> {
		const q = new URLSearchParams()
		if (opts.before) q.set('before', opts.before)
		if (opts.limit) q.set('limit', String(opts.limit))
		const qs = q.toString()
		const res = await this.json<{ sales: Sale[] }>('GET', `/api/v1/owner/sales${qs ? `?${qs}` : ''}`)
		return res.sales
	}

	/**
	 * CSV export for the local dates `from` to `to` (YYYY-MM-DD), both
	 * included. The range is sent as times, so the store's own days count,
	 * not UTC days.
	 */
	async salesCsv(from: string, to: string): Promise<Blob> {
		const q = new URLSearchParams(localDayRange(from, to))
		const res = await this.request('GET', `/api/v1/owner/sales.csv?${q}`)
		return res.blob()
	}

	private async json<T>(
		method: string,
		path: string,
		body?: unknown,
		opts: { auth?: boolean; timeoutMs?: number; signal?: AbortSignal } = {},
	): Promise<T> {
		const res = await this.request(method, path, body, opts)
		return (await res.json()) as T
	}

	private async request(
		method: string,
		path: string,
		body?: unknown,
		opts: { auth?: boolean; timeoutMs?: number; signal?: AbortSignal } = {},
	): Promise<Response> {
		const headers: Record<string, string> = { Accept: 'application/json' }
		if (body !== undefined) headers['Content-Type'] = 'application/json'
		if (opts.auth !== false && this.token) headers.Authorization = `Bearer ${this.token}`

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

		if (!res.ok) {
			if (res.status === 401) this.onUnauthorized?.()
			throw new ApiError(res.status, await errorMessage(res))
		}
		return res
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
