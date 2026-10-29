import { CrossfadeLoop } from './loop';
import type { Manifest } from './manifest';
import { DEFAULT_SETTINGS, gainFor, type SoundSettings } from './settings';
import { createClickBuffer } from './synth';
import { pickIndex, type KeySoundKind } from './typing';

/** Fixed mix of the two layers before the master volume (0 to 1 each). */
const LEVELS = { ambience: 0.8, typing: 0.6 } as const;
type Channel = keyof typeof LEVELS;

/** Seconds the end of the soundtrack overlaps with its beginning when it loops. */
export const CROSSFADE_SECONDS = 4;
/** Seconds the soundtrack takes to fade in when sound starts. */
const FADE_IN_SECONDS = 2;
/** The next loop pass is scheduled this many seconds before it is needed. */
const LOOP_LOOKAHEAD_SECONDS = 3;
/** The master gain eases to a new value instead of jumping, which would click. */
const GAIN_SMOOTHING_SECONDS = 0.05;
/** Typing voices that may sound at once; the oldest is cut off beyond this. */
export const MAX_VOICES = 8;
const RATE_JITTER = 0.04;

export interface SoundscapeDeps {
	manifest: Manifest;
	createContext?: () => AudioContext;
	fetchBytes?: (url: string) => Promise<ArrayBuffer>;
	random?: () => number;
	setTimer?: (callback: () => void, ms: number) => unknown;
	clearTimer?: (id: unknown) => void;
}

async function fetchBytes(url: string): Promise<ArrayBuffer> {
	const response = await fetch(url);
	if (!response.ok) throw new Error(`HTTP ${response.status}`);
	return response.arrayBuffer();
}

/**
 * The game's sound: one long ambience track (music, vinyl crackle, rain, breathing and smoking
 * are all mixed into it) looped with a crossfade, and a typewriter sound per keystroke.
 *
 * Browsers only allow audio after a user gesture, so nothing exists until `start()` is called
 * from one. Every file is optional: a missing or undecodable one is skipped with a warning, and
 * typing falls back to a synthesized click so the keyboard is never silent by accident.
 */
export class Soundscape {
	private context?: AudioContext;
	private master?: GainNode;
	private readonly channels = new Map<Channel, GainNode>();
	private settings: SoundSettings = { ...DEFAULT_SETTINGS };
	private starting?: Promise<void>;
	private disposed = false;

	private keys: AudioBuffer[] = [];
	private special: Partial<Record<'space' | 'return' | 'backspace', AudioBuffer>> = {};
	private fallback?: AudioBuffer;
	private lastKey: number | null = null;
	private loop?: CrossfadeLoop;
	private voices: AudioBufferSourceNode[] = [];

	constructor(private readonly deps: SoundscapeDeps) {}

	private random(): number {
		return (this.deps.random ?? Math.random)();
	}

	/** Whether the audio graph exists, so keystrokes can be heard. */
	get started(): boolean {
		return this.context !== undefined;
	}

	setSettings(settings: SoundSettings): void {
		this.settings = settings;
		this.applyMaster(false);
	}

	/** Create the audio graph and begin playing. Call from a click or key press. Idempotent. */
	start(): Promise<void> {
		this.starting ??= this.begin();
		return this.starting;
	}

	private async begin(): Promise<void> {
		let context: AudioContext;
		try {
			context = this.deps.createContext ? this.deps.createContext() : new AudioContext();
		} catch (error) {
			console.warn('Sound is unavailable in this browser', error);
			return;
		}
		if (this.disposed) {
			void context.close();
			return;
		}
		this.context = context;

		const master = context.createGain();
		const compressor = context.createDynamicsCompressor(); // keeps the layers from clipping together
		master.connect(compressor);
		compressor.connect(context.destination);
		this.master = master;
		for (const channel of Object.keys(LEVELS) as Channel[]) {
			const gain = context.createGain();
			gain.gain.value = LEVELS[channel];
			gain.connect(master);
			this.channels.set(channel, gain);
		}
		this.applyMaster(true);
		this.fallback = createClickBuffer(context);
		void context.resume().catch(() => {});

		// Typing already works at this point; the recordings arrive as they finish decoding.
		await this.loadAssets(context);
	}

	private async decode(context: AudioContext, url: string | undefined): Promise<AudioBuffer | undefined> {
		if (!url) return undefined;
		try {
			const bytes = await (this.deps.fetchBytes ?? fetchBytes)(url);
			return await context.decodeAudioData(bytes);
		} catch (error) {
			console.warn(`Could not load sound ${url}`, error);
			return undefined;
		}
	}

	private async loadAssets(context: AudioContext): Promise<void> {
		const { manifest } = this.deps;
		const [ambience, keys, space, ret, backspace] = await Promise.all([
			this.decode(context, manifest.ambience),
			Promise.all(manifest.keys.map((url) => this.decode(context, url))),
			this.decode(context, manifest.space),
			this.decode(context, manifest.return),
			this.decode(context, manifest.backspace)
		]);
		if (this.disposed) return;

		this.keys = keys.filter((b): b is AudioBuffer => !!b);
		this.special = { space, return: ret, backspace };

		const channel = this.channels.get('ambience');
		if (ambience && channel) {
			this.loop = new CrossfadeLoop(context, ambience, channel, {
				crossfade: CROSSFADE_SECONDS,
				fadeIn: FADE_IN_SECONDS,
				lookahead: LOOP_LOOKAHEAD_SECONDS,
				setTimer: this.deps.setTimer,
				clearTimer: this.deps.clearTimer
			});
			this.loop.start();
		}
	}

	private applyMaster(immediately: boolean): void {
		if (!this.master || !this.context) return;
		const gain = gainFor(this.settings);
		if (immediately) this.master.gain.value = gain;
		else this.master.gain.setTargetAtTime(gain, this.context.currentTime, GAIN_SMOOTHING_SECONDS);
	}

	/** Play the typewriter sound for a keystroke. */
	keystroke(kind: KeySoundKind): void {
		const context = this.context;
		const typing = this.channels.get('typing');
		if (!context || !typing || this.disposed || this.settings.muted || context.state === 'suspended') return;

		let buffer: AudioBuffer | undefined;
		let rate = 1;
		let gain = 1;
		if (kind === 'char' && this.keys.length > 0) {
			const index = pickIndex(this.keys.length, this.lastKey, () => this.random());
			this.lastKey = index;
			buffer = this.keys[index];
		} else if (kind === 'space') {
			buffer = this.special.space;
			if (!buffer) [buffer, rate, gain] = [this.fallback, 0.7, 1];
		} else if (kind === 'return') {
			buffer = this.special.return;
			if (!buffer) [buffer, rate, gain] = [this.fallback, 0.5, 1.3];
		} else if (kind === 'backspace') {
			buffer = this.special.backspace;
			if (!buffer) [buffer, rate, gain] = [this.fallback, 1.25, 0.7];
		} else {
			buffer = this.fallback;
		}
		if (!buffer) return;

		const source = context.createBufferSource();
		source.buffer = buffer;
		source.playbackRate.value = rate * (1 + (this.random() * 2 - 1) * RATE_JITTER);
		const level = context.createGain();
		level.gain.value = gain * (0.85 + this.random() * 0.15);
		source.connect(level);
		level.connect(typing);

		this.voices.push(source);
		source.onended = () => {
			this.voices = this.voices.filter((v) => v !== source);
		};
		while (this.voices.length > MAX_VOICES) {
			try {
				this.voices.shift()?.stop();
			} catch {
				// already finished
			}
		}
		source.start();
	}

	/** Pause everything (the tab is hidden). */
	suspend(): void {
		void this.context?.suspend().catch(() => {});
	}

	resume(): void {
		void this.context?.resume().catch(() => {});
	}

	dispose(): void {
		this.disposed = true;
		this.loop?.stop();
		for (const source of this.voices) {
			try {
				source.stop();
			} catch {
				// not started or already stopped
			}
		}
		void this.context?.close().catch(() => {});
		this.context = undefined;
	}
}
