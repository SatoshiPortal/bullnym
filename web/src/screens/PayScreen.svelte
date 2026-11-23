<script lang="ts">
	import { fitAmount } from '$lib/fit-amount'
	// After bullnym's PayScreen and PayFlow: the amount, one QR code for the
	// selected rail, and the payment state, kept current with the service's long-poll.
	// A payment seen but not counted yet shows an amber "in progress" panel
	// that says it is not guaranteed, with its warnings. Only a paid sale is
	// green.
	import { untrack } from 'svelte'
	import { ApiError } from '$lib/api'
	import { formatFiat, formatSats } from '$lib/money'
	import { router } from '$lib/router.svelte'
	import { canAddBitcoin, countdown, missingSats, noticeText, progressText, railLabel, saleAmountLabel } from '$lib/sale'
	import { followSale } from '$lib/follow-sale.svelte'
	import { client, session } from '$lib/session.svelte'
	import BullSpinner from '$lib/components/BullSpinner.svelte'
	import Button from '$lib/components/Button.svelte'
	import ConfirmDialog from '$lib/components/ConfirmDialog.svelte'
	import CopyRow from '$lib/components/CopyRow.svelte'
	import HeaderLinks from '$lib/components/HeaderLinks.svelte'
	import ChoiceTabs from '$lib/components/ChoiceTabs.svelte'
	import QrCard from '$lib/components/QrCard.svelte'
	import SuccessScreen from '$lib/components/SuccessScreen.svelte'
	import TerminalPanel from '$lib/components/TerminalPanel.svelte'

	let { id, refreshCheckout }: { id: string; refreshCheckout: () => Promise<void> } = $props()

	const reader = untrack(() => followSale(id))
	const sale = $derived(reader.sale)
	const offline = $derived(reader.kind === 'retrying')
	let cancelState = $state<'idle' | 'sending' | 'failed' | 'conflict'>('idle')
	let nowMs = $state(Date.now())

	$effect(() => {
		const timer = setInterval(() => (nowMs = Date.now()), 1000)
		return () => clearInterval(timer)
	})

	// An expired sale returns to the keypad on its own, as in bullnym.
	const returnAfterExpiry = $derived(sale?.state === 'expired' && !sale.late_payment)
	$effect(() => {
		if (!returnAfterExpiry) return
		const t = setTimeout(newSale, 1800)
		return () => clearTimeout(t)
	})

	const amountLabel = $derived(sale ? saleAmountLabel(sale) : '')
	const remaining = $derived(sale ? countdown(sale.expires_at, nowMs) : null)
	const progress = $derived(sale ? progressText(sale) : null)
	const notice = $derived(sale ? noticeText(sale) : null)
	const missing = $derived(sale ? missingSats(sale) : 0)
	const funds = $derived.by(() => {
		if (!sale) return ''
		const received = formatSats(sale.received_sat) + ' received.'
		return sale.seen_sat ? received + ' ' + formatSats(sale.seen_sat) + ' unconfirmed.' : received
	})

	let selected = $state<'lightning' | 'onchain'>('lightning')
	let option = $state<'idle' | 'confirm' | 'requesting' | 'failed'>('idle')
	const beforeDeadline = $derived(remaining !== null)
	const eligible = $derived(beforeDeadline && canAddBitcoin(sale, session.checkout?.store.onchain_min_sat ?? null))
	const minimum = $derived(session.checkout?.store.onchain_min_usd)
	const qrValue = $derived(selected === 'onchain' ? sale?.onchain_request : sale?.lightning_request)

	$effect(() => {
		if (!eligible) option = 'idle'
	})

	function chooseRail(rail: 'lightning' | 'onchain') {
		selected = rail
		option = rail === 'onchain' && eligible && cancelState !== 'sending' ? 'confirm' : 'idle'
		if (rail === 'onchain' && !sale?.onchain_request && !eligible && beforeDeadline) void refreshCheckout()
	}
	async function addBitcoin() {
		if (option !== 'confirm' || !eligible) return
		option = 'requesting'
		try {
			const next = await client().addOnchain(id)
			reader.accept(next)
			option = 'idle'
		} catch (error) {
			if (eligible && option === 'requesting') option = 'failed'
			if (error instanceof ApiError && (error.status === 400 || error.status === 409)) void refreshCheckout()
			reader.refresh()
		}
	}

	function newSale() {
		router.go('/')
	}

	async function cancel() {
		if (sale?.state !== 'open' || cancelState === 'sending') return
		cancelState = 'sending'
		try {
			const next = await client().cancelSale(sale.id)
			if (reader.accept(next) && reader.sale?.state === 'canceled' && !reader.sale.late_payment) newSale()
			cancelState = 'idle'
		} catch (e) {
			cancelState = e instanceof ApiError && e.status === 409 ? 'conflict' : 'failed'
			reader.refresh()
		}
	}
</script>

<main class="min-h-screen bg-[#f5f0e8] text-[#211f1a] dark:bg-[#161512] dark:text-[#fff6e8]">
	<div class="mx-auto grid min-h-screen max-w-4xl grid-rows-1">
		<section class="px-5 py-5 sm:px-8">
			<header class="mb-6 flex items-center justify-between">
				<button
					type="button"
					class="inline-flex min-h-12 items-center gap-2 rounded-md px-2 text-sm font-semibold disabled:opacity-50"
					onclick={sale?.state === 'open' ? cancel : newSale}
					disabled={sale?.state === 'open' && cancelState === 'sending'}
				>
					{sale?.state === 'open' ? (cancelState === 'sending' ? 'Canceling…' : 'Cancel sale') : '← New sale'}
				</button>
				<HeaderLinks />
			</header>

			{#if offline}
				<p role="status" class="mb-3 text-center">Reconnecting…</p>
			{/if}
			{#if sale?.state === 'open' && (cancelState === 'failed' || cancelState === 'conflict')}
				<p role="alert" class="mb-3 text-center">{cancelState === 'conflict' ? 'Cancellation refused. Checking the payment.' : 'Could not cancel the sale. Check its status and try again.'}</p>
			{/if}
			{#if reader.kind === 'missing'}
				<TerminalPanel icon="×" tone="err" title="Not found" detail="This sale could not be found." actionLabel="Back" onaction={newSale} />
			{:else if !sale}
				<div class="grid min-h-[60vh] place-items-center">
					<BullSpinner size={72} label="Preparing" />
				</div>
			{:else if sale.state === 'paid'}
				<div class="flex flex-col items-center gap-3">
					<SuccessScreen
						{amountLabel}
						rail={sale.paid_rail ? railLabel(sale.paid_rail) : null}
						actionLabel="New Sale"
						onaction={newSale}
						secondaryLabel="Receipt"
						onsecondary={() => router.go(`/receipt/${id}`)}
					/>
					{#if sale.received_sat > sale.amount_sat}
						<p class="max-w-sm rounded-md bg-[#fff0c7] px-4 py-3 text-center text-sm font-semibold text-[#725315] dark:bg-[#3a321f] dark:text-[#f0d38a]">
							Paid more than the sale: received {formatSats(sale.received_sat)}, expected {formatSats(sale.amount_sat)}.
						</p>
					{:else if sale.received_sat < sale.amount_sat}
						<p class="text-center text-sm" role="status">Currently received {formatSats(sale.received_sat)} of the {formatSats(sale.amount_sat)} sale total.</p>
					{/if}
				</div>
			{:else if (sale.state === 'expired' || sale.state === 'canceled') && sale.late_payment}
				<TerminalPanel
					icon="!"
					tone="warn"
					title="Late payment"
					detail={`This sale is ${sale.state}. ${funds} The owner sees the payment in the history.`}
					actionLabel="New Sale"
					onaction={newSale}
				/>
			{:else if sale.state === 'expired'}
				<TerminalPanel icon="×" tone="err" title="Expired"
					detail={sale.received_sat > 0
						? `This sale expired with ${formatSats(sale.received_sat)} received. Returning...`
						: 'This sale expired without payment. Returning...'} />
			{:else if sale.state === 'canceled'}
				<TerminalPanel icon="×" tone="err" title="Canceled" detail="This sale was canceled." actionLabel="Back" onaction={newSale} />
			{:else if sale.state === 'in-progress'}
				<div class="mx-auto flex max-w-md flex-col items-center gap-5 py-6 text-center">
					<h1 use:fitAmount={amountLabel} class="w-full [contain:inline-size] whitespace-nowrap text-center text-7xl font-display tabular-nums tracking-display leading-none">{amountLabel}</h1>
					<BullSpinner size={72} label="Payment in progress" />
					<div class="rounded-md bg-[#fff0c7] px-5 py-4 text-[#725315] dark:bg-[#3a321f] dark:text-[#f0d38a]" role="status">
						<p class="font-semibold">{progress?.title}. It is not paid yet.</p>
						{#each progress?.lines ?? [] as line (line)}
							<p class="mt-1 text-sm">{line}</p>
						{/each}
						{#if sale.seen_sat || sale.received_sat}<p class="mt-1 text-sm">{funds}</p>{/if}
						<p class="mt-1 text-sm">You can start a new sale. The owner can follow this payment in the wallet.</p>
					</div>
					<Button variant="secondary" onclick={newSale}>New Sale</Button>
				</div>
			{:else}
				<div class="mx-auto flex w-full max-w-md flex-col items-center gap-5">
					<div class="flex w-full flex-col items-center gap-1">
						<h1 use:fitAmount={amountLabel} class="w-full [contain:inline-size] whitespace-nowrap text-center text-7xl font-display tabular-nums tracking-display leading-none">{amountLabel}</h1>
						{#if sale.fiat_amount !== null}
							<p class="text-sm tabular-nums text-[#776b5a] dark:text-[#b9aa91]">{formatSats(sale.amount_sat)}</p>
						{/if}
						<p class="inline-flex items-center gap-1.5 text-xs font-semibold text-[#725315] dark:text-[#f0d38a]">
							<span class="inline-block h-1.5 w-1.5 rounded-full bg-current"></span>
							{remaining ? `Waiting for payment · ${remaining}` : 'Payment time elapsed. Checking the payment status.'}
						</p>
						{#if sale.memo}
							<p class="w-full wrap-anywhere whitespace-pre-wrap text-center text-sm text-[#776b5a] dark:text-[#b9aa91]">{sale.memo}</p>
						{/if}
					</div>

					{#if notice}
						<p class="w-full rounded-md bg-[#ffe0d9] px-4 py-3 text-sm font-semibold text-[#8c2d28] dark:bg-[#3a211e] dark:text-[#e8a49e]" role="alert">
							{notice} The sale is open again.
						</p>
					{/if}
					{#if missing > 0}
						<p class="w-full rounded-md bg-[#fff0c7] px-4 py-3 text-sm font-semibold text-[#725315] dark:bg-[#3a321f] dark:text-[#f0d38a]">
							{funds} {formatSats(missing)} short of the amount.
						</p>
					{/if}

					<ChoiceTabs label="Payment method" panel="payment-code" value={selected}
						choices={[{ value: 'lightning', label: 'Lightning' }, { value: 'onchain', label: 'On-chain' }]}
						onchange={chooseRail} />

					<div class="flex w-full min-w-0 flex-col gap-5" id="payment-code" role="tabpanel" aria-labelledby={'payment-code-' + selected}>
						{#if selected === 'onchain' && !sale.onchain_request}
							{#if !beforeDeadline}
								<p class="text-center">The payment time has elapsed. A new on-chain address is unavailable.</p>
							{:else if cancelState === 'sending'}
								<p class="text-center" role="status">Canceling the sale…</p>
							{:else if eligible}
								{#if option === 'failed'}
									<p class="text-center" role="alert">Could not add Bitcoin. Check the sale and try again.</p>
								{/if}
								{#if option === 'failed' || option === 'idle'}
									<Button variant="secondary" onclick={() => chooseRail('onchain')}>Request on-chain address</Button>
								{/if}
							{:else}
								<div class="w-full rounded-lg border border-[#d7c8b4] bg-[#fffaf0] p-5 text-center dark:border-[#3a342a] dark:bg-[#211f1a]">
									{#if minimum != null}
										<p class="font-semibold">On-chain minimum: {formatFiat(minimum, 'USD')} USD</p>
									{:else}
										<p class="font-semibold">The store has not provided a USD minimum.</p>
									{/if}
									<p class="mt-2 text-sm">On-chain payments can have high network fees and can take up to 60 minutes to confirm.</p>
									<p class="mt-2 text-sm">{session.checkout?.store.onchain_min_sat != null
										? 'This sale is below the minimum. Choose Lightning to pay.'
										: 'On-chain eligibility is unavailable. Choose Lightning to pay.'}</p>
								</div>
							{/if}
						{:else}
							{#if qrValue}
								<QrCard
									value={qrValue}
									label={selected === 'onchain' ? 'Bitcoin payment code' : 'Lightning payment code'}
								/>
							{/if}

							<div class="flex w-full flex-col gap-2">
								{#if selected === 'onchain' && sale.onchain_address}
									<CopyRow label="Bitcoin address" value={sale.onchain_address} />
								{:else if selected === 'lightning' && sale.lightning_invoice}
									<CopyRow label="Lightning invoice" value={sale.lightning_invoice} />
								{/if}
							</div>
						{/if}
					</div>
				</div>
			{/if}
		</section>
	</div>
	{#if sale?.state === 'open' && (option === 'confirm' || option === 'requesting')}
		<ConfirmDialog
			title="Bitcoin on-chain"
			text="On-chain Bitcoin payments can take up to 60 minutes to confirm. Don't consider the payment complete until it is confirmed."
			busy={option === 'requesting'}
			onconfirm={addBitcoin}
			oncancel={() => chooseRail('lightning')}
		/>
	{/if}
</main>
