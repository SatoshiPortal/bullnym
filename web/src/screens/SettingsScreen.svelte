<script lang="ts">
	// After bullnym's SettingsScreen (nostr-pos's Settings look). The owner
	// manages the store from the wallet, so this screen shows the setup and
	// the name of the cashier at this device, which labels the sales made
	// here and is never verified.
	import { formatSats } from '$lib/money'
	import { session } from '$lib/session.svelte'
	import BullFooter from '$lib/components/BullFooter.svelte'

	const store = $derived(session.store)
	let name = $state(session.cashierName)

	function save() {
		session.cashierName = name.trim().slice(0, 64)
	}
</script>

<main class="min-h-screen bg-[#f5f0e8] px-5 py-5 text-[#211f1a] dark:bg-[#161512] dark:text-[#fff6e8]">
	<div class="mx-auto max-w-xl">
		<header class="mb-8 flex items-center justify-between gap-4">
			<a class="inline-flex min-h-12 items-center gap-2 rounded-md px-2 text-sm font-semibold" href="#/">← Back</a>
		</header>

		<h1 class="mb-5 font-display text-4xl uppercase tracking-display leading-none">Settings</h1>

		<label class="mb-6 flex flex-col gap-1.5">
			<span class="text-xs font-medium uppercase tracking-[0.12em] text-[#776b5a] dark:text-[#b9aa91]">Cashier name</span>
			<input
				class="min-h-12 rounded-lg border border-[#d7c8b4] bg-[#fffaf0] px-4 text-base outline-none focus:ring-2 focus:ring-[#B7000B] dark:border-[#3a342a] dark:bg-[#211f1a]"
				bind:value={name}
				onchange={save}
				maxlength="64"
				placeholder="Front till"
			/>
			<span class="text-xs text-[#776b5a] dark:text-[#b9aa91]">Shown on the sales made here. Leave it empty on a payer's own phone.</span>
		</label>

		<dl class="space-y-3 rounded-lg border border-[#d7c8b4] bg-[#fffaf0] p-5 text-sm dark:border-[#3a342a] dark:bg-[#211f1a]">
			<div class="flex justify-between gap-4">
				<dt class="text-[#776b5a] dark:text-[#b9aa91]">Store</dt>
				<dd class="text-right font-semibold">{store?.name ?? '—'}</dd>
			</div>
			<div class="flex justify-between gap-4">
				<dt class="text-[#776b5a] dark:text-[#b9aa91]">Lightning address</dt>
				<dd class="break-all text-right font-semibold">{store?.lightning_address ?? '—'}</dd>
			</div>
			<div class="flex justify-between gap-4">
				<dt class="text-[#776b5a] dark:text-[#b9aa91]">Currency</dt>
				<dd class="text-right font-semibold">{store?.currency ?? '—'}</dd>
			</div>
			<div class="flex justify-between gap-4">
				<dt class="text-[#776b5a] dark:text-[#b9aa91]">On-chain from</dt>
				<dd class="text-right font-semibold">{store ? formatSats(store.onchain_min_sat) : '—'}</dd>
			</div>
		</dl>
		<p class="mt-3 text-xs text-[#776b5a] dark:text-[#b9aa91]">
			The owner changes these settings in the wallet. Payments go to the owner's wallet; this device cannot send funds.
		</p>

		<BullFooter />
	</div>
</main>
