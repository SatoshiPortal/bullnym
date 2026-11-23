// Capture every rendered element at named points without storing input values.
import { appendFile, mkdir } from 'node:fs/promises'
import { join } from 'node:path'

export async function checkpoint(page, label, screenshot = false) {
	const dir = page.context().auditDirectory
	if (!dir) return
	await mkdir(dir, { recursive: true })
	const data = await page.evaluate(() => {
		const nodes = Array.from(document.querySelectorAll('body, body *'))
		const index = new Map(nodes.map((el, n) => [el, n]))
		const elements = nodes.map((el) => {
			const box = el.getBoundingClientRect()
			const css = getComputedStyle(el)
			const text = Array.from(el.childNodes).filter((node) => node.nodeType === Node.TEXT_NODE)
				.map((node) => node.textContent.trim()).join(' ').trim()
			return {
				parent: index.get(el.parentElement) ?? null, tag: el.localName, role: el.getAttribute('role'), text,
				name: el.getAttribute('aria-label') ?? el.getAttribute('alt'),
				class: el.getAttribute('class'), disabled: el.matches(':disabled'),
				display: css.display, visibility: css.visibility, overflow: [css.overflowX, css.overflowY],
				box: [box.x, box.y, box.width, box.height].map(Math.round),
				color: css.color, background: css.backgroundColor, font: css.fontSize, weight: css.fontWeight,
			}
		})
		return { route: location.pathname + location.hash, elements }
	})
	await appendFile(join(dir, 'elements.jsonl'), JSON.stringify({ label, ...data }) + '\n')
	if (screenshot) await page.screenshot({ path: join(dir, label + '.png'), fullPage: true })
}
