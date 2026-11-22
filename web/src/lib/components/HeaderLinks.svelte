<script lang="ts">
	// The History and Settings icon buttons from bullnym's POS header. The
	// History button shows only once the owner set a cashier PIN.
	import { History, Settings } from 'lucide-svelte'
	import { session } from '$lib/session.svelte'

	let { history = true }: { history?: boolean } = $props()
</script>

<div class="flex items-center gap-2">
	{#if history && session.checkout?.store.cashier_pin_set}
		<a
			class="grid min-h-12 min-w-12 place-items-center rounded-md bg-[#eadfce] text-[#211f1a] dark:bg-[#2c2922] dark:text-[#fff6e8]"
			href="#/history"
			aria-label="Recent sales"
		>
			<History size={22} />
		</a>
	{/if}
	<a
		class="grid min-h-12 min-w-12 place-items-center rounded-md bg-[#eadfce] text-[#211f1a] dark:bg-[#2c2922] dark:text-[#fff6e8]"
		href="#/settings"
		aria-label="Settings"
	>
		<Settings size={22} />
	</a>
</div>
