import { describe, expect, it } from 'vitest';

import { parseKeys } from '../terminal/keys';
import { createClickBuffer } from './synth';
import { FakeContext } from './fakeAudio';
import { pickIndex, soundForKey } from './typing';

const sound = (input: string) => parseKeys(input).map(soundForKey);

describe('soundForKey', () => {
	it('makes a sound for typing, space, enter and deleting', () => {
		expect(sound('a')).toEqual(['char']);
		expect(sound(' ')).toEqual(['space']);
		expect(sound('\r')).toEqual(['return']);
		expect(sound('\x7f')).toEqual(['backspace']);
		expect(sound('\x1b[3~')).toEqual(['backspace']);
		expect(sound('\t')).toEqual(['space']);
	});

	it('stays silent for movement and most shortcuts', () => {
		expect(sound('\x1b[A\x1b[B\x1b[C\x1b[D\x1b[H\x1b[F')).toEqual([null, null, null, null, null, null]);
		expect(sound('\x01\x05\x03\x0c')).toEqual([null, null, null, null]);
	});

	it('treats the kill shortcuts like deleting', () => {
		expect(sound('\x15\x0b\x17')).toEqual(['backspace', 'backspace', 'backspace']);
	});

	it('makes one sound for a whole paste, and a return per newline', () => {
		expect(sound('hello world')).toEqual(['char']);
		expect(sound('one\ntwo')).toEqual(['char', 'return', 'char']);
	});
});

describe('pickIndex', () => {
	it('never repeats the previous index when there is a choice', () => {
		for (const count of [2, 3, 5]) {
			for (let last = 0; last < count; last++) {
				for (const r of [0, 0.2, 0.5, 0.8, 0.999]) {
					const picked = pickIndex(count, last, () => r);
					expect(picked).not.toBe(last);
					expect(picked).toBeGreaterThanOrEqual(0);
					expect(picked).toBeLessThan(count);
				}
			}
		}
	});

	it('can reach every other index', () => {
		const seen = new Set<number>();
		for (let i = 0; i < 100; i++) seen.add(pickIndex(4, 2, () => i / 100));
		expect([...seen].sort()).toEqual([0, 1, 3]);
	});

	it('handles the first pick and a single choice', () => {
		expect(pickIndex(1, null)).toBe(0);
		expect(pickIndex(1, 0)).toBe(0);
		expect(pickIndex(3, null, () => 0.99)).toBe(2);
		expect(pickIndex(0, null)).toBe(0);
	});
});

describe('createClickBuffer', () => {
	it('is a short, audible, bounded click', () => {
		const context = new FakeContext();
		const buffer = createClickBuffer(context.asContext());
		const data = buffer.getChannelData(0);
		expect(buffer.duration).toBeCloseTo(0.06, 2);
		expect(Math.max(...data.map(Math.abs))).toBeGreaterThan(0.05);
		expect(Math.max(...data.map(Math.abs))).toBeLessThanOrEqual(1);
		// It decays: the tail is much quieter than the attack.
		const energy = (from: number, to: number) => data.slice(from, to).reduce((s, v) => s + v * v, 0);
		expect(energy(data.length - 400, data.length)).toBeLessThan(energy(0, 400) / 100);
	});
});
