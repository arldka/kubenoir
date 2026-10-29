import { describe, expect, it } from 'vitest';

import { parseKeys } from './keys';

describe('parseKeys', () => {
	it('groups printable text into one key', () => {
		expect(parseKeys('kubectl get')).toEqual([{ type: 'char', text: 'kubectl get' }]);
	});

	it('recognises editing and movement keys', () => {
		expect(parseKeys('\r\x7f\x1b[3~\t')).toEqual([
			{ type: 'enter' },
			{ type: 'backspace' },
			{ type: 'delete' },
			{ type: 'tab' }
		]);
		expect(parseKeys('\x1b[A\x1b[B\x1b[C\x1b[D')).toEqual([
			{ type: 'up' },
			{ type: 'down' },
			{ type: 'right' },
			{ type: 'left' }
		]);
	});

	it('accepts the common Home/End encodings', () => {
		for (const home of ['\x1b[H', '\x1bOH', '\x1b[1~', '\x1b[7~']) expect(parseKeys(home)).toEqual([{ type: 'home' }]);
		for (const end of ['\x1b[F', '\x1bOF', '\x1b[4~', '\x1b[8~']) expect(parseKeys(end)).toEqual([{ type: 'end' }]);
	});

	it('maps Ctrl/Alt arrows and Alt+b/f to word movement', () => {
		expect(parseKeys('\x1b[1;5D\x1b[1;5C\x1b[1;3D\x1b[1;3C\x1bb\x1bf')).toEqual([
			{ type: 'wordLeft' },
			{ type: 'wordRight' },
			{ type: 'wordLeft' },
			{ type: 'wordRight' },
			{ type: 'wordLeft' },
			{ type: 'wordRight' }
		]);
	});

	it('maps control characters', () => {
		expect(parseKeys('\x01\x03\x04\x05\x0b\x0c\x15\x17')).toEqual(
			['a', 'c', 'd', 'e', 'k', 'l', 'u', 'w'].map((key) => ({ type: 'ctrl', key }))
		);
	});

	it('splits a multi-line paste into text and enters', () => {
		expect(parseKeys('one\r\ntwo\nthree')).toEqual([
			{ type: 'char', text: 'one' },
			{ type: 'enter' },
			{ type: 'char', text: 'two' },
			{ type: 'enter' },
			{ type: 'char', text: 'three' }
		]);
	});

	it('drops unknown escape sequences and control characters', () => {
		expect(parseKeys('a\x1b[99Xb\x02c\x1b')).toEqual([
			{ type: 'char', text: 'a' },
			{ type: 'char', text: 'b' },
			{ type: 'char', text: 'c' }
		]);
	});

	it('keeps non-ASCII text intact', () => {
		expect(parseKeys('café')).toEqual([{ type: 'char', text: 'café' }]);
	});
});
