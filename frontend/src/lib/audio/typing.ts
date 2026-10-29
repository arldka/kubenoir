import type { Key } from '../terminal/keys';

export type KeySoundKind = 'char' | 'space' | 'return' | 'backspace';

/**
 * Which typewriter sound a key makes, or null for keys that stay silent (cursor movement and
 * most shortcuts). A paste arrives as one chunk of text and makes a single sound, not a rattle.
 */
export function soundForKey(key: Key): KeySoundKind | null {
	switch (key.type) {
		case 'char':
			return key.text === ' ' ? 'space' : 'char';
		case 'enter':
			return 'return';
		case 'backspace':
		case 'delete':
			return 'backspace';
		case 'tab':
			return 'space';
		case 'ctrl':
			return key.key === 'u' || key.key === 'k' || key.key === 'w' ? 'backspace' : null;
		default:
			return null;
	}
}

/** A random index into `count` choices that is never the same as `last` (when there is a choice). */
export function pickIndex(count: number, last: number | null, random: () => number = Math.random): number {
	if (count <= 1) return 0;
	if (last === null) return Math.floor(random() * count) % count;
	// Choose among the other `count - 1` indexes, then skip over `last`.
	const index = Math.floor(random() * (count - 1)) % (count - 1);
	return index >= last ? index + 1 : index;
}
