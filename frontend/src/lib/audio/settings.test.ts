import { describe, expect, it } from 'vitest';

import type { KeyValueStorage } from '../terminal/history';
import { DEFAULT_SETTINGS, createSettingsStore, gainFor, loadSettings } from './settings';

function storage(initial?: string): KeyValueStorage & { data: Map<string, string> } {
	const data = new Map<string, string>(initial === undefined ? [] : [['k8sd:sound', initial]]);
	return { data, getItem: (k) => data.get(k) ?? null, setItem: (k, v) => void data.set(k, v) };
}

describe('loadSettings', () => {
	it('uses the defaults when nothing is saved', () => {
		expect(loadSettings(storage())).toEqual(DEFAULT_SETTINGS);
		expect(loadSettings(null)).toEqual(DEFAULT_SETTINGS);
	});

	it('reads saved values, clamping the volume', () => {
		expect(loadSettings(storage('{"volume":0.25,"muted":true}'))).toEqual({ volume: 0.25, muted: true });
		expect(loadSettings(storage('{"volume":7}')).volume).toBe(1);
		expect(loadSettings(storage('{"volume":-3}')).volume).toBe(0);
	});

	it('ignores malformed data piece by piece', () => {
		expect(loadSettings(storage('{nope'))).toEqual(DEFAULT_SETTINGS);
		expect(loadSettings(storage('"text"'))).toEqual(DEFAULT_SETTINGS);
		expect(loadSettings(storage('{"volume":"loud","muted":"yes"}'))).toEqual(DEFAULT_SETTINGS);
		expect(loadSettings(storage('{"volume":0.3}'))).toEqual({ volume: 0.3, muted: false });
	});

	it('survives storage that throws', () => {
		const broken: KeyValueStorage = {
			getItem: () => {
				throw new Error('blocked');
			},
			setItem: () => {
				throw new Error('full');
			}
		};
		expect(loadSettings(broken)).toEqual(DEFAULT_SETTINGS);
		const store = createSettingsStore(broken);
		store.setVolume(0.9);
		let seen = 0;
		store.subscribe((s) => (seen = s.volume))();
		expect(seen).toBe(0.9);
	});
});

describe('gainFor', () => {
	it('squares the slider so quiet settings are usable, and mutes to zero', () => {
		expect(gainFor({ volume: 1, muted: false })).toBe(1);
		expect(gainFor({ volume: 0.5, muted: false })).toBe(0.25);
		expect(gainFor({ volume: 0, muted: false })).toBe(0);
		expect(gainFor({ volume: 1, muted: true })).toBe(0);
	});
});

describe('createSettingsStore', () => {
	it('saves every change and starts from what was saved', () => {
		const s = storage();
		const store = createSettingsStore(s);
		store.setVolume(0.4);
		store.toggleMuted();
		expect(JSON.parse(s.data.get('k8sd:sound')!)).toEqual({ volume: 0.4, muted: true });

		const reloaded = createSettingsStore(s);
		let value = DEFAULT_SETTINGS;
		reloaded.subscribe((v) => (value = v))();
		expect(value).toEqual({ volume: 0.4, muted: true });
		reloaded.setMuted(false);
		expect(JSON.parse(s.data.get('k8sd:sound')!).muted).toBe(false);
	});

	it('clamps what it is given', () => {
		const store = createSettingsStore(storage());
		let value = DEFAULT_SETTINGS;
		store.subscribe((v) => (value = v));
		store.setVolume(5);
		expect(value.volume).toBe(1);
		store.setVolume(-1);
		expect(value.volume).toBe(0);
	});
});
