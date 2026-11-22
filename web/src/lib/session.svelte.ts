// The cashier link of this page, from its path ("/<nym>" or "/c/<secret>"):
// any device with the link takes payments, with no pairing and no token.
// The owner's session token, which the history and the CSV need, comes
// from the wallet in the fragment of "/owner#token=<token>" and stays in
// this tab.
import { PosClient, type Checkout, type Link } from './api'

const TOKEN_KEY = 'bullnym:owner-token'

function readToken(): string | null {
	try {
		const match = location.hash.match(/token=([0-9a-fA-F]+)/)
		if (match) {
			sessionStorage.setItem(TOKEN_KEY, match[1]!)
			history.replaceState(null, '', location.pathname)
			return match[1]!
		}
		return sessionStorage.getItem(TOKEN_KEY)
	} catch {
		return null
	}
}

/** The cashier link in "/<nym>" or "/c/<secret>", or null. */
export function linkFromPath(pathname: string): Link | null {
	const unlisted = pathname.match(/^\/c\/([0-9a-fA-F]{1,64})\/?$/)
	if (unlisted) return { kind: 'unlisted', secret: unlisted[1]! }
	const pub = pathname.match(/^\/([a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?)\/?$/)
	if (pub && pub[1] !== 'owner') return { kind: 'public', nym: pub[1]! }
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

const state = $state<{ token: string | null; checkout: Checkout | null; cashierName: string }>({
	token: readToken(),
	checkout: null,
	cashierName: readLocal(linkKey('cashier-name')) ?? '',
})

export const session = {
	get link(): Link | null {
		return link
	},
	/** The owner's session token, on "/owner". */
	get token(): string | null {
		return state.token
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
	signOut(): void {
		try {
			sessionStorage.removeItem(TOKEN_KEY)
		} catch {
			/* storage unavailable */
		}
		state.token = null
	},
}

/** A client for this page's link, with the owner token if there is one. A 401 drops the token. */
export function client(): PosClient {
	return new PosClient({ link, token: state.token, onUnauthorized: () => session.signOut() })
}

const LOCAL_SALES_MAX = 50

/**
 * The sales this device made with this link, newest first. It is a
 * convenience of the screen, never the record: the owner's history is.
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
