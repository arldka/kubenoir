import { afterEach, describe, expect, it, vi } from 'vitest';

import { FakeContext, FakeGain, FakeSource, sequence } from './fakeAudio';
import type { Manifest } from './manifest';
import { MAX_VOICES, Soundscape } from './soundscape';

const emptyManifest: Manifest = { keys: [] };

function make(manifest: Manifest = emptyManifest, deps: { fetchBytes?: (url: string) => Promise<ArrayBuffer>; random?: () => number } = {}) {
	const context = new FakeContext();
	const timers: { callback: () => void; ms: number }[] = [];
	const createContext = vi.fn(() => context.asContext());
	const soundscape = new Soundscape({
		manifest,
		createContext,
		// "Files" are n bytes for an n/1000 second recording.
		fetchBytes: deps.fetchBytes ?? (async (url) => new ArrayBuffer(url.includes('ambience') ? 60_000 : 200)),
		random: deps.random ?? (() => 0.5),
		setTimer: (callback, ms) => timers.push({ callback, ms }),
		clearTimer: () => {}
	});
	const sources = () => context.sources as unknown as FakeSource[];
	return { context, soundscape, createContext, timers, sources };
}

afterEach(() => vi.restoreAllMocks());

describe('Soundscape start', () => {
	it('does nothing until it is started from a gesture', () => {
		const { createContext, soundscape } = make();
		soundscape.keystroke('char');
		expect(createContext).not.toHaveBeenCalled();
		expect(soundscape.started).toBe(false);
	});

	it('builds layers -> master -> compressor -> output, once', async () => {
		const { context, soundscape, createContext } = make();
		await Promise.all([soundscape.start(), soundscape.start()]);
		await soundscape.start();
		expect(createContext).toHaveBeenCalledTimes(1);

		const [master, ...layers] = context.gains as FakeGain[];
		expect(layers).toHaveLength(2); // ambience, typing
		for (const layer of layers) expect(layer.connected).toEqual([master]);
		expect(master.connected).toEqual([context.compressors[0]]);
		expect(context.compressors[0].connected).toEqual([context.destination]);
		expect(context.resumeCalls).toBe(1);
	});

	it('applies the saved volume from the first moment, then eases changes', async () => {
		const { context, soundscape } = make();
		soundscape.setSettings({ volume: 0.5, muted: false });
		await soundscape.start();
		const master = context.gains[0] as FakeGain;
		expect(master.gain.value).toBe(0.25);

		soundscape.setSettings({ volume: 1, muted: false });
		soundscape.setSettings({ volume: 1, muted: true });
		const targets = master.gain.ofType('target').map((e) => e.args[0]);
		expect(targets).toEqual([1, 0]);
		expect(master.gain.ofType('target')[0].args[2]).toBeGreaterThan(0); // eased, not instant
	});

	it('survives a browser without audio', async () => {
		const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
		const soundscape = new Soundscape({
			manifest: emptyManifest,
			createContext: () => {
				throw new Error('no audio');
			}
		});
		await expect(soundscape.start()).resolves.toBeUndefined();
		expect(soundscape.started).toBe(false);
		soundscape.keystroke('char');
		expect(warn).toHaveBeenCalled();
	});
});

describe('Soundscape ambience', () => {
	it('loops the soundtrack with a crossfade, scheduling the next pass ahead of time', async () => {
		const { context, soundscape, timers, sources } = make({ keys: [], ambience: '/a/ambience.ogg' });
		await soundscape.start();
		expect(sources()).toHaveLength(1);
		expect(sources()[0].started).toEqual([0.05]);
		expect(sources()[0].loop).toBe(false); // looping is done by overlapping passes
		expect(timers).toHaveLength(1);
		expect(timers[0].ms).toBeCloseTo((0.05 + 60 - 4 - 3) * 1000, 3);

		context.currentTime = 53.05;
		timers[0].callback();
		expect(sources()).toHaveLength(2);
		expect(sources()[1].started[0]).toBeCloseTo(56.05, 6);
	});

	it('plays nothing for the ambience when the file is missing or broken', async () => {
		const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
		const { soundscape, sources } = make(
			{ keys: [], ambience: '/a/ambience.ogg' },
			{ fetchBytes: async () => Promise.reject(new Error('HTTP 404')) }
		);
		await expect(soundscape.start()).resolves.toBeUndefined();
		expect(sources()).toHaveLength(0);
		expect(warn).toHaveBeenCalledWith(expect.stringContaining('/a/ambience.ogg'), expect.anything());
		soundscape.keystroke('char'); // typing still works through the fallback click
		expect(sources()).toHaveLength(1);
	});

	it('stops the soundtrack and closes the context on dispose', async () => {
		const { context, soundscape, sources } = make({ keys: [], ambience: '/a/ambience.ogg' });
		await soundscape.start();
		soundscape.dispose();
		expect(context.closed).toBe(true);
		expect(sources()[0].stopped.length).toBeGreaterThan(0);
		soundscape.keystroke('char');
		expect(sources()).toHaveLength(1);
	});
});

describe('Soundscape typing', () => {
	it('is audible straight away using the synthesized click', async () => {
		const { context, soundscape, sources } = make();
		await soundscape.start();
		expect(context.buffersCreated).toBe(1);
		soundscape.keystroke('char');
		expect(sources()).toHaveLength(1);
		expect(sources()[0].started).toHaveLength(1);
		expect(sources()[0].playbackRate.value).toBeCloseTo(1, 6);
		// source -> its own gain -> the typing layer
		expect(sources()[0].connected).toHaveLength(1);
	});

	it('gives space, return and backspace their own character when no recordings exist', async () => {
		const { soundscape, sources } = make();
		await soundscape.start();
		for (const kind of ['space', 'return', 'backspace'] as const) soundscape.keystroke(kind);
		expect(sources().map((s) => s.playbackRate.value)).toEqual([0.7, 0.5, 1.25]);
	});

	it('varies pitch slightly around the base rate', async () => {
		const low = make(emptyManifest, { random: () => 0 });
		await low.soundscape.start();
		low.soundscape.keystroke('char');
		expect(low.sources()[0].playbackRate.value).toBeCloseTo(0.96, 6);
		const high = make(emptyManifest, { random: () => 0.9999 });
		await high.soundscape.start();
		high.soundscape.keystroke('char');
		expect(high.sources()[0].playbackRate.value).toBeCloseTo(1.04, 2);
	});

	it('uses the recordings and never repeats the same key sound twice in a row', async () => {
		const manifest: Manifest = { keys: ['/k/1', '/k/2', '/k/3'], space: '/k/space' };
		const { context, soundscape, sources } = make(manifest, { random: sequence(0.1, 0.9, 0.4, 0.7, 0.2, 0.55, 0.85, 0.3) });
		await soundscape.start();
		const buffersBefore = context.buffersCreated;
		for (let i = 0; i < 30; i++) soundscape.keystroke('char');
		const played = sources().map((s) => s.buffer);
		expect(played.every((b) => b !== null)).toBe(true);
		for (let i = 1; i < played.length; i++) expect(played[i]).not.toBe(played[i - 1]);
		expect(new Set(played).size).toBe(3);
		expect(context.buffersCreated).toBe(buffersBefore); // the click was never needed for letters

		soundscape.keystroke('space');
		expect(sources().at(-1)!.playbackRate.value).toBeCloseTo(1, 1); // a real recording, not the pitched click
	});

	it('plays nothing while muted or while the tab is suspended', async () => {
		const { context, soundscape, sources } = make();
		await soundscape.start();
		soundscape.setSettings({ volume: 0.6, muted: true });
		soundscape.keystroke('char');
		expect(sources()).toHaveLength(0);

		soundscape.setSettings({ volume: 0.6, muted: false });
		soundscape.suspend();
		await Promise.resolve();
		expect(context.suspendCalls).toBe(1);
		soundscape.keystroke('char');
		expect(sources()).toHaveLength(0);

		soundscape.resume();
		await Promise.resolve();
		soundscape.keystroke('char');
		expect(sources()).toHaveLength(1);
	});

	it('caps simultaneous sounds by cutting off the oldest', async () => {
		const { soundscape, sources } = make();
		await soundscape.start();
		for (let i = 0; i < MAX_VOICES + 4; i++) soundscape.keystroke('char');
		const stopped = sources().filter((s) => s.stopped.length > 0).length;
		expect(stopped).toBe(4);
		expect(sources().slice(0, 4).every((s) => s.stopped.length === 1)).toBe(true);

		// A sound that finished on its own no longer counts toward the cap.
		sources()[MAX_VOICES + 3].onended?.();
		soundscape.keystroke('char');
		expect(sources().filter((s) => s.stopped.length > 0).length).toBe(4);
	});
});
