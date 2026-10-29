import { describe, expect, it } from 'vitest';

import { parseKeys } from './keys';
import { ROOKIE_MESSAGE, UNSAVED_MESSAGE, Vim, type VimResult } from './vim';

const ESC = '\x1b';
const CTRL_C = '\x03';
const CTRL_Z = '\x1a';
const CTRL_R = '\x12';

/** An editor on `text`, in an 80x10 terminal. */
function editor(text: string, isNew = false) {
	const vim = new Vim(text, { name: 'pod.yaml', rows: 10, cols: 80, isNew });
	const results: VimResult[] = [];
	return {
		vim,
		results,
		/** Type keys; `\x1b` is Escape, `\r` Enter. */
		type(keys: string) {
			for (const key of parseKeys(keys, true)) results.push(vim.handle(key));
			return this;
		},
		get lines() {
			return vim.text === '' ? [''] : vim.text.replace(/\n$/, '').split('\n');
		},
		get status() {
			return vim.screen().status;
		},
		get cursor() {
			const { row, col } = vim.screen().cursor;
			return [row, col];
		}
	};
}

describe('opening', () => {
	it('shows the file and its size, or that it is new', () => {
		expect(editor('a\nb\n').status).toMatch(/^"pod.yaml" 2L, 4B/);
		expect(editor('', true).status).toMatch(/^"pod.yaml" \[New\]/);
	});

	it('lays text out with ~ below it and the name, flag and position on the status line', () => {
		const { rows, status } = editor('one\ntwo\n').vim.screen();
		expect(rows.slice(0, 4)).toEqual(['one', 'two', '~', '~']);
		expect(rows).toHaveLength(9);
		expect(status.endsWith('pod.yaml  1,1')).toBe(true);
		expect(status).toHaveLength(80);
	});
});

describe('moving', () => {
	const text = 'alpha beta\n  gamma.delta\n\nlast line here\n';

	it('h j k l and the arrows, clamped to the text', () => {
		const e = editor(text);
		e.type('lll');
		expect(e.cursor).toEqual([0, 3]);
		e.type('j');
		expect(e.cursor).toEqual([1, 3]);
		e.type('jj');
		expect(e.cursor).toEqual([3, 3]);
		e.type('jjjj');
		expect(e.cursor).toEqual([3, 3]);
		e.type('kkk');
		// Up and down aim for the column the last sideways move left the cursor in.
		expect(e.cursor).toEqual([0, 3]);
		e.type('\x1b[C\x1b[B\x1b[D\x1b[A');
		expect(e.cursor).toEqual([0, 3]);
		e.type('hhh');
		expect(e.cursor).toEqual([0, 0]);
	});

	it('0 and $, and an empty line has a column zero only', () => {
		const e = editor(text);
		e.type('$');
		expect(e.cursor).toEqual([0, 9]);
		e.type('0');
		expect(e.cursor).toEqual([0, 0]);
		e.type('jj');
		expect(e.cursor).toEqual([2, 0]);
		e.type('$l');
		expect(e.cursor).toEqual([2, 0]);
	});

	it('w b e jump over words, punctuation and lines', () => {
		const e = editor(text);
		e.type('w');
		expect(e.cursor).toEqual([0, 6]);
		e.type('w');
		expect(e.cursor).toEqual([1, 2]);
		e.type('w');
		expect(e.cursor).toEqual([1, 7]);
		e.type('w');
		expect(e.cursor).toEqual([1, 8]);
		e.type('w');
		expect(e.cursor).toEqual([2, 0]);
		e.type('w');
		expect(e.cursor).toEqual([3, 0]);
		e.type('b');
		expect(e.cursor).toEqual([2, 0]);
		e.type('b');
		expect(e.cursor).toEqual([1, 8]);
		e.type('bbb');
		expect(e.cursor).toEqual([0, 6]);
		const f = editor(text);
		f.type('e');
		expect(f.cursor).toEqual([0, 4]);
		f.type('e');
		expect(f.cursor).toEqual([0, 9]);
		f.type('e');
		expect(f.cursor).toEqual([1, 6]);
	});

	it('gg, G and counts', () => {
		const e = editor(text);
		e.type('G');
		expect(e.cursor[0]).toBe(3);
		e.type('gg');
		expect(e.cursor[0]).toBe(0);
		e.type('3G');
		expect(e.cursor[0]).toBe(2);
		e.type('2gg');
		expect(e.cursor[0]).toBe(1);
		e.type('2j');
		expect(e.cursor[0]).toBe(3);
		e.type('10k');
		expect(e.cursor[0]).toBe(0);
		e.type('3l');
		// `2gg` put the cursor on the first word of "  gamma.delta"; the column stays for k.
		expect(e.cursor).toEqual([0, 5]);
		e.type('2w');
		expect(e.cursor).toEqual([1, 2]);
		e.type(':3\r');
		expect(e.cursor[0]).toBe(2);
	});

	it('scrolls to keep the cursor in view', () => {
		const e = editor(Array.from({ length: 30 }, (_, i) => `line ${i + 1}`).join('\n'));
		e.type('20G');
		const { rows, cursor } = e.vim.screen();
		expect(rows).toContain('line 20');
		expect(rows[cursor.row]).toBe('line 20');
		e.type('gg');
		expect(e.vim.screen().rows[0]).toBe('line 1');
	});
});

describe('inserting', () => {
	it('i a I A insert at the right place and Escape steps back', () => {
		const e = editor('  key: value\n');
		e.type('wiX' + ESC);
		expect(e.lines).toEqual(['  Xkey: value']);
		e.type('0aY' + ESC);
		expect(e.lines).toEqual([' Y Xkey: value']);
		e.type('Iz' + ESC);
		expect(e.lines).toEqual([' zY Xkey: value']);
		e.type('Aend' + ESC);
		expect(e.lines).toEqual([' zY Xkey: valueend']);
		expect(e.vim.currentMode).toBe('normal');
		expect(e.status).not.toContain('INSERT');
	});

	it('shows -- INSERT -- while inserting', () => {
		const e = editor('a\n').type('i');
		expect(e.status).toMatch(/^-- INSERT --/);
	});

	it('o and O open lines with the indent of the current one', () => {
		const e = editor('spec:\n  containers:\n    - name: a\n');
		e.type('jotail' + ESC);
		expect(e.lines).toEqual(['spec:', '  containers:', '  tail', '    - name: a']);
		e.type('Ohead' + ESC);
		expect(e.lines[2]).toBe('  head');
		expect(e.lines[3]).toBe('  tail');
	});

	it('Enter keeps the indent of the line it splits', () => {
		const e = editor('  a: 1\n');
		e.type('A\rb: 2' + ESC);
		expect(e.lines).toEqual(['  a: 1', '  b: 2']);
		e.type('0wi\r' + ESC);
		expect(e.lines).toEqual(['  a: 1', '  ', '  b: 2']);
	});

	it('Tab inserts two spaces, never a tab character', () => {
		const e = editor('a:\n');
		e.type('o\tb: 1\t\t' + ESC);
		expect(e.lines[1]).toBe('  b: 1    ');
		expect(e.vim.text).not.toContain('\t');
	});

	it('Backspace deletes and joins lines; Delete does too', () => {
		const e = editor('ab\ncd\n');
		e.type('jI\x7f' + ESC);
		expect(e.lines).toEqual(['abcd']);
		e.type('0i\x1b[3~' + ESC);
		expect(e.lines).toEqual(['bcd']);
	});

	it('typing a pasted run inserts it whole, in order', () => {
		const e = editor('\n');
		e.type('i' + 'apiVersion: v1\rkind: Pod\r  name: x' + ESC);
		expect(e.lines).toEqual(['apiVersion: v1', 'kind: Pod', '  name: x']);
	});
});

describe('deleting, yanking and putting', () => {
	it('x deletes characters, with a count', () => {
		const e = editor('abcdef\n');
		e.type('x');
		expect(e.lines).toEqual(['bcdef']);
		e.type('3x');
		expect(e.lines).toEqual(['ef']);
		e.type('5x');
		expect(e.lines).toEqual(['']);
	});

	it('dd deletes lines (counts too), yy yanks, p and P put them back', () => {
		const e = editor('1\n2\n3\n4\n5\n');
		e.type('dd');
		expect(e.lines).toEqual(['2', '3', '4', '5']);
		e.type('p');
		expect(e.lines).toEqual(['2', '1', '3', '4', '5']);
		expect(e.cursor[0]).toBe(1);
		e.type('2dd');
		expect(e.lines).toEqual(['2', '4', '5']);
		e.type('P');
		expect(e.lines).toEqual(['2', '1', '3', '4', '5']);
		e.type('Gyyggp');
		expect(e.lines).toEqual(['2', '5', '1', '3', '4', '5']);
		e.type('2yyGp');
		expect(e.lines).toEqual(['2', '5', '1', '3', '4', '5', '5', '1']);
	});

	it('deleting the last line leaves one empty line, and p/P work on characters', () => {
		const e = editor('only\n');
		e.type('dd');
		expect(e.lines).toEqual(['']);
		const f = editor('abc\n');
		f.type('xp');
		expect(f.lines).toEqual(['bac']);
		f.type('P');
		expect(f.lines).toEqual(['baac']);
	});

	it('p with nothing yanked says so', () => {
		expect(editor('a\n').type('p').status).toMatch(/^E353/);
	});
});

describe('undo and redo', () => {
	it('u undoes one change at a time and Ctrl-R redoes it', () => {
		const e = editor('a\nb\nc\n');
		e.type('ddx');
		expect(e.lines).toEqual(['', 'c']);
		e.type('u');
		expect(e.lines).toEqual(['b', 'c']);
		e.type('u');
		expect(e.lines).toEqual(['a', 'b', 'c']);
		e.type('u');
		expect(e.status).toMatch(/^Already at oldest change/);
		e.type(CTRL_R);
		expect(e.lines).toEqual(['b', 'c']);
		e.type(CTRL_R + CTRL_R);
		expect(e.status).toMatch(/^Already at newest change/);
	});

	it('a whole insert session is one undo step', () => {
		const e = editor('a\n');
		e.type('ihello\rworld' + ESC);
		expect(e.lines).toEqual(['hello', 'worlda']);
		e.type('u');
		expect(e.lines).toEqual(['a']);
	});

	it('a new change drops the redo history', () => {
		const e = editor('a\nb\n');
		e.type('ddu');
		e.type('x');
		e.type(CTRL_R);
		expect(e.status).toMatch(/^Already at newest change/);
	});
});

describe('saving (V24)', () => {
	it(':w hands out the buffer as lines joined by newlines plus a final newline', () => {
		const e = editor('a: 1\nb: 2');
		e.type('Ac' + ESC);
		e.type(':w\r');
		const saved = e.results.at(-1)!;
		expect(saved.save).toBe('a: 1c\nb: 2\n');
		expect(saved.quit).toBeUndefined();
		expect(e.status).toMatch(/^"pod.yaml" 2L, 11B written/);
		expect(e.vim.modified).toBe(false);
	});

	it(':wq and :x save and quit', () => {
		for (const command of [':wq\r', ':x\r', ':wq!\r']) {
			const e = editor('a\n');
			e.type('Ab' + ESC).type(command);
			const result = e.results.at(-1)!;
			expect(result.save, command).toBe('ab\n');
			expect(result.quit, command).toBe(true);
		}
	});

	it('an empty buffer is written as an empty file', () => {
		const e = editor('x\n');
		e.type('dd:w\r');
		expect(e.results.at(-1)!.save).toBe('');
	});

	it('nothing is saved without a write', () => {
		const e = editor('a\n');
		e.type('Ab' + ESC);
		expect(e.results.every((r) => r.save === undefined)).toBe(true);
	});
});

describe('leaving (V25)', () => {
	it(':q leaves when nothing changed', () => {
		const e = editor('a\n').type(':q\r');
		expect(e.results.at(-1)!.quit).toBe(true);
	});

	it(':q with unsaved changes is refused, :q! discards them', () => {
		const e = editor('a\n');
		e.type('Ab' + ESC);
		e.type(':q\r');
		expect(e.results.at(-1)!.quit).toBeUndefined();
		expect(e.status).toContain(UNSAVED_MESSAGE);
		e.type(':q!\r');
		expect(e.results.at(-1)!.quit).toBe(true);
		expect(e.results.at(-1)!.save).toBeUndefined();
	});

	it('shows [+] while the buffer differs from the file, and not after writing or undoing', () => {
		const e = editor('a\n');
		expect(e.status).not.toContain('[+]');
		e.type('Ab' + ESC);
		expect(e.status).toContain('pod.yaml [+]');
		e.type('u');
		expect(e.status).not.toContain('[+]');
		e.type('x:w\r');
		expect(e.status).not.toContain('[+]');
	});
});

describe('the boss on failed exits (V29)', () => {
	it('Ctrl-C and Ctrl-Z do not leave; the third failed attempt of any kind gets the scolding', () => {
		const e = editor('a\n');
		e.type('Ab' + ESC);
		e.type(':q\r');
		expect(e.vim.failedExitAttempts).toBe(1);
		expect(e.status).toContain(UNSAVED_MESSAGE);
		e.type(CTRL_C);
		expect(e.vim.failedExitAttempts).toBe(2);
		expect(e.status).toContain('Type  :qa!  and press <Enter>');
		expect(e.status).not.toContain(ROOKIE_MESSAGE);
		e.type(CTRL_Z);
		expect(e.vim.failedExitAttempts).toBe(3);
		expect(e.status).toContain(ROOKIE_MESSAGE);
		expect(e.results.every((r) => !r.quit)).toBe(true);
		// Every further attempt is scolded too, and the editor stays open.
		e.type(':q\r');
		expect(e.status).toContain(ROOKIE_MESSAGE);
		expect(e.status).not.toContain(UNSAVED_MESSAGE);
		expect(e.results.at(-1)!.quit).toBeUndefined();
	});

	it('the scolding replaces the normal message of that attempt', () => {
		const e = editor('a\n');
		e.type('Ab' + ESC + ':q\r' + CTRL_C + ':q\r');
		expect(e.status).toContain(ROOKIE_MESSAGE);
		expect(e.status).not.toContain('E37');
	});

	it(':q!, :wq and :x still leave after the scolding', () => {
		for (const way of [':q!\r', ':wq\r', ':x\r']) {
			const e = editor('a\n');
			e.type('Ab' + ESC + CTRL_C + CTRL_C + CTRL_C);
			expect(e.status).toContain(ROOKIE_MESSAGE);
			e.type(way);
			expect(e.results.at(-1)!.quit, way).toBe(true);
		}
	});

	it('Ctrl-C in insert mode leaves insert mode but still counts', () => {
		const e = editor('a\n').type('ix');
		e.type(CTRL_C);
		expect(e.vim.currentMode).toBe('normal');
		expect(e.vim.failedExitAttempts).toBe(1);
	});

	it('a clean :q is no failed attempt', () => {
		const e = editor('a\n').type(':q\r');
		expect(e.vim.failedExitAttempts).toBe(0);
	});
});

describe('what it does not do (V27)', () => {
	it('names an unsupported ex command', () => {
		const e = editor('a\n').type(':foo\r');
		expect(e.status).toMatch(/^E492: Not an editor command: foo/);
		const f = editor('a\n').type(':%s/a/b/\r');
		expect(f.status).toMatch(/^E492: Not an editor command: %s\/a\/b\//);
		expect(f.lines).toEqual(['a']);
	});

	it('names an unsupported normal-mode key and changes nothing', () => {
		for (const key of ['v', 'r', '/', '.', 'J', 'D', 'c']) {
			const e = editor('abc\n').type(key);
			expect(e.status, key).toMatch(new RegExp(`^E492: Not an editor command: ${key.replace(/[./]/g, '\\$&')}`));
			expect(e.lines).toEqual(['abc']);
			expect(e.vim.modified).toBe(false);
		}
	});

	it('names an unsupported operator pair and does not half-apply it', () => {
		const e = editor('abc\ndef\n').type('dw');
		expect(e.status).toMatch(/^E492: Not an editor command: dw/);
		expect(e.lines).toEqual(['abc', 'def']);
		const f = editor('abc\n').type('gx');
		expect(f.status).toMatch(/^E492: Not an editor command: gx/);
	});

	it('Escape clears a half-typed command and the ex line', () => {
		const e = editor('a\nb\nc\n').type('2d' + ESC + 'j');
		expect(e.cursor[0]).toBe(1);
		const f = editor('a\n').type(':q' + ESC);
		expect(f.vim.currentMode).toBe('normal');
		expect(f.results.every((r) => !r.quit)).toBe(true);
	});

	it('shows the ex line being typed', () => {
		const e = editor('a\n').type(':wq');
		expect(e.status).toMatch(/^:wq\s/);
		e.type('\x7f');
		expect(e.status).toMatch(/^:w\s/);
	});
});
