<script lang="ts">
	// Terminal shell. A cashier link ("/<nym>" or "/c/<secret>") opens the
	// terminal on any device, with no pairing; the app is hash-routed like
	// bullnym's POS.
	import { ApiError } from '$lib/api'
	import { router } from '$lib/router.svelte'
	import { client, session } from '$lib/session.svelte'
	import BullSpinner from '$lib/components/BullSpinner.svelte'
	import HistoryScreen from './screens/HistoryScreen.svelte'
	import KeypadScreen from './screens/KeypadScreen.svelte'
	import PayScreen from './screens/PayScreen.svelte'
	import ReceiptScreen from './screens/ReceiptScreen.svelte'
	import SettingsScreen from './screens/SettingsScreen.svelte'
	import UnpairedScreen from './screens/UnpairedScreen.svelte'

	let loadError = $state<string | null>(null)

	$effect(() => {
		if (!session.link) return
		let current = true
		loadError = null
		client()
			.checkout()
			.then((c) => {
				if (current) session.checkout = c
			})
			.catch((e) => {
				if (!current) return
				if (e instanceof ApiError && e.status === 404) loadError = 'This link does not work. Ask the owner for the current link.'
				else loadError = e instanceof ApiError && e.status === 0 ? 'The server is not reachable.' : 'Could not load the store.'
			})
		return () => {
			current = false
		}
	})

	const payId = $derived(router.match('/pay/:id')?.id)
	const receiptId = $derived(router.match('/receipt/:id')?.id)
</script>

{#if !session.link}
	<UnpairedScreen />
{:else if !session.checkout}
	<main class="grid min-h-screen place-items-center bg-[#f5f0e8] px-5 text-[#211f1a] dark:bg-[#161512] dark:text-[#fff6e8]">
		{#if loadError}
			<div class="text-center">
				<p class="font-semibold">{loadError}</p>
				<button class="mt-4 min-h-12 rounded-md bg-[#eadfce] px-5 font-semibold dark:bg-[#2c2922]" onclick={() => location.reload()}>
					Try again
				</button>
			</div>
		{:else}
			<BullSpinner size={72} label="Loading" />
		{/if}
	</main>
{:else if payId}
	{#key payId}
		<PayScreen id={payId} />
	{/key}
{:else if receiptId}
	{#key receiptId}
		<ReceiptScreen id={receiptId} />
	{/key}
{:else if router.path === '/history'}
	<HistoryScreen />
{:else if router.path === '/settings'}
	<SettingsScreen />
{:else}
	<KeypadScreen />
{/if}
