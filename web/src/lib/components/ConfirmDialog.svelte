<script lang="ts">
	// A confirmation over the screen, in the POS's colors: a title, a short
	// text, and Cancel and Continue. Lee's POS had no dialog to reuse.
	import { onMount } from 'svelte'
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
	let dialog: HTMLDialogElement
	onMount(() => { dialog.showModal() })
</script>

<dialog bind:this={dialog} aria-labelledby="confirm-title"
	oncancel={(event) => { event.preventDefault(); if (!busy) oncancel() }}
	class="m-auto w-full max-w-sm rounded-lg border border-[#d7c8b4] bg-[#fffaf0] p-5 text-[#211f1a] shadow-lg backdrop:bg-black/40 dark:border-[#3a342a] dark:bg-[#211f1a] dark:text-[#fff6e8]"
>
		<h2 id="confirm-title" class="font-display text-2xl uppercase tracking-display leading-none">{title}</h2>
		<p class="mt-3 text-sm">{text}</p>
		<div class="mt-5 grid grid-cols-2 gap-3">
			<Button variant="secondary" disabled={busy} onclick={oncancel}>Cancel</Button>
			<Button disabled={busy} onclick={onconfirm}>{busy ? 'Adding' : confirmLabel}</Button>
		</div>
</dialog>
