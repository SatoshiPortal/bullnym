// Exercise the watcher's production selection query with a frozen PostgreSQL
// transaction clock. Only the resulting eligible sale IDs feed the browser fixture.
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

export function watchedAtSevenDays() {
	const source = readFileSync(new URL('../../src/watcher.rs', import.meta.url), 'utf8')
	const start = source.indexOf('async fn watched_addresses(')
	assert(start >= 0, 'watcher entry point')
	const query = source.slice(start).match(/conn\.query\("([\s\S]*?)",\s*&\[&\(app\.cfg\.sales\.onchain_watch_secs/)
	assert(query, 'expected the configured watch-window SQL')
	const bindir = execFileSync('pg_config', ['--bindir'], { encoding: 'utf8' }).trim()
	const db = mkdtempSync(join(tmpdir(), 'bullnym-window-'))
	try {
		execFileSync(join(bindir, 'initdb'), ['--no-locale', '--no-sync', '-A', 'trust', '-D', db], { stdio: 'pipe' })
		const statements = [
			'BEGIN',
			'CREATE TEMP TABLE sale (id bigint, public_id text, state text, progress text, created_at timestamptz, expires_at timestamptz)',
			'CREATE TEMP TABLE sale_payment (id bigint, sale_id bigint, rail text, onchain_address text, received_sat bigint, pending_sat bigint, warnings text[])',
			"INSERT INTO sale SELECT id, id::text, 'paid', NULL, NOW() - INTERVAL '7 days' + delta, NOW() - INTERVAL '6 days' FROM (VALUES (1, INTERVAL '1 microsecond'), (2, INTERVAL '0'), (3, INTERVAL '-1 microsecond')) AS offsets(id,delta)",
			"INSERT INTO sale_payment SELECT id, id, 'onchain', 'regtest-address-' || id, 60000, 0, '{}' FROM sale",
			'PREPARE watch(double precision) AS ' + query[1],
			'EXECUTE watch(604800)',
			'ROLLBACK',
		]
		const input = statements.map((s) => s.replace(/\s+/g, ' ').trim() + ';').join('\n') + '\n'
		const output = execFileSync(join(bindir, 'postgres'), ['--single', '-c', 'exit_on_error=on', '-D', db, 'postgres'], { input, encoding: 'utf8', stdio: 'pipe' })
		const ids = [...output.matchAll(/sale_id = "(\d+)"/g)].map((m) => Number(m[1]))
		assert.deepEqual(ids, [1], 'only the paid sale one microsecond inside the window should still be watched')
		return new Set(ids)
	} finally { rmSync(db, { recursive: true, force: true }) }
}
