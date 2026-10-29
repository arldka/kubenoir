import { writable, type Readable } from 'svelte/store';

import type { KeyValueStorage } from '../terminal/history';

export interface SoundSettings {
	/** Master volume, 0 to 1. */
	volume: number;
	muted: boolean;
}

export const DEFAULT_SETTINGS: SoundSettings = { volume: 0.6, muted: false };

const STORAGE_KEY = 'k8sd:sound';

function browserStorage(): KeyValueStorage | null {
	try {
		return globalThis.localStorage ?? null;
	} catch {
		return null;
	}
}

const clamp01 = (n: number) => Math.min(1, Math.max(0, n));

/** Read saved settings; anything missing or malformed falls back to the defaults. */
export function loadSettings(storage: KeyValueStorage | null = browserStorage()): SoundSettings {
	try {
		const raw = storage?.getItem(STORAGE_KEY);
		const parsed: unknown = raw ? JSON.parse(raw) : null;
		if (typeof parsed === 'object' && parsed !== null) {
			const { volume, muted } = parsed as Record<string, unknown>;
			return {
				volume: typeof volume === 'number' && Number.isFinite(volume) ? clamp01(volume) : DEFAULT_SETTINGS.volume,
				muted: typeof muted === 'boolean' ? muted : DEFAULT_SETTINGS.muted
			};
		}
	} catch {
		// fall through to the defaults
	}
	return { ...DEFAULT_SETTINGS };
}

/**
 * The gain the master volume node should have. Loudness is perceived roughly logarithmically, so
 * the slider is squared: the lower half of the slider covers a useful quiet range.
 */
export function gainFor(settings: SoundSettings): number {
	return settings.muted ? 0 : clamp01(settings.volume) ** 2;
}

export interface SoundSettingsStore extends Readable<SoundSettings> {
	setVolume(volume: number): void;
	setMuted(muted: boolean): void;
	toggleMuted(): void;
}

/** A store of the player's sound settings, saved whenever they change. */
export function createSettingsStore(storage: KeyValueStorage | null = browserStorage()): SoundSettingsStore {
	const store = writable<SoundSettings>(loadSettings(storage));
	let current = loadSettings(storage);

	const update = (next: SoundSettings) => {
		current = next;
		store.set(next);
		try {
			storage?.setItem(STORAGE_KEY, JSON.stringify(next));
		} catch {
			// Storage full or blocked: the setting still applies for this visit.
		}
	};

	return {
		subscribe: store.subscribe,
		setVolume: (volume) => update({ ...current, volume: clamp01(volume) }),
		setMuted: (muted) => update({ ...current, muted }),
		toggleMuted: () => update({ ...current, muted: !current.muted })
	};
}

/** The shared settings, backed by localStorage. */
export const soundSettings = createSettingsStore();
