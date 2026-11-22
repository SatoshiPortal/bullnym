<script lang="ts">
	// The owner's view: the store's full history with each sale's stage and
	// warnings, the CSV export and the cashier links. The owner's wallet logs
	// in (a signed challenge) and opens "/owner#token=<session token>"; the
	// fragment never reaches the server, and the token stays in this tab.
	import { Download, Link2, RotateCw } from 'lucide-svelte'
	import { ApiError, OwnerClient, type OwnerStore, type Sale } from '$lib/api'
	import { isoDate } from '$lib/sale'
	import BullFooter from '$lib/components/BullFooter.svelte'
	import Button from '$lib/components/Button.svelte'
	import CopyRow from '$lib/components/CopyRow.svelte'
	import TransactionSheet from '$lib/components/TransactionSheet.svelte'

	const TOKEN_KEY = 'bullnym:owner-token'
	const PAGE = 50

	function takeToken(): string | null {
		const match = location.hash.match(/token=([0-9a-fA-F]+)/)
		try {
			if (match) {
				sessionStorage.setItem(TOKEN_KEY, match[1]!)
				history.replaceState(null, '', location.pathname)
				return match[1]!
			}
			return sessionStorage.getItem(TOKEN_KEY)
		} catch {
			return match ? match[1]! : null
		}
	}

	const token = takeToken()
	const owner = token ? new OwnerClient(token) : null

	let store = $state<OwnerStore | null>(null)
	let rows = $state<Sale[]>([])
	let more = $state(false)
	let loading = $state(true)
	let errorMsg = $state<string | null>(null)
	let unlistedUrl = $state<string | null>(null)
	let confirmRotate = $state(false)

	const today = new Date()
	let from = $state(isoDate(new Date(today.getFullYear(), today.getMonth(), 1)))
	let to = $state(isoDate(today))
	let exporting = $state(false)

	function fail(e: unknown) {
		if (e instanceof ApiError && e.isUnauthorized) errorMsg = 'The login expired. Open the history again from the wallet.'
		else errorMsg = e instanceof Error ? e.message : 'Something went wrong'
	}

	async function load(before?: string) {
		if (!owner) return
		loading = true
		errorMsg = null
		try {
			if (!before) store = await owner.store()
			const page = await owner.sales({ before, limit: PAGE })
			rows = before ? [...rows, ...page] : page
			more = page.length === PAGE
		} catch (e) {
			fail(e)
		} finally {
			loading = false
		}
	}

	void load()

	async function togglePublic() {
		if (!owner || !store) return
		try {
			store = await owner.updateStore({ public_checkout: !store.public_checkout })
		} catch (e) {
			fail(e)
		}
	}

	async function rotate() {
		if (!owner) return
		if (store?.unlisted_link && !confirmRotate) {
			confirmRotate = true
			return
		}
		confirmRotate = false
		try {
			unlistedUrl = (await owner.rotateUnlistedLink()).url
			store = await owner.store()
		} catch (e) {
			fail(e)
		}
	}

	async function exportCsv() {
		if (!owner || exporting || !from || !to) return
		exporting = true
		errorMsg = null
		try {
			const blob = await owner.salesCsv(from, to)
			const url = URL.createObjectURL(blob)
			const a = document.createElement('a')
			a.href = url
			a.download = `sales-${from}-to-${to}.csv`
			document.body.appendChild(a)
			a.click()
			a.remove()
			setTimeout(() => URL.revokeObjectURL(url), 1000)
		} catch (e) {
			fail(e)
		} finally {
			exporting = false
		}
	}
</script>

<main class="min-h-screen bg-[#f5f0e8] px-5 py-5 text-[#211f1a] dark:bg-[#161512] dark:text-[#fff6e8]">
	<div class="mx-auto max-w-3xl">
		{#if !owner}
			<section class="grid min-h-[70vh] place-items-center text-center">
				<div>
					<h1 class="text-3xl font-black">Owner login needed</h1>
					<p class="mt-3 text-[#776b5a] dark:text-[#b9aa91]">Open the sales history from the wallet that registered the store.</p>
				</div>
			</section>
		{:else}
			<div class="mb-5">
				<h1 class="font-display text-4xl uppercase tracking-display leading-none">{store?.name ?? 'Sales'}</h1>
				<p class="mt-1 text-xs font-medium uppercase tracking-[0.12em] text-[#776b5a] dark:text-[#b9aa91]">
					{store?.lightning_address ?? ''}
				</p>
			</div>

			{#if errorMsg}
				<p class="mb-4 rounded-md bg-[#ffe0d9] px-4 py-3 text-sm font-semibold text-[#8c2d28]">{errorMsg}</p>
			{/if}

			{#if store}
				<section class="mb-8 rounded-lg border border-[#d7c8b4] bg-[#fffaf0] p-4 dark:border-[#3a342a] dark:bg-[#211f1a]">
					<h2 class="font-display text-2xl uppercase tracking-display leading-none">Cashier links</h2>
					<p class="mt-1 text-sm text-[#776b5a] dark:text-[#b9aa91]">
						Anyone with a link can take payments for the store. Nobody with a link can see other sales.
					</p>
					<div class="mt-3 flex flex-col gap-2">
						<CopyRow label={store.public_checkout ? 'Public checkout' : 'Public checkout (off)'} value={store.checkout_url} />
						<Button variant="secondary" onclick={togglePublic}>
							{store.public_checkout ? 'Turn the public checkout off' : 'Turn the public checkout on'}
						</Button>
						{#if unlistedUrl}
							<CopyRow label="Unlisted link (shown once)" value={unlistedUrl} />
						{/if}
						<Button variant="secondary" onclick={rotate}>
							{#if store.unlisted_link}
								<RotateCw size={18} />{confirmRotate ? 'Tap again: the current link stops working' : 'Replace the unlisted link'}
							{:else}
								<Link2 size={18} />Make an unlisted link for staff
							{/if}
						</Button>
					</div>
				</section>
			{/if}

			<h2 class="mb-3 font-display text-3xl uppercase tracking-display leading-none">History</h2>
			<TransactionSheet {rows} href={() => '#'} />

			{#if more}
				<div class="mt-4 flex justify-center">
					<Button variant="secondary" disabled={loading} onclick={() => load(rows[rows.length - 1]?.id)}>
						{loading ? 'Loading' : 'Load more'}
					</Button>
				</div>
			{/if}

			<section class="mt-8 rounded-lg border border-[#d7c8b4] bg-[#fffaf0] p-4 dark:border-[#3a342a] dark:bg-[#211f1a]">
				<h2 class="font-display text-2xl uppercase tracking-display leading-none">Export</h2>
				<p class="mt-1 text-sm text-[#776b5a] dark:text-[#b9aa91]">
					Download the sales between two dates as a CSV file, with each sale's stage and warnings.
				</p>
				<div class="mt-3 grid grid-cols-2 gap-3">
					<label class="flex flex-col gap-1.5">
						<span class="text-xs font-medium uppercase tracking-[0.12em] text-[#776b5a] dark:text-[#b9aa91]">From</span>
						<input type="date" bind:value={from} class="min-h-12 rounded-lg border border-[#d7c8b4] bg-[#fffaf0] px-3 dark:border-[#3a342a] dark:bg-[#161512]" />
					</label>
					<label class="flex flex-col gap-1.5">
						<span class="text-xs font-medium uppercase tracking-[0.12em] text-[#776b5a] dark:text-[#b9aa91]">To</span>
						<input type="date" bind:value={to} class="min-h-12 rounded-lg border border-[#d7c8b4] bg-[#fffaf0] px-3 dark:border-[#3a342a] dark:bg-[#161512]" />
					</label>
				</div>
				<div class="mt-3">
					<Button variant="secondary" disabled={exporting || !from || !to} onclick={exportCsv}>
						<Download size={18} />{exporting ? 'Exporting' : 'Export CSV'}
					</Button>
				</div>
			</section>
		{/if}

		<BullFooter />
	</div>
</main>
