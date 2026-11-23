<script lang="ts">
	// Terminal shell. The store's link ("/<nym>") opens the terminal on any
	// device, with no pairing; the app is hash-routed like bullnym's POS.
	import { tick, untrack } from 'svelte'
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

	let generation = 0
	let retry: ReturnType<typeof setTimeout>
	let retryMs = 2000
	async function loadCheckout() {
		clearTimeout(retry)
		const attempt = ++generation
		try {
			const checkout = await client().checkout()
			if (attempt !== generation) return
			retryMs = 2000
			session.store = { kind: 'ready', checkout }
		} catch (e) {
			if (attempt !== generation) return
			const missing = e instanceof ApiError && e.status === 404
			if (missing || !session.checkout) {
				const message = missing ? 'There is no store at this link.' :
					e instanceof ApiError && e.status === 0 ? 'The server is not reachable.' : 'Could not load the store.'
				session.store = { kind: 'error', message }
			}
			if (!missing) {
				retry = setTimeout(loadCheckout, retryMs)
				retryMs = Math.min(retryMs * 2, 30000)
			}
		}
	}

	$effect(() => {
		if (!session.link) return
		const visible = () => { if (!document.hidden) void loadCheckout() }
		untrack(loadCheckout)
		window.addEventListener('focus', loadCheckout)
		window.addEventListener('online', loadCheckout)
		document.addEventListener('visibilitychange', visible)
		return () => {
			generation++
			clearTimeout(retry)
			window.removeEventListener('focus', loadCheckout)
			window.removeEventListener('online', loadCheckout)
			document.removeEventListener('visibilitychange', visible)
		}
	})

	const payId = $derived(router.match('/pay/:id')?.id)
	const receiptId = $derived(router.match('/receipt/:id')?.id)

	$effect(() => {
		const name = session.checkout?.store.name ?? 'Point of sale'
		const screen = payId ? 'Payment' : receiptId ? 'Receipt' :
			router.path === '/history' ? 'Recent sales' : router.path === '/settings' ? 'Settings' : ''
		document.title = screen ? screen + ' · ' + name : name
	})
	let previousPath = router.path
	$effect(() => {
		const path = router.path
		if (path === previousPath) return
		previousPath = path
		tick().then(() => {
			if (router.path !== path) return
			const main = document.querySelector('main')
			main?.setAttribute('tabindex', '-1')
			main?.focus({ preventScroll: true })
			window.scrollTo(0, 0)
		})
	})
</script>

{#if !session.link}
	<UnpairedScreen />
{:else if !session.checkout}
	<main class="grid min-h-screen place-items-center bg-[#f5f0e8] px-5 text-[#211f1a] dark:bg-[#161512] dark:text-[#fff6e8]">
		{#if session.store.kind === 'error'}
			<div class="text-center">
				<p class="font-semibold">{session.store.message}</p>
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
	<KeypadScreen refreshCheckout={loadCheckout} />
{/if}
