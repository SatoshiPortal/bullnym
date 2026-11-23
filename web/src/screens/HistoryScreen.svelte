<script lang="ts">
	// Ported from bullnym's HistoryScreen (nostr-pos's Transactions layout).
	// The store's history and CSV are the owner's only, through the owner API
	// from the wallet. This screen lists the sales made on this device with
	// this link, read back by their ids: a convenience, never the record.
	// The list shows only after the service accepted the owner's cashier PIN,
	// typed on the POS keypad, and only while this screen stays open. Without
	// a PIN it stays hidden.
	import { onDestroy } from 'svelte'
	import { ApiError, type Sale } from '$lib/api'
	import { applyPinInput, MIN_PIN_DIGITS, pinRefusal } from '$lib/pin'
	import { client, localSaleIds, session } from '$lib/session.svelte'
	import BullFooter from '$lib/components/BullFooter.svelte'
	import Button from '$lib/components/Button.svelte'
	import HeaderLinks from '$lib/components/HeaderLinks.svelte'
	import Keypad from '$lib/components/Keypad.svelte'
	import TransactionSheet from '$lib/components/TransactionSheet.svelte'

	const pinSet = $derived(session.checkout?.store.cashier_pin_set ?? false)

	let pin = $state('')
	type Gate = { kind: 'locked' | 'checking' | 'unlocked' } | { kind: 'refused'; message: string } | { kind: 'waiting'; message: string; until: number }
	let gate = $state<Gate>({ kind: 'locked' })
	type List = { kind: 'loading' } | { kind: 'loaded' | 'incomplete'; rows: Sale[] }
	let list = $state<List>({ kind: 'loading' })
	let generation = 0
	let active = true
	onDestroy(() => { active = false; generation++ })

	function lock() {
		generation++
		gate = { kind: 'locked' }
		pin = ''
		list = { kind: 'loading' }
	}
	$effect(() => { if (!pinSet) lock() })
	$effect(() => {
		if (gate.kind !== 'waiting') return
		const timer = setTimeout(lock, Math.max(0, gate.until - Date.now()))
		return () => clearTimeout(timer)
	})
	$effect(() => {
		if (gate.kind !== 'unlocked') return
		void load()
		const storage = (event: StorageEvent) => {
			if (!event.key || event.key.startsWith('bullnym:' + session.link?.nym + ':')) void load()
		}
		window.addEventListener('storage', storage)
		window.addEventListener('online', load)
		window.addEventListener('focus', load)
		window.addEventListener('pagehide', lock)
		return () => {
			window.removeEventListener('storage', storage)
			window.removeEventListener('online', load)
			window.removeEventListener('focus', load)
			window.removeEventListener('pagehide', lock)
		}
	})

	async function load() {
		const request = ++generation
		const ids = localSaleIds()
		const results = await Promise.allSettled(ids.map((id) => client().sale(id)))
		if (!active || request !== generation) return
		const rows = results.flatMap((r, i) => r.status === 'fulfilled' && r.value.id === ids[i] ? [r.value] : [])
		list = { kind: rows.length === ids.length ? 'loaded' : 'incomplete', rows }
	}

	async function unlock() {
		if (gate.kind === 'checking' || gate.kind === 'waiting' || pin.length < MIN_PIN_DIGITS) return
		const request = ++generation
		gate = { kind: 'checking' }
		try {
			await client().checkPin(pin)
			if (active && request === generation) gate = { kind: 'unlocked' }
		} catch (e) {
			if (!active || request !== generation) return
			if (e instanceof ApiError && e.status === 404 && session.checkout) {
				session.store = { kind: 'ready', checkout: { store: { ...session.checkout.store, cashier_pin_set: false } } }
			}
			gate = e instanceof ApiError && e.retryAfter > 0
				? { kind: 'waiting', message: pinRefusal(e), until: Date.now() + e.retryAfter * 1000 }
				: { kind: 'refused', message: pinRefusal(e) }
		} finally {
			if (request === generation) pin = ''
		}
	}

</script>

<main class="min-h-screen bg-[#f5f0e8] px-5 py-5 text-[#211f1a] dark:bg-[#161512] dark:text-[#fff6e8]">
	<div class="mx-auto max-w-3xl">
		<header class="mb-8 flex items-center justify-between gap-4">
			<a class="inline-flex min-h-12 items-center gap-2 rounded-md px-2 text-sm font-semibold" href="#/">← Back</a>
			<HeaderLinks history={false} />
		</header>

		<div class="mb-5">
			<h1 class="font-display text-4xl uppercase tracking-display leading-none">Recent sales</h1>
			<p class="mt-1 text-xs font-medium uppercase tracking-[0.12em] text-[#776b5a] dark:text-[#b9aa91]">
				{session.checkout?.store.name ?? ''}
			</p>
		</div>

		{#if !pinSet}
			<p class="rounded-lg border border-[#d7c8b4] bg-[#fffaf0] px-4 py-12 text-center text-sm text-[#776b5a] dark:border-[#3a342a] dark:bg-[#211f1a] dark:text-[#b9aa91]">
				The owner has not set a cashier PIN, so the sales of this device are not shown.
			</p>
		{:else if gate.kind !== 'unlocked'}
			<div class="mx-auto flex max-w-sm flex-col gap-4">
				<div class="text-right">
					<div class="font-display text-xs uppercase tracking-[0.18em] text-[#776b5a] dark:text-[#b9aa91]">Cashier PIN</div>
					<div
						class="mt-1 min-h-[1em] font-display tabular-nums tracking-[0.3em] text-[clamp(2.5rem,10vw,4.5rem)] leading-[0.95]"
						aria-label={`${pin.length} digits typed`}
					>
						{'•'.repeat(pin.length) || '—'}
					</div>
				</div>
				<Keypad disabled={gate.kind === 'checking' || gate.kind === 'waiting'} precision={null} onInput={(key) => (pin = applyPinInput(pin, key))} />
				{#if gate.kind === 'refused' || gate.kind === 'waiting'}
					<p class="rounded-md bg-[#ffe0d9] px-4 py-3 text-sm font-semibold text-[#8c2d28]">{gate.message}</p>
				{/if}
				<Button disabled={gate.kind === 'checking' || gate.kind === 'waiting' || pin.length < MIN_PIN_DIGITS} onclick={unlock}>
					{gate.kind === 'checking' ? 'Checking' : 'Show sales'}
				</Button>
			</div>
		{:else}
			{#if list.kind === 'loading'}
				<p role="status">Loading sales…</p>
			{:else}
				{#if list.kind === 'incomplete'}
					<p role="alert">Could not load all sales on this device.</p>
					<Button variant="secondary" onclick={load}>Try again</Button>
				{/if}
				{#if list.rows.length || list.kind === 'loaded'}<TransactionSheet rows={list.rows} />{/if}
			{/if}
		{/if}

		<p class="mt-6 text-sm text-[#776b5a] dark:text-[#b9aa91]">
			Only the sales made on this device are listed. The owner sees and exports every sale from the wallet.
		</p>

		<BullFooter />
	</div>
</main>
