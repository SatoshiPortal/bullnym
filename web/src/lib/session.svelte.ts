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

function readLocal(key: string | null): string | null {
	try {
		return key ? localStorage.getItem(key) : null
	} catch {
		return null
	}
}

function writeLocal(key: string | null, value: string): void {
	try {
		if (key) localStorage.setItem(key, value)
	} catch {
		/* storage unavailable: it lasts for this page only */
	}
}

const state = $state<{ checkout: Checkout | null }>({
	checkout: null,
})

export const session = {
	get link(): Link | null {
		return link
	},
	get checkout(): Checkout | null {
		return state.checkout
	},
	set checkout(value: Checkout | null) {
		state.checkout = value
	},
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
export function localSaleIds(): string[] {
	try {
		const ids = JSON.parse(readLocal(linkKey('sales')) ?? '[]')
		return Array.isArray(ids) ? ids.filter((x): x is string => typeof x === 'string') : []
	} catch {
		return []
	}
}

export function rememberLocalSale(id: string): void {
	writeLocal(linkKey('sales'), JSON.stringify([id, ...localSaleIds().filter((x) => x !== id)].slice(0, LOCAL_SALES_MAX)))
}
