// The cashier link of this page, from its path ("/<nym>" or "/c/<secret>"):
// any device with the link takes payments, with no pairing and no token.
import { PosClient, type Checkout, type Link } from './api'

/** The cashier link in "/<nym>" or "/c/<secret>", or null. */
export function linkFromPath(pathname: string): Link | null {
	const unlisted = pathname.match(/^\/c\/([0-9a-fA-F]{1,64})\/?$/)
	if (unlisted) return { kind: 'unlisted', secret: unlisted[1]! }
	const pub = pathname.match(/^\/([a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?)\/?$/)
	if (pub) return { kind: 'public', nym: pub[1]! }
	return null
}

const link = linkFromPath(location.pathname)

function linkKey(what: string): string | null {
	if (!link) return null
	return `bullnym:${link.kind === 'public' ? link.nym : link.secret}:${what}`
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

const state = $state<{ checkout: Checkout | null; cashierName: string }>({
	checkout: null,
	cashierName: readLocal(linkKey('cashier-name')) ?? '',
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
	/** A name typed on this device. It labels the sales made here and is never verified. */
	get cashierName(): string {
		return state.cashierName
	},
	set cashierName(value: string) {
		state.cashierName = value
		writeLocal(linkKey('cashier-name'), value)
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
