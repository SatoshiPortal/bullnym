// Typed client for the bullnym HTTP API. The contract is docs/api.md.
//
// A cashier link names the store: no device token. The store's history and
// CSV are on the owner API, which the owner's wallet calls.

export type SaleState = 'open' | 'in-progress' | 'paid' | 'expired' | 'canceled'
export type PaidRail = 'lightning' | 'ark' | 'onchain'
export type Warning = 'not-guaranteed' | 'low-fee' | 'unconfirmed-parents' | 'replaced'

export interface Sale {
	id: string
	store_nym: string
	updated_at: string
	created_at: string
	expires_at: string
	state: SaleState
	amount_sat: number
	fiat_amount: string | null
	currency: string | null
	btc_price: string | null
	memo: string | null
	/** The Lightning QR: `lightning:<invoice>`. */
	lightning_request: string | null
	/** The Bitcoin QR: `bitcoin:<address>?amount=<btc>`, once Bitcoin was added. */
	onchain_request: string | null
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
	late_payment: boolean
}

export interface Store {
	name: string
	nym: string
	currency: string
	/** Configured in US dollars, in decimal major units. Older services omit it. */
	onchain_min_usd?: string
	/** Read-only satoshi cutoff for the service’s USD quote. */
	onchain_min_sat: number | null
	lightning_address: string
	/** Whether the owner set a cashier PIN. Without one, the list of this device's sales stays hidden. */
	cashier_pin_set: boolean
}

/** The store behind a cashier link. */
export interface Checkout {
	store: Store
}

/** A store's cashier link, "/<nym>". */
export type Link = { nym: string }

export interface Rate {
	currency: string
	btc_price: string
	fetched_at: string
}

export type NewSale = ({ fiat_amount: string; expected_currency: string } | { amount_sat: number }) & { memo?: string }

export class ApiError extends Error {
	constructor(
		readonly status: number,
		message: string,
		/** Seconds until the service takes the request again, when it says so. */
		readonly retryAfter: number = 0,
	) {
		super(message)
		this.name = 'ApiError'
	}
}

export type Fetch = typeof fetch

export interface ClientOptions {
	/** Prefix for every path; '' when the service serves this app. */
	baseUrl?: string
	/** The cashier link of the store. */
	link?: Link | null
	fetch?: Fetch
}

/** Long-poll requests wait up to 25 s on the server. */
const WAIT_TIMEOUT_MS = 35_000
const DEFAULT_TIMEOUT_MS = 15_000

/** The API path of a cashier link. */
export function linkPath(link: Link): string {
	return `/api/v1/checkout/${encodeURIComponent(link.nym)}`
}

export class PosClient {
	private readonly baseUrl: string
	private readonly link: Link | null
	private readonly fetchFn: Fetch

	constructor(opts: ClientOptions = {}) {
		this.baseUrl = opts.baseUrl ?? ''
		this.link = opts.link ?? null
		this.fetchFn = opts.fetch ?? ((input, init) => fetch(input, init))
	}

	private linkPath(): string {
		if (!this.link) throw new Error('no cashier link')
		return linkPath(this.link)
	}

	checkout(): Promise<Checkout> {
		return this.json('GET', this.linkPath())
	}

	rate(): Promise<Rate> {
		return this.json('GET', `${this.linkPath()}/rate`)
	}

	createSale(sale: NewSale): Promise<Sale> {
		return this.json('POST', `${this.linkPath()}/sales`, sale)
	}

	sale(id: string, signal?: AbortSignal): Promise<Sale> {
		return this.json('GET', `/api/v1/sales/${encodeURIComponent(id)}`, undefined, { signal })
	}

	/** Returns when the sale is no longer in `state`, or after the server's wait limit. */
	waitSale(id: string, state: SaleState, signal?: AbortSignal, since?: string): Promise<Sale> {
		const query = new URLSearchParams({ wait: state })
		if (since) query.set('since', since)
		const path = `/api/v1/sales/${encodeURIComponent(id)}?${query}`
		return this.json('GET', path, undefined, { timeoutMs: WAIT_TIMEOUT_MS, signal })
	}

	/** Adds Bitcoin to an open sale. A second call returns the same address. */
	addOnchain(id: string): Promise<Sale> {
		return this.json('POST', `/api/v1/sales/${encodeURIComponent(id)}/onchain`)
	}

	cancelSale(id: string): Promise<Sale> {
		return this.json('POST', `/api/v1/sales/${encodeURIComponent(id)}/cancel`)
	}

	/**
	 * Asks the service whether `pin` is the store's cashier PIN. Resolves on
	 * a match; a wrong or locked PIN throws an ApiError (401 or 429) whose
	 * `retryAfter` says how long checks are locked.
	 */
	async checkPin(pin: string): Promise<void> {
		const result = await this.json<{ ok?: boolean }>('POST', `${this.linkPath()}/pin`, { pin })
		if (result?.ok !== true) throw new ApiError(502, 'Could not verify the PIN response')
	}

	private async json<T>(
		method: string,
		path: string,
		body?: unknown,
		opts: { timeoutMs?: number; signal?: AbortSignal } = {},
	): Promise<T> {
		const headers: Record<string, string> = { Accept: 'application/json' }
		if (body !== undefined) headers['Content-Type'] = 'application/json'

		const timeout = new AbortController()
		const timer = setTimeout(() => timeout.abort(), opts.timeoutMs ?? DEFAULT_TIMEOUT_MS)
		const onAbort = () => timeout.abort()
		opts.signal?.addEventListener('abort', onAbort)

		try {
			opts.signal?.throwIfAborted()
			const res = await this.fetchFn(`${this.baseUrl}${path}`, {
				method,
				// Firefox can replay an earlier checkout or sale response on offline reload.
				cache: 'no-store',
				headers,
				body: body === undefined ? undefined : JSON.stringify(body),
				signal: timeout.signal,
			})
			if (!res.ok) {
				const { message, retryAfter } = await errorDetails(res)
				throw new ApiError(res.status, message, retryAfter)
			}
			return await res.json() as T
		} catch (e) {
			if (opts.signal?.aborted || e instanceof ApiError) throw e
			if (e instanceof SyntaxError) throw new ApiError(502, 'Invalid response from the server')
			throw new ApiError(0, 'The server is not reachable')
		} finally {
			clearTimeout(timer)
			opts.signal?.removeEventListener('abort', onAbort)
		}
	}
}

async function errorDetails(res: Response): Promise<{ message: string; retryAfter: number }> {
	let message = `Request failed (HTTP ${res.status})`
	let retryAfter = 0
	try {
		const body = (await res.json()) as { error?: unknown; retry_after?: unknown }
		if (typeof body.error === 'string' && body.error) message = body.error
		if (typeof body.retry_after === 'number' && body.retry_after > 0) retryAfter = body.retry_after
	} catch {
		/* not JSON */
	}
	return { message, retryAfter }
}
