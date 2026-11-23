// One reader per mounted pay or receipt route. Navigation, foregrounding the
// tab and successful mutations supersede the request already in flight.
import { untrack } from 'svelte'
import { ApiError, type Sale } from './api'
import { isLive } from './sale'
import { cachedSale, rememberSale } from './sale-cache'
import { client, session } from './session.svelte'

type View =
	| { kind: 'loading' | 'missing' }
	| { kind: 'ready'; sale: Sale }
	| { kind: 'retrying'; sale: Sale | null }

export function followSale(id: string) {
	let view = $state<View>({ kind: 'loading' })
	let controller: AbortController | undefined
	let timer: ReturnType<typeof setTimeout>
	let active = false
	const value = () => 'sale' in view ? view.sale : null

	function refresh() {
		if (!active) return
		controller?.abort()
		clearTimeout(timer)
		controller = new AbortController()
		void read(controller.signal)
	}

	async function read(signal: AbortSignal) {
		let previous: Sale | undefined
		try {
			do {
				const next = previous
					? await client().waitSale(id, previous.state, signal, previous.updated_at)
					: await client().sale(id, signal)
				if (signal.aborted) return
				if (next.id !== id) throw Error('Sale identifier mismatch')
				if (next.store_nym && next.store_nym !== session.link?.nym) {
					location.replace('/' + encodeURIComponent(next.store_nym) + location.hash)
					return
				}
				previous = rememberSale(next)
				view = { kind: 'ready', sale: previous }
				// A funded invoice may acquire its method after settlement.
				// An expired invoice can still receive a late payment.
			} while (isLive(previous.state) || previous.onchain_address ||
				(previous.state === 'paid' && !previous.paid_rail) ||
				(previous.state === 'expired' && !previous.late_payment && previous.lightning_invoice))
		} catch (e) {
			if (signal.aborted) return
			if (e instanceof ApiError && e.status === 404) {
				view = { kind: 'missing' }
			} else {
				view = { kind: 'retrying', sale: value() }
				timer = setTimeout(refresh, 2000)
			}
		}
	}

	$effect(() => {
		active = true
		const cached = cachedSale(id)
		if (cached && cached.store_nym === session.link?.nym) view = { kind: 'ready', sale: cached }
		untrack(refresh)
		const visible = () => { if (!document.hidden) refresh() }
		window.addEventListener('online', refresh)
		window.addEventListener('focus', refresh)
		document.addEventListener('visibilitychange', visible)
		return () => {
			active = false
			controller?.abort()
			clearTimeout(timer)
			window.removeEventListener('online', refresh)
			window.removeEventListener('focus', refresh)
			document.removeEventListener('visibilitychange', visible)
		}
	})

	return {
		get kind() { return view.kind },
		get sale() { return value() },
		refresh,
		accept(next: Sale) {
			if (!active || next.id !== id) return false
			const accepted = rememberSale(next)
			view = { kind: 'ready', sale: accepted }
			refresh()
			return accepted === next || (next.updated_at !== undefined && accepted.updated_at === next.updated_at)
		},
	}
}
