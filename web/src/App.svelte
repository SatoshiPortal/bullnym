<script lang="ts">
	// The cashier screen and the public checkout are one page: "/<nym>" for a
	// store's public link and "/c/<secret>" for its unlisted link. Any device
	// with the link takes payments; there is no pairing and no device token.
	// Inside a link, the app is hash-routed like bullnym's POS. "/owner" is
	// the owner's history, behind the owner login.
	import { ApiError } from '$lib/api'
	import { router } from '$lib/router.svelte'
	import { client, session } from '$lib/session.svelte'
	import BullSpinner from '$lib/components/BullSpinner.svelte'
	import HistoryScreen from './screens/HistoryScreen.svelte'
	import KeypadScreen from './screens/KeypadScreen.svelte'
	import LandingScreen from './screens/LandingScreen.svelte'
	import OwnerScreen from './screens/OwnerScreen.svelte'
	import PayScreen from './screens/PayScreen.svelte'
	import ReceiptScreen from './screens/ReceiptScreen.svelte'
	import SettingsScreen from './screens/SettingsScreen.svelte'

	const isOwner = /^\/owner\/?$/.test(location.pathname)

	let loadError = $state<string | null>(null)

	$effect(() => {
		if (!session.link) return
		let current = true
		loadError = null
		client()
			.store()
			.then((s) => {
				if (current) session.store = s
			})
			.catch((e) => {
				if (!current) return
				if (e instanceof ApiError && e.isNotFound) loadError = 'This checkout link does not work. Ask the owner for the current link.'
				else if (e instanceof ApiError && e.status === 0) loadError = 'The server is not reachable.'
				else loadError = 'Could not load the store.'
			})
		return () => {
			current = false
		}
	})

	const payId = $derived(router.match('/pay/:id')?.id)
	const receiptId = $derived(router.match('/receipt/:id')?.id)
</script>

{#if isOwner}
	<OwnerScreen />
{:else if !session.link}
	<LandingScreen />
{:else if !session.store}
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
