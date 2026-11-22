// The cashier link this page serves, the store behind it, and the cashier
// name typed on this device. There is no device token: the link is all a
// cashier needs. The name is never verified; it only labels the sales made
// here.
import { CashierClient, linkFromPath, type Link, type Store } from './api'

function storageKey(link: Link, what: string): string {
	return `bullnym:${link.kind === 'public' ? `nym:${link.nym}` : `link:${link.secret}`}:${what}`
}

function read(key: string): string | null {
	try {
		return localStorage.getItem(key)
	} catch {
		return null
	}
}

function write(key: string, value: string | null): void {
	try {
		if (value === null) localStorage.removeItem(key)
		else localStorage.setItem(key, value)
	} catch {
		/* storage unavailable: it lasts for this page only */
	}
}

const link = linkFromPath(location.pathname)

const state = $state<{ store: Store | null; cashierName: string }>({
	store: null,
	cashierName: link ? (read(storageKey(link, 'cashier-name')) ?? '') : '',
})

export const session = {
	get link(): Link | null {
		return link
	},
	get store(): Store | null {
		return state.store
	},
	set store(value: Store | null) {
		state.store = value
	},
	get cashierName(): string {
		return state.cashierName
	},
	set cashierName(value: string) {
		state.cashierName = value
		if (link) write(storageKey(link, 'cashier-name'), value.trim() || null)
	},
}

/** A client for this page's cashier link. */
export function client(): CashierClient {
	if (!link) throw new Error('no cashier link')
	return new CashierClient(link)
}

/**
 * The sales this device created on this link, newest first. It is a
 * convenience of this screen, never the record: the owner's history is.
 */
const LOCAL_HISTORY_MAX = 50

export function localSaleIds(): string[] {
	if (!link) return []
	try {
		const ids = JSON.parse(read(storageKey(link, 'sales')) ?? '[]')
		return Array.isArray(ids) ? ids.filter((x): x is string => typeof x === 'string') : []
	} catch {
		return []
	}
}

export function rememberLocalSale(id: string): void {
	if (!link) return
	const ids = [id, ...localSaleIds().filter((x) => x !== id)].slice(0, LOCAL_HISTORY_MAX)
	write(storageKey(link, 'sales'), JSON.stringify(ids))
}
