// The cashier link of this page, from its path "/<nym>": any device with the
// link takes payments, with no pairing and no token.
import { PosClient, type Checkout, type Link } from './api'

/** The cashier link in "/<nym>", or null. */
export function linkFromPath(pathname: string): Link | null {
	const match = pathname.match(/^\/([a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?)\/?$/)
	return match ? { nym: match[1]! } : null
}

const link = linkFromPath(location.pathname)

function linkKey(what: string): string | null {
	if (!link) return null
	return `bullnym:${link.nym}:${what}`
}

type StoreLoad = { kind: 'loading' } | { kind: 'ready'; checkout: Checkout } | { kind: 'error'; message: string }
let store = $state<StoreLoad>({ kind: 'loading' })

export const session = {
	get link() { return link },
	get store() { return store },
	set store(value: StoreLoad) { store = value },
	get checkout() { return store.kind === 'ready' ? store.checkout : null },
}

/** A client for this page's link. */
export function client(): PosClient {
	return new PosClient({ link })
}

const LOCAL_SALES_MAX = 50

/**
 * The sales this device made with this link, newest first. It is a
 * convenience of the screen, never the record: the owner's history, through
 * the owner API, is.
 */
const prefix = linkKey('sale:') ?? ''
const fallback = new Map<string, number>()

function storedSales(): string[] {
	const entries = new Map(fallback)
	try {
		// Keep the previous list and its order for installations updated
		// in place. New sales each have a key, so two tabs can append safely.
		let legacy: unknown = []
		try { legacy = JSON.parse(localStorage.getItem(linkKey('sales')!) ?? '[]') } catch { /* corrupt old list */ }
		if (Array.isArray(legacy)) legacy.forEach((id, i) => {
			if (typeof id === 'string' && !entries.has(id)) entries.set(id, -i)
		})
		for (let i = 0; i < localStorage.length; i++) {
			const key = localStorage.key(i)
			if (key?.startsWith(prefix)) entries.set(key.slice(prefix.length), Number(localStorage.getItem(key)))
		}
	} catch { /* The current page still remembers its own sales. */ }
	return [...entries.keys()].sort((a, b) => entries.get(b)! - entries.get(a)! || a.localeCompare(b))
}

export function localSaleIds(): string[] {
	return storedSales().slice(0, LOCAL_SALES_MAX)
}

export function rememberLocalSale(id: string): void {
	const now = Date.now()
	fallback.set(id, now)
	try { localStorage.setItem(prefix + id, String(now)) } catch { /* memory only */ }
	for (const old of storedSales().slice(LOCAL_SALES_MAX)) {
		fallback.delete(old)
		try { localStorage.removeItem(prefix + old) } catch { /* memory only */ }
	}
}
