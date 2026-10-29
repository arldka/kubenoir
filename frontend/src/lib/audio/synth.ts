/**
 * A stand-in key sound, synthesized, so typing is audible (and testable) before real recordings
 * are added: a short filtered noise click over a low thump.
 */
export function createClickBuffer(context: BaseAudioContext): AudioBuffer {
	const rate = context.sampleRate;
	const length = Math.floor(rate * 0.06);
	const buffer = context.createBuffer(1, length, rate);
	const data = buffer.getChannelData(0);

	let smooth = 0; // one-pole low-pass takes the harsh edge off the noise
	for (let i = 0; i < length; i++) {
		const t = i / rate;
		smooth += 0.35 * (Math.random() * 2 - 1 - smooth);
		const click = smooth * Math.exp(-t / 0.004);
		const thump = Math.sin(2 * Math.PI * 150 * t) * Math.exp(-t / 0.012) * 0.5;
		data[i] = (click + thump) * 0.8;
	}
	return buffer;
}
