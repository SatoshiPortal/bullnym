import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'
import tailwindcss from '@tailwindcss/vite'
import { VitePWA } from 'vite-plugin-pwa'
import { resolve } from 'node:path'

// bullnym embeds dist/ and serves it at the site root. The SPA handles "/"
// and "/<nym>", so assets use root-absolute URLs.
//
// The service serves the manifest of a store's link itself
// ("/<nym>/manifest.webmanifest"), and public/manifest.webmanifest is the
// default, so the plugin must not emit one.
export default defineConfig({
	base: '/',
	plugins: [
		svelte(),
		tailwindcss(),
		VitePWA({
			manifest: false,
			injectRegister: null,
			registerType: 'autoUpdate',
			workbox: {
				globPatterns: ['**/*.{js,css,html,woff2,png,svg}'],
				navigateFallback: '/index.html',
				navigateFallbackDenylist: [/^\/api\//, /^\/\.well-known\//, /\.webmanifest$/],
				runtimeCaching: [],
			},
		}),
	],
	resolve: {
		alias: { $lib: resolve(__dirname, 'src/lib') },
	},
	build: {
		outDir: 'dist',
		assetsDir: 'assets',
	},
	server: {
		proxy: {
			'/api': 'http://127.0.0.1:3080',
			'/.well-known': 'http://127.0.0.1:3080',
			'^/.+/manifest\\.webmanifest$': 'http://127.0.0.1:3080',
		},
	},
})
