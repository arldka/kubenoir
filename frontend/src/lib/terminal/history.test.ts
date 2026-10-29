import { describe, expect, it } from 'vitest';

import { History, type KeyValueStorage } from './history';

function fakeStorage(initial: Record<string, string> = {}): KeyValueStorage & { data: Record<string, string> } {
	const data = { ...initial };
	return { data, getItem: (k) => data[k] ?? null, setItem: (k, v) => void (data[k] = v) };
}

describe('History', () => {
	it('stores trimmed commands, skipping blanks and immediate repeats', () => {
		const h = new History('k', null);
		['  get pods  ', '', 'get pods', 'logs x', 'get pods'].forEach((l) => h.add(l));
		expect(h.entries()).toEqual(['get pods', 'logs x', 'get pods']);
	});

	it('keeps only the newest entries up to the limit', () => {
		const h = new History('k', null, 3);
		['a', 'b', 'c', 'd'].forEach((l) => h.add(l));
		expect(h.entries()).toEqual(['b', 'c', 'd']);
	});

	it('persists and reloads per key', () => {
		const storage = fakeStorage();
		new History('case-1', storage).add('get pods');
		expect(new History('case-1', storage).entries()).toEqual(['get pods']);
		expect(new History('case-2', storage).entries()).toEqual([]);
	});

	it('ignores corrupt stored data and non-string entries', () => {
		expect(new History('k', fakeStorage({ k: '{nope' })).entries()).toEqual([]);
		expect(new History('k', fakeStorage({ k: '"text"' })).entries()).toEqual([]);
		expect(new History('k', fakeStorage({ k: '["a", 1, null, "b"]' })).entries()).toEqual(['a', 'b']);
	});

	it('keeps working when storage throws', () => {
		const broken: KeyValueStorage = {
			getItem: () => {
				throw new Error('blocked');
			},
			setItem: () => {
				throw new Error('full');
			}
		};
		const h = new History('k', broken);
		h.add('get pods');
		expect(h.entries()).toEqual(['get pods']);
	});
});
