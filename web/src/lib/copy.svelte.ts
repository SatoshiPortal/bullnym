import { onDestroy } from 'svelte'

// Bind feedback to the value copied. A clipboard response or reset timer
// from a previous request must not acknowledge a different payment code.
export function copyFeedback() {
	let feedback = $state<{ value: string; kind: 'copied' | 'manual' } | null>(null)
	let request = 0
	let timer: ReturnType<typeof setTimeout>
	onDestroy(() => { request++; clearTimeout(timer) })
	return {
		forValue(value: string) { return feedback?.value === value ? feedback.kind : null },
		async write(value: string) {
			const attempt = ++request
			clearTimeout(timer)
			try {
				await navigator.clipboard.writeText(value)
				if (attempt !== request) return
				feedback = { kind: 'copied', value }
				timer = setTimeout(() => { feedback = null }, 1500)
			} catch {
				if (attempt === request) feedback = { kind: 'manual', value }
			}
		},
	}
}
