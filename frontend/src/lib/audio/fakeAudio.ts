/** Test support: a recording stand-in for the Web Audio API. */

export class FakeParam {
	value: number;
	events: { type: string; args: unknown[] }[] = [];
	constructor(value = 1) {
		this.value = value;
	}
	setValueAtTime(value: number, time: number) {
		this.events.push({ type: 'set', args: [value, time] });
		return this;
	}
	setTargetAtTime(target: number, time: number, constant: number) {
		this.events.push({ type: 'target', args: [target, time, constant] });
		return this;
	}
	setValueCurveAtTime(curve: Float32Array, time: number, duration: number) {
		this.events.push({ type: 'curve', args: [curve, time, duration] });
		return this;
	}
	cancelScheduledValues(time: number) {
		this.events.push({ type: 'cancel', args: [time] });
		return this;
	}
	ofType(type: string) {
		return this.events.filter((e) => e.type === type);
	}
}

export class FakeNode {
	connected: unknown[] = [];
	connect<T>(node: T): T {
		this.connected.push(node);
		return node;
	}
}

export class FakeGain extends FakeNode {
	gain = new FakeParam(1);
}

export class FakeBuffer {
	constructor(
		public duration: number,
		public sampleRate = 48000,
		private data = new Float32Array(Math.max(1, Math.floor(duration * 48000)))
	) {}
	get length() {
		return this.data.length;
	}
	getChannelData() {
		return this.data;
	}
}

export class FakeSource extends FakeNode {
	buffer: FakeBuffer | null = null;
	loop = false;
	playbackRate = new FakeParam(1);
	started: (number | undefined)[] = [];
	stopped: (number | undefined)[] = [];
	onended: (() => void) | null = null;
	start(when?: number) {
		this.started.push(when);
	}
	stop(when?: number) {
		this.stopped.push(when);
	}
}

export class FakeContext {
	sampleRate = 48000;
	currentTime = 0;
	state: 'running' | 'suspended' | 'closed' = 'running';
	destination = new FakeNode();
	gains: FakeGain[] = [];
	sources: FakeSource[] = [];
	compressors: FakeNode[] = [];
	buffersCreated = 0;
	resumeCalls = 0;
	suspendCalls = 0;
	closed = false;

	createGain() {
		const gain = new FakeGain();
		this.gains.push(gain);
		return gain as unknown as GainNode;
	}
	createBufferSource() {
		const source = new FakeSource();
		this.sources.push(source);
		return source as unknown as AudioBufferSourceNode;
	}
	createDynamicsCompressor() {
		const node = new FakeNode();
		this.compressors.push(node);
		return node as unknown as DynamicsCompressorNode;
	}
	createBuffer(_channels: number, length: number, rate: number) {
		this.buffersCreated++;
		return new FakeBuffer(length / rate, rate) as unknown as AudioBuffer;
	}
	/** The "file" is `n` bytes for an `n / 1000` second recording. */
	async decodeAudioData(bytes: ArrayBuffer) {
		return new FakeBuffer(bytes.byteLength / 1000) as unknown as AudioBuffer;
	}
	async resume() {
		this.resumeCalls++;
		this.state = 'running';
	}
	async suspend() {
		this.suspendCalls++;
		this.state = 'suspended';
	}
	async close() {
		this.closed = true;
		this.state = 'closed';
	}
	asContext() {
		return this as unknown as AudioContext;
	}
}

/** A deterministic "random" that cycles through the given values. */
export function sequence(...values: number[]): () => number {
	let i = 0;
	return () => values[i++ % values.length];
}
