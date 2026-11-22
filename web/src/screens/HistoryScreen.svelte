<script lang="ts">
	// Ported from bullnym's HistoryScreen (nostr-pos's Transactions layout).
	// The store's history and CSV are the owner's only, through the owner API
	// from the wallet. This screen lists the sales made on this device with
	// this link, read back by their ids: a convenience, never the record.
	// The list shows only after the service accepted the owner's cashier PIN,
	// typed on the POS keypad, and only while this screen stays open. Without
	// a PIN it stays hidden.
	import type { Sale } from '$lib/api'
	import { applyPinInput, MIN_PIN_DIGITS, pinRefusal } from '$lib/pin'
	import { client, localSaleIds, session } from '$lib/session.svelte'
	import BullFooter from '$lib/components/BullFooter.svelte'
	import Button from '$lib/components/Button.svelte'
	import HeaderLinks from '$lib/components/HeaderLinks.svelte'
	import Keypad from '$lib/components/Keypad.svelte'
	import TransactionSheet from '$lib/components/TransactionSheet.svelte'

	const pinSet = $derived(session.checkout?.store.cashier_pin_set ?? false)

	let pin = $state('')
	let unlocked = $state(false)
	let checking = $state(false)
	let pinError = $state<string | null>(null)

	let rows = $state<Sale[]>([])
	let loading = $state(true)
	let errorMsg = $state<string | null>(null)

	async function load() {
		loading = true
		errorMsg = null
		const results = await Promise.allSettled(localSaleIds().map((id) => client().sale(id)))
		rows = results.flatMap((r) => (r.status === 'fulfilled' ? [r.value] : []))
		if (rows.length === 0 && results.some((r) => r.status === 'rejected')) errorMsg = 'Could not load sales'
		loading = false
	}

	async function unlock() {
		if (checking || pin.length < MIN_PIN_DIGITS) return
		checking = true
		pinError = null
		try {
			await client().checkPin(pin)
			unlocked = true
			void load()
		} catch (e) {
			pinError = pinRefusal(e)
		} finally {
			pin = ''
			checking = false
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
		{:else if !unlocked}
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
				<Keypad precision={null} onInput={(key) => (pin = applyPinInput(pin, key))} />
				{#if pinError}
					<p class="rounded-md bg-[#ffe0d9] px-4 py-3 text-sm font-semibold text-[#8c2d28]">{pinError}</p>
				{/if}
				<Button disabled={checking || pin.length < MIN_PIN_DIGITS} onclick={unlock}>
					{checking ? 'Checking' : 'Show sales'}
				</Button>
			</div>
		{:else}
			{#if errorMsg}
				<p class="mb-4 rounded-md bg-[#ffe0d9] px-4 py-3 text-sm font-semibold text-[#8c2d28]">{errorMsg}</p>
			{/if}

			{#if !loading}
				<TransactionSheet {rows} />
			{/if}
		{/if}

		<p class="mt-6 text-sm text-[#776b5a] dark:text-[#b9aa91]">
			Only the sales made on this device are listed. The owner sees and exports every sale from the wallet.
		</p>

		<BullFooter />
	</div>
</main>
