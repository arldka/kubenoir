import { describe, expect, it } from 'vitest';

import { buildManifest } from './manifest';

const all = () => true;
const files = (...names: string[]) => Object.fromEntries(names.map((n) => [`./assets/${n}`, `/url/${n}`]));

describe('buildManifest', () => {
	it('is empty when there are no files', () => {
		expect(buildManifest({}, all)).toEqual({
			ambience: undefined,
			keys: [],
			space: undefined,
			return: undefined,
			backspace: undefined
		});
	});

	it('assigns roles by file name', () => {
		const m = buildManifest(files('ambience.ogg', 'key-1.ogg', 'key-space.ogg', 'key-return.ogg', 'key-backspace.ogg'), all);
		expect(m.ambience).toBe('/url/ambience.ogg');
		expect(m.keys).toEqual(['/url/key-1.ogg']);
		expect(m.space).toBe('/url/key-space.ogg');
		expect(m.return).toBe('/url/key-return.ogg');
		expect(m.backspace).toBe('/url/key-backspace.ogg');
	});

	it('orders numbered keys numerically, not alphabetically', () => {
		const m = buildManifest(files('key-10.ogg', 'key-2.ogg', 'key-1.ogg'), all);
		expect(m.keys).toEqual(['/url/key-1.ogg', '/url/key-2.ogg', '/url/key-10.ogg']);
	});

	it('does not confuse the special keys with numbered ones, and ignores unrelated files', () => {
		const m = buildManifest(files('key-space.ogg', 'key-x.ogg', 'readme.ogg', 'music.ogg', 'notes.txt'), all);
		expect(m.keys).toEqual([]);
		expect(m.space).toBe('/url/key-space.ogg');
		expect(m.ambience).toBeUndefined();
	});

	it('prefers ogg, then falls back to formats the browser can play', () => {
		const both = files('ambience.mp3', 'ambience.ogg');
		expect(buildManifest(both, all).ambience).toBe('/url/ambience.ogg');
		expect(buildManifest(both, (ext) => ext !== 'ogg').ambience).toBe('/url/ambience.mp3');
		expect(buildManifest(both, () => false).ambience).toBeUndefined();
		expect(buildManifest(files('ambience.wav', 'ambience.m4a'), all).ambience).toBe('/url/ambience.m4a');
	});

	it('is case-insensitive about names and extensions', () => {
		expect(buildManifest({ './assets/Ambience.OGG': '/a' }, all).ambience).toBe('/a');
	});
});
