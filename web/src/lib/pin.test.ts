import { describe, expect, it } from 'vitest'
import { ApiError } from './api'
import { MAX_PIN_DIGITS, applyPinInput, pinRefusal } from './pin'

describe('cashier PIN', () => {
	it('types digits and deletes them', () => {
		expect(applyPinInput('', '1')).toBe('1')
		expect(applyPinInput('12', 'back')).toBe('1')
		expect(applyPinInput('', 'back')).toBe('')
		// The keypad's other keys do nothing.
		expect(applyPinInput('12', '.')).toBe('12')
		expect(applyPinInput('12', '00')).toBe('12')
		expect(applyPinInput('1'.repeat(MAX_PIN_DIGITS), '2')).toBe('1'.repeat(MAX_PIN_DIGITS))
	})

	it('explains a refusal and its wait', () => {
		expect(pinRefusal(new ApiError(401, 'wrong PIN'))).toBe('Wrong PIN.')
		expect(pinRefusal(new ApiError(401, 'wrong PIN', 2))).toBe('Wrong PIN. Try again in 2 s.')
		expect(pinRefusal(new ApiError(429, 'too many', 900))).toBe('Too many wrong PINs. Try again in 15 min.')
		expect(pinRefusal(new ApiError(429, 'too many requests'))).toBe('Too many wrong PINs. Try again later.')
		expect(pinRefusal(new ApiError(404, 'not set'))).toBe('The owner has not set a cashier PIN.')
		expect(pinRefusal(new Error('x'))).toBe('Could not check the PIN')
	})
})
