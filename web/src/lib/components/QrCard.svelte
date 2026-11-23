<script lang="ts">
	import { copyFeedback } from '$lib/copy.svelte'
	import QRCode from 'qrcode'
	import { Copy } from 'lucide-svelte'
	import Button from './Button.svelte'

	let { value, label = 'Payment code' }: { value: string; label?: string } = $props()
	type Image = { value: string; kind: 'ready'; url: string } | { value: string; kind: 'failed' }
	let image = $state<Image | null>(null)
	const payload = $derived(value)
	const copy = copyFeedback()

	$effect(() => {
		const exactValue = payload
		let current = true
		QRCode.toDataURL(exactValue, {
			margin: 4,
			width: 300,
			errorCorrectionLevel: 'L',
			color: { dark: '#000000', light: '#ffffff' },
		}).then((next) => {
			if (current) image = { kind: 'ready', value: exactValue, url: next }
		}).catch(() => {
			if (current) image = { kind: 'failed', value: exactValue }
		})
		return () => {
			current = false
		}
	})

</script>

<div
	class="mx-auto w-full max-w-sm rounded-lg border border-[#d7c8b4] bg-[#fffaf0] p-4 text-center shadow-sm dark:border-[#3a342a] dark:bg-[#211f1a]"
>
	{#if image?.value === value && image.kind === 'ready'}
		<img class="mx-auto aspect-square w-full max-w-[300px]" src={image.url} alt={label} />
	{:else if image?.value === value && image.kind === 'failed'}
		<p role="alert">Could not display this code. Copy the payment request below.</p>
	{:else}
		<div class="mx-auto aspect-square w-full max-w-[300px]"></div>
	{/if}
	<div class="mt-3 flex flex-wrap items-center justify-center gap-2">
		<Button variant="secondary" onclick={() => copy.write(value)}>
			<Copy size={18} />
			{copy.forValue(value) === 'copied' ? 'Copied' : 'Copy'}
		</Button>
	</div>
	{#if copy.forValue(value) === 'manual'}
		<label>Copy this payment request manually<input readonly {value} onfocus={(event) => event.currentTarget.select()} /></label>
	{/if}
</div>
