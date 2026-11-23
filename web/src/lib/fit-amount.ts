// Start at the screen's authored size. Only reduce an amount if its rendered
// text would cross the edge, including after a font load or a viewport resize.
export function fitAmount(node: HTMLElement, _amount: string) {
	const text = document.createRange()
	let frame = 0

	function fit() {
		frame = 0
		node.style.removeProperty('font-size')
		text.selectNodeContents(node)
		const available = node.clientWidth
		if (!available || text.getBoundingClientRect().width <= available) return

		let high = parseFloat(getComputedStyle(node).fontSize)
		let low = 1
		while (high - low > 0.5) {
			const size = (high + low) / 2
			node.style.fontSize = size + 'px'
			if (text.getBoundingClientRect().width > available) high = size
			else low = size
		}
		node.style.fontSize = low + 'px'
	}

	function schedule() {
		if (!frame) frame = requestAnimationFrame(fit)
	}

	let width = 0
	const observer = new ResizeObserver(([entry]) => {
		if (entry && entry.contentRect.width !== width) {
			width = entry.contentRect.width
			schedule()
		}
	})
	observer.observe(node)
	window.addEventListener('resize', schedule)
	document.fonts.addEventListener('loadingdone', schedule)
	schedule()
	return {
		update: schedule,
		destroy() {
			cancelAnimationFrame(frame)
			observer.disconnect()
			window.removeEventListener('resize', schedule)
			document.fonts.removeEventListener('loadingdone', schedule)
		},
	}
}
