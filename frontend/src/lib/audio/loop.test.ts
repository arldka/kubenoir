import { describe, expect, it } from 'vitest';

import { FakeBuffer, FakeContext, FakeGain, FakeSource } from './fakeAudio';
import { CrossfadeLoop } from './loop';

function setup(duration: number, options: { crossfade?: number } = {}) {
	const context = new FakeContext();
	const destination = new FakeGain();
	const timers: { callback: () => void; ms: number; cleared: boolean }[] = [];
	const loop = new CrossfadeLoop(
		context.asContext(),
		new FakeBuffer(duration) as unknown as AudioBuffer,
		destination as unknown as AudioNode,
		{
			crossfade: options.crossfade ?? 4,
			fadeIn: 2,
			lookahead: 3,
			setTimer: (callback, ms) => {
				const timer = { callback, ms, cleared: false };
				timers.push(timer);
				return timer;
			},
			clearTimer: (id) => void ((id as { cleared: boolean }).cleared = true)
		}
	);
	/** Run the most recent timer, with the audio clock where it would be when it fires. */
	const fire = () => {
		const timer = timers[timers.length - 1];
		context.currentTime += timer.ms / 1000;
		timer.callback();
	};
	return { context, destination, timers, loop, fire, source: (i: number) => context.sources[i] as FakeSource, gain: (i: number) => context.gains[i] as FakeGain };
}

describe('CrossfadeLoop', () => {
	it('starts the first pass just ahead, fading in, and schedules the next before it ends', () => {
		const { loop, source, gain, timers, destination } = setup(60);
		loop.start();

		expect(source(0).started).toEqual([0.05]);
		expect(source(0).buffer).not.toBeNull();
		expect(gain(0).connected).toContain(destination);
		expect(gain(0).gain.ofType('set')[0].args).toEqual([0, 0.05]);
		expect(gain(0).gain.ofType('target')[0].args[0]).toBe(1);
		// The pass fades out over its last 4 seconds.
		const out = gain(0).gain.ofType('curve')[0].args;
		expect([out[1], out[2]]).toEqual([0.05 + 56, 4]);
		// The next pass is scheduled 3 s before it is due at 56.05.
		expect(timers).toHaveLength(1);
		expect(timers[0].ms).toBeCloseTo((56.05 - 3) * 1000, 3);
	});

	it('overlaps each pass with the next so the loop has no seam', () => {
		const { loop, source, gain, fire, timers } = setup(60);
		loop.start();
		fire();
		fire();

		// A pass begins every (duration - crossfade) = 56 s, each fading in as the last fades out.
		expect(source(1).started[0]).toBeCloseTo(56.05, 6);
		expect(source(2).started[0]).toBeCloseTo(112.05, 6);
		const fadeIn = gain(1).gain.ofType('curve')[0].args;
		const fadeOut = gain(1).gain.ofType('curve')[1].args;
		expect([fadeIn[1], fadeIn[2]]).toEqual([source(1).started[0], 4]);
		expect(fadeOut[1]).toBeCloseTo(56.05 + 56, 6);
		// The old pass is fully faded by the time it ends, which is when the new one is fully in.
		expect((source(0).started[0] as number) + 60).toBeCloseTo((source(1).started[0] as number) + 4, 6);
		expect(timers).toHaveLength(3);
	});

	it('keeps loudness level through the overlap (equal power)', () => {
		const { loop, gain } = setup(60);
		loop.start();
		const out = gain(0).gain.ofType('curve')[0].args[0] as Float32Array;
		// The matching fade-in curve is the mirror of the fade-out.
		const second = setup(60);
		second.loop.start();
		second.fire();
		const into = second.gain(1).gain.ofType('curve')[0].args[0] as Float32Array;
		for (let i = 0; i < out.length; i++) {
			expect(into[i] ** 2 + out[i] ** 2).toBeCloseTo(1, 6);
		}
		expect(into[0]).toBeCloseTo(0, 6);
		expect(into[into.length - 1]).toBeCloseTo(1, 6);
	});

	it('shortens the overlap for a short recording', () => {
		const { loop, fire, source, gain } = setup(9);
		expect(loop.overlap).toBe(3);
		loop.start();
		fire();
		expect(source(1).started[0]).toBeCloseTo(0.05 + 6, 6);
		expect(gain(1).gain.ofType('curve')[0].args[2]).toBe(3);
	});

	it('starts late rather than in the past if the timer ran late', () => {
		const { loop, context, timers, source } = setup(60);
		loop.start();
		context.currentTime = 70; // the timer was badly delayed
		timers[0].callback();
		expect(source(1).started[0]).toBe(70);
	});

	it('stops cleanly: fades out, ends its sources, and schedules nothing more', () => {
		const { loop, timers, source, gain, context } = setup(60);
		loop.start();
		context.currentTime = 10;
		loop.stop();

		expect(timers[0].cleared).toBe(true);
		expect(source(0).stopped.length).toBeGreaterThan(1); // the scheduled end plus the early one
		expect(gain(0).gain.ofType('cancel')).toHaveLength(1);
		timers[0].callback(); // even if the timer had already fired
		expect(context.sources).toHaveLength(1);
	});

	it('forgets passes once they end', () => {
		const { loop, source } = setup(60);
		loop.start();
		source(0).onended?.();
		expect(() => loop.stop()).not.toThrow();
	});
});
