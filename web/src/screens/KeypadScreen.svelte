<script lang="ts">
	// Ported from bullnym's KeypadScreen (nostr-pos's Keypad layout): header
	// with the store name and History/Settings buttons, a big amount, the
	// keypad, a memo, and the Charge button pinned at the bottom.
	import { onDestroy, untrack } from 'svelte'
	import { ApiError, type Rate } from '$lib/api'
	import { applyAmountInput } from '$lib/amount-input'
	import { currencyPrecision, fiatToSats, formatFiat, formatRate, formatSats, normalizeFiat } from '$lib/money'
	import { router } from '$lib/router.svelte'
	import { rememberSale } from '$lib/sale-cache'
	import { client, rememberLocalSale, session } from '$lib/session.svelte'
	import AmountDisplay from '$lib/components/AmountDisplay.svelte'
	import BullFooter from '$lib/components/BullFooter.svelte'
	import Button from '$lib/components/Button.svelte'
	import HeaderLinks from '$lib/components/HeaderLinks.svelte'
	import ChoiceTabs from '$lib/components/ChoiceTabs.svelte'
	import Keypad from '$lib/components/Keypad.svelte'

	let { refreshCheckout }: { refreshCheckout: () => void } = $props()

	const RATE_REFRESH_MS = 60_000

	const store = $derived(session.checkout?.store ?? null)
	const currency = $derived(store?.currency ?? 'USD')

	let unit = $state<'fiat' | 'sat'>('fiat')
	let amount = $state('')
	let memo = $state('')
	let denominationNotice = $state('')
	let active = true
	onDestroy(() => { active = false })
	type Submission = { kind: 'editing' | 'creating' } | { kind: 'error'; message: string }
	let submission = $state<Submission>({ kind: 'editing' })
	const creating = $derived(submission.kind === 'creating')
	const errorMsg = $derived(submission.kind === 'error' ? submission.message : null)
	type Quote = { kind: 'loading' } | { kind: 'current'; rate: Rate } | { kind: 'unavailable'; last: Rate | null }
	let quote = $state<Quote>({ kind: 'loading' })
	const rate = $derived(quote.kind === 'current' ? quote.rate : quote.kind === 'unavailable' ? quote.last : null)
	const rateError = $derived(quote.kind === 'unavailable')

	const precision = $derived(unit === 'sat' ? 0 : currencyPrecision(currency))

	$effect(() => {
		const expected = currency
		quote = { kind: 'loading' }
		// A fiat draft belongs to the denomination on which it was entered.
		if (untrack(() => unit === 'fiat' && amount !== '')) {
			amount = ''
			denominationNotice = 'The store currency changed. Enter the amount in ' + expected + '.'
		}
		let stopped = false
		let generation = 0
		let last: Rate | null = null
		async function load() {
			const request = ++generation
			try {
				const next = await client().rate()
				if (stopped || request !== generation) return
				if (next.currency !== expected) {
					last = null
					quote = { kind: 'unavailable', last }
					refreshCheckout()
					return
				}
				last = next
				quote = { kind: 'current', rate: next }
			} catch {
				if (!stopped && request === generation) quote = { kind: 'unavailable', last }
			}
		}
		void load()
		window.addEventListener('online', load)
		const timer = setInterval(load, RATE_REFRESH_MS)
		return () => {
			stopped = true
			clearInterval(timer)
			window.removeEventListener('online', load)
		}
	})

	const sats = $derived(unit === 'sat' ? Math.trunc(Number(amount || '0')) : rate ? fiatToSats(amount, rate.btc_price) : 0)
	const hint = $derived.by(() => {
		if (!amount || !rate) return undefined
		if (unit === 'fiat') return `≈ ${formatSats(sats)}`
		const fiat = (sats / 1e8) * Number(rate.btc_price)
		return `≈ ${formatFiat(fiat, rate.currency)}`
	})
	const rateReady = $derived(unit === 'sat' || rate !== null)
	const memoTooLong = $derived(Array.from(memo.trim()).length > 200)
	const canCharge = $derived(sats > 0 && rateReady && !memoTooLong && !creating)

	function applyInput(key: string) {
		if (creating) return
		denominationNotice = ''
		submission = { kind: 'editing' }
		amount = applyAmountInput(amount, key, precision)
	}

	function setUnit(next: 'fiat' | 'sat') {
		if (creating || unit === next) return
		denominationNotice = ''
		submission = { kind: 'editing' }
		unit = next
		amount = ''
	}

	async function charge() {
		if (!canCharge) return
		submission = { kind: 'creating' }
		const note = memo.trim() || undefined
		try {
			const sale = await client().createSale(
				unit === 'sat' ? { amount_sat: sats, memo: note } : { fiat_amount: normalizeFiat(amount, precision), memo: note },
			)
			rememberSale(sale)
			rememberLocalSale(sale.id)
			if (!active) return
			amount = ''
			memo = ''
			router.go(`/pay/${sale.id}`)
		} catch (e) {
			let message = e instanceof Error ? e.message : 'Something went wrong'
			if (e instanceof ApiError && (e.status === 0 || e.status === 502)) {
				message = e.message + '. We could not confirm whether the sale was created. Check the owner’s sales before trying again.'
			}
			if (e instanceof ApiError && e.status === 429) message = 'Too many sales at once. Try again in a moment.'
			submission = { kind: 'error', message }
		}
	}
</script>

<main class="h-[100dvh] overflow-hidden bg-[#f5f0e8] text-[#211f1a] dark:bg-[#161512] dark:text-[#fff6e8]">
	<div class="mx-auto flex h-[100dvh] max-w-4xl flex-col overflow-hidden">
		<section class="flex min-h-0 flex-1 flex-col overflow-hidden px-5 py-3 sm:px-8 sm:py-5">
			<header class="mb-3 flex shrink-0 items-center justify-between gap-4 sm:mb-5">
				<div class="min-w-0">
					<h1 class="truncate font-display text-3xl uppercase tracking-display leading-none">{store?.name ?? 'Point of sale'}</h1>
				</div>
				<HeaderLinks />
			</header>

			<div class="mx-auto flex min-h-0 w-full max-w-xl flex-1 flex-col overflow-y-auto scrollbar-none">
				<div class="my-auto flex w-full flex-col gap-3 sm:gap-5">
					<div class="flex justify-end">
						<ChoiceTabs label="Amount unit" panel="amount-entry" value={unit} disabled={creating}
							choices={[{ value: 'fiat', label: currency }, { value: 'sat', label: 'Sats' }]} onchange={setUnit} />
					</div>
					<div role="tabpanel" id="amount-entry" aria-labelledby={'amount-entry-' + unit}>
						<AmountDisplay amount={amount || '0'} unit={unit === 'sat' ? 'sat' : currency} {precision} {hint} />
					</div>
					{#if denominationNotice}<p role="status" class="text-sm">{denominationNotice}</p>{/if}
					<Keypad disabled={creating} {precision} onInput={applyInput} />
					<label class="flex shrink-0 flex-col gap-1.5">
						<span class="text-xs font-medium uppercase tracking-[0.12em] text-[#776b5a] dark:text-[#b9aa91]">Memo</span>
						<textarea
							class="min-h-12 rounded-lg border border-[#d7c8b4] bg-[#fffaf0] px-4 py-3 text-base outline-none focus:ring-2 focus:ring-[#B7000B] dark:border-[#3a342a] dark:bg-[#211f1a]"
							disabled={creating}
							bind:value={memo}
							placeholder="Add a memo for this sale"
							aria-invalid={memoTooLong}
							aria-describedby={memoTooLong ? 'memo-error' : undefined}
							rows="1"
						></textarea>
					</label>
					{#if memoTooLong}<p id="memo-error" role="alert" class="text-sm">Use 200 characters or fewer for the memo.</p>{/if}
					<div class={`text-center text-xs ${rateError ? 'text-[#a9362f] dark:text-[#e8a49e]' : 'text-[#776b5a] dark:text-[#b9aa91]'}`}>
						{#if rate}
							Rate: {formatRate(rate.btc_price, rate.currency)}{rateError ? ' (not updated)' : ''}
						{:else if rateError}
							Rate unavailable. You can still charge in sats.
						{:else}
							Updating rate…
						{/if}
					</div>
					{#if errorMsg}
						<p role="alert" class="rounded-md bg-[#ffe0d9] px-4 py-3 text-sm font-semibold text-[#8c2d28]">{errorMsg}</p>
					{/if}
					<BullFooter />
				</div>
			</div>

			<div
				class="mx-auto mt-2 flex w-full max-w-xl shrink-0 flex-col bg-gradient-to-t from-[#f5f0e8] from-60% to-transparent pb-[max(0.25rem,env(safe-area-inset-bottom))] pt-2 dark:from-[#161512]"
			>
				<Button disabled={!canCharge} onclick={charge}>
					{creating ? 'Preparing' : 'Charge'}
				</Button>
			</div>
		</section>
	</div>
</main>
