import { fileURLToPath } from 'node:url'
import { preview } from 'vite'

// Use the same loopback namespace as the browsers. Only API calls are stubbed.
const dist = new URL(process.env.DIST_URL ?? '../dist/', import.meta.url)
const server = await preview({
	plugins: [{
		name: 'test-offline-checkout',
		configurePreviewServer(server) {
			server.middlewares.use('/api/v1/checkout/pwa-test', (req, res) => {
				res.setHeader('Content-Type', 'application/json')
				const store = { name: 'Offline checkout', nym: 'pwa-test', currency: 'USD', cashier_pin_set: false,
					lightning_address: 'pwa-test@example.test', onchain_min_usd: '50.00', onchain_min_sat: 50000 }
				res.end(JSON.stringify(req.url === '/rate' ? { currency: 'USD', btc_price: '100000' } : { store }))
			})
		},
	}],
	build: { outDir: fileURLToPath(dist) },
	preview: { host: '127.0.0.1', port: 0, proxy: {} },
})
try {
	process.env.APP_ORIGIN = 'http://127.0.0.1:' + server.httpServer.address().port
	await import('./run.mjs')
} finally {
	await new Promise((resolve) => server.httpServer.close(resolve))
}
