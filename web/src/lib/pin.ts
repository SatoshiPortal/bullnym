// The cashier PIN that unlocks the list of this device's sales. The service
// checks it: the page never holds the hash.
import { ApiError } from './api'

export const MIN_PIN_DIGITS = 4
export const MAX_PIN_DIGITS = 12

/** A key of the keypad applied to the PIN typed so far. */
export function applyPinInput(pin: string, key: string): string {
	if (key === 'back') return pin.slice(0, -1)
	if (/^[0-9]$/.test(key) && pin.length < MAX_PIN_DIGITS) return pin + key
	return pin
}

/** What to tell the cashier after the service refused a PIN. */
export function pinRefusal(e: unknown): string {
	if (e instanceof ApiError) {
		const wait = e.retryAfter > 0 ? ` Try again in ${waitText(e.retryAfter)}.` : ''
		if (e.status === 401) return `Wrong PIN.${wait}`
		if (e.status === 429) return `Too many PIN checks.${wait || ' Try again later.'}`
		if (e.status === 404) return 'The owner has not set a cashier PIN.'
		if (e.status === 0) return 'The server is not reachable'
	}
	return 'Could not check the PIN'
}

function waitText(secs: number): string {
	if (secs < 60) return `${secs} s`
	return `${Math.ceil(secs / 60)} min`
}
