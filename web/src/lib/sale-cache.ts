// Sales this page created or loaded, so the pay and receipt screens can
// render at once and refresh from posd after.
import type { Sale } from './api'
import { acceptSale } from './sale'

const sales = new Map<string, Sale>()

/** Keeps the newest snapshot of a sale. Returns the one kept. */
export function rememberSale(sale: Sale): Sale {
	const current = sales.get(sale.id) ?? null
	if (!acceptSale(current, sale, sale.id)) {
		if (!current || current.updated_at !== sale.updated_at || current.onchain_request || !sale.onchain_request) return current!
		sale = { ...current, onchain_address: sale.onchain_address, onchain_request: sale.onchain_request }
	}
	sales.set(sale.id, sale)
	return sale
}

export function cachedSale(id: string): Sale | undefined {
	return sales.get(id)
}
