<script lang="ts">
	// Ported from bullnym, which ported nostr-pos's TransactionSheet: the same
	// markup and classes over the service's sales, with each sale's stage and
	// warnings.
	import { ChevronRight } from 'lucide-svelte'
	import type { Sale } from '$lib/api'
	import { formatFiat } from '$lib/money'
	import { railLabel, saleAmountLabel, stageLabel, stateBorderTone } from '$lib/sale'

	let { rows, href = (id: string) => `#/receipt/${id}` }: { rows: Sale[]; href?: (id: string) => string } = $props()

	function when(sale: Sale): string {
		return new Date(sale.paid_at ?? sale.created_at).toLocaleString([], {
			month: 'short',
			day: 'numeric',
			hour: '2-digit',
			minute: '2-digit',
		})
	}
</script>

<section>
	{#if rows.length === 0}
		<p
			class="rounded-lg border border-[#d7c8b4] bg-[#fffaf0] py-12 text-center text-sm text-[#776b5a] dark:border-[#3a342a] dark:bg-[#211f1a] dark:text-[#b9aa91]"
		>
			Sales will appear here.
		</p>
	{:else}
		<div class="space-y-1.5">
			{#each rows as row (row.id)}
				<a
					class={`grid grid-cols-[1fr_auto] items-center gap-3 rounded-lg border-l-4 bg-[#fbf4e8] px-4 py-3 transition hover:bg-[#f3e7d6] dark:bg-[#26231d] ${stateBorderTone(row)}`}
					href={href(row.id)}
				>
					<div class="min-w-0">
						<div class="flex items-baseline gap-2">
							<span class="font-black tabular-nums">{saleAmountLabel(row, (a, c) => formatFiat(a, c))}</span>
							<span class="text-xs font-semibold text-[#776b5a] dark:text-[#b9aa91]">{stageLabel(row)}</span>
						</div>
						<div class="mt-0.5 truncate text-xs text-[#776b5a] dark:text-[#b9aa91]">
							{when(row)} &middot; {railLabel(row.paid_rail)}{row.cashier_name ? ` · ${row.cashier_name}` : ''}{row.memo
								? ` · ${row.memo}`
								: ''}
						</div>
						{#if row.warnings.length > 0 && row.state === 'in-progress'}
							<div class="mt-0.5 truncate text-xs font-semibold text-[#725315] dark:text-[#f0d38a]">
								Not guaranteed{row.warnings.includes('low-fee') ? ' · low fee' : ''}{row.warnings.includes('unconfirmed-parents')
									? ' · unconfirmed parents'
									: ''}{row.warnings.includes('replaced') ? ' · replaced' : ''}
							</div>
						{/if}
					</div>
					<ChevronRight size={18} class="text-[#b1a287] dark:text-[#6d634f]" />
				</a>
			{/each}
		</div>
	{/if}
</section>
