<script lang="ts">
	// A confirmation over the screen, in the POS's colors: a title, a short
	// text, and Cancel and Continue. Lee's POS had no dialog to reuse.
	import Button from './Button.svelte'

	let {
		title,
		text,
		confirmLabel = 'Continue',
		busy = false,
		onconfirm,
		oncancel,
	}: {
		title: string
		text: string
		confirmLabel?: string
		busy?: boolean
		onconfirm: () => void
		oncancel: () => void
	} = $props()
</script>

<div class="fixed inset-0 z-50 grid place-items-center bg-black/40 px-5" role="presentation">
	<div
		class="w-full max-w-sm rounded-lg border border-[#d7c8b4] bg-[#fffaf0] p-5 text-[#211f1a] shadow-lg dark:border-[#3a342a] dark:bg-[#211f1a] dark:text-[#fff6e8]"
		role="dialog"
		aria-modal="true"
		aria-labelledby="confirm-title"
	>
		<h2 id="confirm-title" class="font-display text-2xl uppercase tracking-display leading-none">{title}</h2>
		<p class="mt-3 text-sm">{text}</p>
		<div class="mt-5 grid grid-cols-2 gap-3">
			<Button variant="secondary" disabled={busy} onclick={oncancel}>Cancel</Button>
			<Button disabled={busy} onclick={onconfirm}>{busy ? 'Adding' : confirmLabel}</Button>
		</div>
	</div>
</div>
