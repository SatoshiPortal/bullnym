import { fileURLToPath } from 'node:url'
import { preview } from 'vite'

// Use the same loopback namespace as the browsers. Only API calls are stubbed.
const dist = new URL(process.env.DIST_URL ?? '../dist/', import.meta.url)
const server = await preview({
	build: { outDir: fileURLToPath(dist) },
	preview: { host: '127.0.0.1', port: 0, proxy: {} },
})
try {
	process.env.APP_ORIGIN = 'http://127.0.0.1:' + server.httpServer.address().port
	await import('./run.mjs')
} finally {
	await new Promise((resolve) => server.httpServer.close(resolve))
}
