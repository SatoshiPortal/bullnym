<script lang="ts">
	// Ported from bullnym's HistoryScreen (nostr-pos's Transactions layout).
	// The store's history and CSV are the owner's only, through the owner API
	// from the wallet. This screen lists the sales made on this device with
	// this link, read back by their ids: a convenience, never the record.
	import type { Sale } from '$lib/api'
	import { client, localSaleIds, session } from '$lib/session.svelte'
	import BullFooter from '$lib/components/BullFooter.svelte'
	import HeaderLinks from '$lib/components/HeaderLinks.svelte'
	import TransactionSheet from '$lib/components/TransactionSheet.svelte'

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

	void load()
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

		{#if errorMsg}
			<p class="mb-4 rounded-md bg-[#ffe0d9] px-4 py-3 text-sm font-semibold text-[#8c2d28]">{errorMsg}</p>
		{/if}

		{#if !loading}
			<TransactionSheet {rows} />
		{/if}

		<p class="mt-6 text-sm text-[#776b5a] dark:text-[#b9aa91]">
			Only the sales made on this device are listed. The owner sees and exports every sale from the wallet.
		</p>

		<BullFooter />
	</div>
</main>
