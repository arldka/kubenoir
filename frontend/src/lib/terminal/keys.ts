/** Keys the terminal understands, parsed from the raw bytes xterm.js hands us via `onData`. */
export type Key =
	| { type: 'char'; text: string }
	| { type: 'enter' }
	| { type: 'backspace' }
	| { type: 'delete' }
	| { type: 'left' }
	| { type: 'right' }
	| { type: 'up' }
	| { type: 'down' }
	| { type: 'wordLeft' }
	| { type: 'wordRight' }
	| { type: 'home' }
	| { type: 'end' }
	| { type: 'tab' }
	| { type: 'escape' }
	| { type: 'ctrl'; key: CtrlKey };

export type CtrlKey = 'a' | 'c' | 'd' | 'e' | 'k' | 'l' | 'r' | 'u' | 'w' | 'z';

const CTRL: Record<number, CtrlKey> = {
	0x01: 'a',
	0x03: 'c',
	0x04: 'd',
	0x05: 'e',
	0x0b: 'k',
	0x0c: 'l',
	0x12: 'r',
	0x15: 'u',
	0x17: 'w',
	0x1a: 'z'
};

/** Map a CSI final byte (plus its numeric parameters) to a key. */
function csiKey(final: string, params: number[]): Key | null {
	// Modifier parameter: 3 = Alt, 5 = Ctrl (e.g. ESC [ 1 ; 5 C).
	const modified = params.length > 1 && (params[1] === 3 || params[1] === 5);
	switch (final) {
		case 'A':
			return { type: 'up' };
		case 'B':
			return { type: 'down' };
		case 'C':
			return { type: modified ? 'wordRight' : 'right' };
		case 'D':
			return { type: modified ? 'wordLeft' : 'left' };
		case 'H':
			return { type: 'home' };
		case 'F':
			return { type: 'end' };
		case '~':
			switch (params[0]) {
				case 1:
				case 7:
					return { type: 'home' };
				case 4:
				case 8:
					return { type: 'end' };
				case 3:
					return { type: 'delete' };
			}
	}
	return null;
}

/**
 * Split raw terminal input into keys. A paste arrives as one string, so it naturally becomes
 * `char`, `enter`, `char`, ... Other control characters and unknown escape sequences are dropped.
 *
 * With `editor`, a lone Escape is a key of its own (the prompt has no use for it), and Esc
 * followed by `b` or `f` is Escape then the letter, not Alt+b / Alt+f.
 */
export function parseKeys(data: string, editor = false): Key[] {
	const keys: Key[] = [];
	let text = '';
	const flush = () => {
		if (text) keys.push({ type: 'char', text });
		text = '';
	};
	const push = (key: Key | null) => {
		flush();
		if (key) keys.push(key);
	};

	for (let i = 0; i < data.length; i++) {
		const c = data[i];
		const code = data.charCodeAt(i);

		if (c === '\x1b') {
			const next = data[i + 1];
			if (next === '[') {
				// CSI: parameters (digits and ;) then one final byte.
				let j = i + 2;
				let raw = '';
				while (j < data.length && /[0-9;]/.test(data[j])) raw += data[j++];
				const final = data[j];
				push(final ? csiKey(final, raw.split(';').map((p) => parseInt(p, 10))) : null);
				i = j;
			} else if (next === 'O') {
				const final = data[i + 2];
				push(final ? csiKey(final, []) : null);
				i += 2;
			} else if (editor) {
				push({ type: 'escape' });
			} else if (next === 'b') {
				push({ type: 'wordLeft' });
				i += 1;
			} else if (next === 'f') {
				push({ type: 'wordRight' });
				i += 1;
			} else {
				push(null);
				if (next !== undefined) i += 1;
			}
		} else if (c === '\r') {
			push({ type: 'enter' });
			if (data[i + 1] === '\n') i += 1;
		} else if (c === '\n') {
			push({ type: 'enter' });
		} else if (code === 0x7f || code === 0x08) {
			push({ type: 'backspace' });
		} else if (c === '\t') {
			push({ type: 'tab' });
		} else if (code in CTRL) {
			push({ type: 'ctrl', key: CTRL[code] });
		} else if (code < 0x20) {
			push(null);
		} else {
			text += c;
		}
	}
	flush();
	return keys;
}
