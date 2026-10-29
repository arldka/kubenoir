import { describe, expect, it } from 'vitest';

import { caseState, loadCompleted, markCompleted } from './progress';

function memory(initial?: string) {
	let value = initial ?? null;
	return {
		getItem: () => value,
		setItem: (_: string, v: string) => void (value = v)
	};
}

describe('progress', () => {
	it('starts empty, remembers cases and ignores damaged storage', () => {
		const store = memory();
		expect([...loadCompleted(store)]).toEqual([]);
		markCompleted('a', store);
		expect([...markCompleted('b', store)]).toEqual(['a', 'b']);
		expect([...loadCompleted(store)]).toEqual(['a', 'b']);
		expect([...loadCompleted(memory('{nope'))]).toEqual([]);
		expect([...loadCompleted(memory('[1,"x"]'))]).toEqual(['x']);
	});

	it('works without storage', () => {
		expect([...loadCompleted(null)]).toEqual([]);
		expect([...markCompleted('a', null)]).toEqual(['a']);
	});

	it('finished cases are done, the first open one is current, the rest are locked', () => {
		const cases = [{ id: 'a' }, { id: 'b' }, { id: 'c' }];
		expect(caseState(cases, new Set()).map((c) => c.state)).toEqual(['current', 'locked', 'locked']);
		expect(caseState(cases, new Set(['a'])).map((c) => c.state)).toEqual(['done', 'current', 'locked']);
		expect(caseState(cases, new Set(['a', 'b', 'c'])).map((c) => c.state)).toEqual(['done', 'done', 'done']);
		// Replaying an old case does not unlock anything out of order.
		expect(caseState(cases, new Set(['b'])).map((c) => c.state)).toEqual(['current', 'done', 'locked']);
	});
});
