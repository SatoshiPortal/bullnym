import { checkpoint } from './checkpoint.mjs'
import { uxJourneys } from './ux.mjs'
import { chromium, firefox } from 'playwright'
import { mkdir, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { Fixture } from './fixture.mjs'
import { journeys } from './journeys.mjs'

const output = resolve(process.env.RESULTS ?? 'browser-results')
await mkdir(output, { recursive: true })
const report = []
for (const engine of (process.env.ENGINES ?? 'chromium,firefox').split(',')) {
	const type = { chromium, firefox }[engine]
	let browser
	try {
		browser = await type.launch({ executablePath: process.env[engine.toUpperCase() + '_EXECUTABLE'] })
	} catch (error) {
		report.push({ engine, result: 'launch failed', message: String(error) })
		console.error(engine, 'launch failed')
		process.exitCode = 1
		continue
	}
	try {
		for (const size of (process.env.SIZES ?? 'desktop,mobile').split(',')) {
			const viewport = size === 'mobile' ? { width: 390, height: 844 } : { width: 1280, height: 800 }
			for (const [id, run] of Object.entries({ ...journeys, ...uxJourneys })) {
				if (process.env.CASES && !process.env.CASES.split(',').includes(id)) continue
				const fixture = new Fixture()
				const contexts = []
				const errors = []
				async function newContext() {
					const ctx = await browser.newContext({
						viewport,
						locale: 'en-US',
						colorScheme: size === 'mobile' ? 'dark' : 'light',
						serviceWorkers: id === 'PWA_REOPEN' ? 'allow' : 'block',
						reducedMotion: 'reduce',
					})
					if (process.env.AUDIT) ctx.auditDirectory = resolve(output, engine, size, id)
					contexts.push(ctx)
					ctx.setDefaultTimeout(8000)
					ctx.setDefaultNavigationTimeout(20000)
					ctx.on('page', (p) => p.on('pageerror', (e) => errors.push(String(e))))
					await ctx.addInitScript(() => {
						Object.defineProperty(window, 'AudioContext', { value: undefined })
					})
					if (id !== 'PWA_REOPEN') await fixture.attach(ctx)
					await ctx.tracing.start({ screenshots: true, snapshots: true })
					return ctx
				}
				const entry = { engine, version: browser.version(), size, viewport, id, result: 'pass' }
				try {
					const context = await newContext()
					const page = await context.newPage()
					await run({ page, context, fixture, newContext })
					await checkpoint(page, 'end')
					if (errors.length) throw Error(errors.join('\n'))
				} catch (e) {
					entry.result = 'fail'
					entry.message = String(e)
					entry.trace = engine + '-' + size + '-' + id + '.zip'
					await contexts[0]?.tracing.stop({ path: resolve(output, entry.trace) })
					process.exitCode = 1
				} finally {
					for (const context of contexts) await context.close()
				}
				report.push(entry)
				console.log(engine, size, id, entry.result, entry.message?.split('\n')[0] ?? '')
				await writeFile(resolve(output, 'results.json'), JSON.stringify(report, null, 2))
			}
		}
	} finally {
		await browser.close()
	}
}
await writeFile(resolve(output, 'results.json'), JSON.stringify(report, null, 2))
