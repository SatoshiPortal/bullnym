<script lang="ts" generics="T extends string">
	let { label, choices, value, panel, disabled = false, onchange }: {
		label: string
		choices: { value: T; label: string }[]
		value: T
		panel: string
		disabled?: boolean
		onchange: (choice: T) => void
	} = $props()

	function move(event: KeyboardEvent, index: number) {
		let target: number
		switch (event.key) {
			case 'ArrowRight': target = (index + 1) % choices.length; break
			case 'ArrowLeft': target = (index + choices.length - 1) % choices.length; break
			case 'Home': target = 0; break
			case 'End': target = choices.length - 1; break
			default: return
		}
		event.preventDefault()
		onchange(choices[target]!.value)
		document.getElementById(panel + '-' + choices[target]!.value)?.focus()
	}
</script>

<div class="inline-flex rounded-md bg-[#eadfce] p-0.5 text-xs dark:bg-[#2c2922]" role="tablist" aria-label={label}>
	{#each choices as choice, index (choice.value)}
		<button type="button" role="tab" {disabled} id={panel + '-' + choice.value} aria-controls={panel}
			aria-selected={value === choice.value} tabindex={value === choice.value ? 0 : -1}
			class={`min-h-11 rounded-md px-4 font-semibold transition ${value === choice.value
				? 'bg-[#fffaf0] text-[#1f513a] shadow-sm dark:bg-[#161512] dark:text-[#8bc8a4]'
				: 'text-[#5f5547] dark:text-[#c9bca7]'}`}
			onkeydown={(event) => move(event, index)} onclick={() => onchange(choice.value)}>
			{choice.label}
		</button>
	{/each}
</div>
