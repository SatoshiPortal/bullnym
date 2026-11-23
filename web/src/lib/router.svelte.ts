// Minimal hash router, ported from bullnym. A dependency-free $state wrapper
// is enough for a handful of static route shapes.

function parseHash(): string {
	const h = location.hash
	return h.startsWith('#/') ? h.slice(1) : '/'
}

const state = $state({ path: parseHash() })

window.addEventListener('hashchange', () => { state.path = parseHash() })

function plainClick(event: MouseEvent): boolean {
	return !event.defaultPrevented && event.button === 0 && !event.metaKey && !event.ctrlKey && !event.shiftKey && !event.altKey
}

export const router = {
	get path(): string {
		return state.path
	},
	go(path: string): void {
		if (path === '/settings' || path === '/history' || path.startsWith('/receipt/')) {
			history.pushState({ cashierParent: state.path }, '', '#' + path)
			state.path = path
		} else {
			location.hash = '#' + path
		}
	},
	get parent(): string {
		return typeof history.state?.cashierParent === 'string' ? history.state.cashierParent : '/'
	},
	link(event: MouseEvent): void {
		if (!plainClick(event) || !(event.currentTarget instanceof HTMLAnchorElement)) return
		event.preventDefault()
		router.go(event.currentTarget.hash.slice(1))
	},
	back(event?: MouseEvent): void {
		if (event && !plainClick(event)) return
		event?.preventDefault()
		if (history.state?.cashierParent || history.state?.receiptParent) history.back()
		else router.go('/')
	},
	/** Match "/pay/:id" style patterns; returns params or null. */
	match(pattern: string): Record<string, string> | null {
		const p = pattern.split('/')
		const a = state.path.split('/')
		if (p.length !== a.length) return null
		const params: Record<string, string> = {}
		for (let i = 0; i < p.length; i++) {
			const seg = p[i]!
			if (seg.startsWith(':')) {
				try {
					params[seg.slice(1)] = decodeURIComponent(a[i]!)
				} catch {
					return null
				}
			}
			else if (seg !== a[i]) return null
		}
		return params
	},
}
