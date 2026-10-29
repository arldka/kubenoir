/**
 * Loops one long recording forever by crossfading its end into its beginning, so there is no
 * audible seam (a plain `loop = true` cuts from the last sample straight to the first, which
 * clicks or jumps in anything that is not perfectly steady).
 *
 * The buffer is played again before it ends: a new copy starts `crossfade` seconds early while
 * the old one fades out under it, with equal-power curves so the loudness stays level. Starts are
 * scheduled on the audio clock (sample accurate, no drift) a little ahead of time from a timer.
 */

export interface CrossfadeLoopOptions {
	/** Seconds of overlap between one pass and the next. */
	crossfade: number;
	/** Seconds the very first pass takes to fade in. */
	fadeIn: number;
	/** How far ahead of a boundary (seconds) the next pass is scheduled. */
	lookahead: number;
	setTimer?: (callback: () => void, ms: number) => unknown;
	clearTimer?: (id: unknown) => void;
}

const CURVE_POINTS = 64;
const FADE_IN_CURVE = Float32Array.from({ length: CURVE_POINTS }, (_, i) => Math.sin((i / (CURVE_POINTS - 1)) * (Math.PI / 2)));
const FADE_OUT_CURVE = Float32Array.from({ length: CURVE_POINTS }, (_, i) => Math.cos((i / (CURVE_POINTS - 1)) * (Math.PI / 2)));
/** Start slightly in the future so the first pass is scheduled, not already late. */
const START_DELAY = 0.05;

interface Pass {
	source: AudioBufferSourceNode;
	gain: GainNode;
}

export class CrossfadeLoop {
	private timer: unknown;
	private passes: Pass[] = [];
	private stopped = false;

	constructor(
		private readonly context: AudioContext,
		private readonly buffer: AudioBuffer,
		private readonly destination: AudioNode,
		private readonly options: CrossfadeLoopOptions
	) {}

	/** The overlap actually used: never more than a third of the recording. */
	get overlap(): number {
		return Math.min(this.options.crossfade, this.buffer.duration / 3);
	}

	start(): void {
		this.schedulePass(this.context.currentTime + START_DELAY, true);
	}

	private schedulePass(at: number, first: boolean): void {
		if (this.stopped) return;
		const { context, buffer, options } = this;
		const duration = buffer.duration;
		const overlap = this.overlap;

		const source = context.createBufferSource();
		source.buffer = buffer;
		const gain = context.createGain();
		source.connect(gain);
		gain.connect(this.destination);

		if (first) {
			gain.gain.setValueAtTime(0, at);
			gain.gain.setTargetAtTime(1, at, options.fadeIn / 3);
		} else {
			gain.gain.setValueCurveAtTime(FADE_IN_CURVE, at, overlap);
		}
		gain.gain.setValueCurveAtTime(FADE_OUT_CURVE, at + duration - overlap, overlap);

		source.start(at);
		source.stop(at + duration + 0.1);
		const pass = { source, gain };
		this.passes.push(pass);
		source.onended = () => {
			this.passes = this.passes.filter((p) => p !== pass);
		};

		// The next pass begins `overlap` before this one ends, scheduled `lookahead` before that.
		const nextAt = at + duration - overlap;
		const delay = Math.max(0, (nextAt - context.currentTime - options.lookahead) * 1000);
		const setTimer = options.setTimer ?? ((callback, ms) => setTimeout(callback, ms));
		this.timer = setTimer(() => this.schedulePass(Math.max(nextAt, this.context.currentTime), false), delay);
	}

	/** Fade out and stop. */
	stop(): void {
		this.stopped = true;
		if (this.timer !== undefined) (this.options.clearTimer ?? ((id) => clearTimeout(id as number)))(this.timer);
		const now = this.context.currentTime;
		for (const { source, gain } of this.passes) {
			try {
				gain.gain.cancelScheduledValues(now);
				gain.gain.setTargetAtTime(0, now, 0.05);
				source.stop(now + 0.4);
			} catch {
				// already stopped
			}
		}
	}
}
