import { describe, expect, it } from 'vitest';

import { History } from './history';
import { parseKeys } from './keys';
import { LineEditor } from './lineEditor';

function editor(history: string[] = []) {
	const h = new History('t', null);
	history.forEach((l) => h.add(l));
	const e = new LineEditor(h);
	const press = (data: string) => parseKeys(data).map((k) => e.handle(k));
	return { e, press };
}

describe('LineEditor', () => {
	it('inserts text at the cursor and moves it along', () => {
		const { e, press } = editor();
		press('helo');
		press('\x1b[D\x1b[D'); // left x2
		press('l');
		expect([e.buffer, e.cursor]).toEqual(['hello', 3]);
	});

	it('deletes with backspace and delete, ringing at the edges', () => {
		const { e, press } = editor();
		expect(press('\x7f')).toEqual(['bell']);
		press('abc');
		press('\x1b[D');
		press('\x7f');
		expect([e.buffer, e.cursor]).toEqual(['ac', 1]);
		press('\x1b[3~');
		expect([e.buffer, e.cursor]).toEqual(['a', 1]);
		expect(press('\x1b[3~')).toEqual(['bell']);
	});

	it('supports the usual readline shortcuts', () => {
		const { e, press } = editor();
		press('get pods -n kube');
		press('\x17'); // Ctrl+W removes "kube"
		expect(e.buffer).toBe('get pods -n ');
		press('\x01'); // Ctrl+A
		expect(e.cursor).toBe(0);
		press('\x0b'); // Ctrl+K
		expect(e.buffer).toBe('');
		press('one two');
		press('\x01\x1b[1;5C'); // start, then a word right
		expect(e.cursor).toBe(3);
		press('\x0b');
		expect(e.buffer).toBe('one');
		press('\x15'); // Ctrl+U
		expect(e.buffer).toBe('');
		press('abc');
		press('\x05'); // Ctrl+E
		expect(e.cursor).toBe(3);
	});

	it('moves by words with Ctrl+arrows', () => {
		const { e, press } = editor();
		press('alpha beta  gamma');
		press('\x1b[1;5D');
		expect(e.cursor).toBe(12);
		press('\x1b[1;5D');
		expect(e.cursor).toBe(6);
		press('\x1b[1;5C');
		expect(e.cursor).toBe(10);
	});

	it('Ctrl+D deletes under the cursor, and does nothing on an empty line', () => {
		const { e, press } = editor();
		expect(press('\x04')).toEqual(['bell']);
		press('ab\x01\x04');
		expect(e.buffer).toBe('b');
	});

	it('reports enter, interrupt and clear as actions', () => {
		const { press } = editor();
		press('get pods');
		expect(press('\r')).toEqual([{ submit: 'get pods' }]);
		expect(press('\x03')).toEqual(['interrupt']);
		expect(press('\x0c')).toEqual(['clear']);
		expect(press('\t')).toEqual(['complete']);
	});

	it('browses history and restores the draft', () => {
		const { e, press } = editor(['first', 'second']);
		press('draft');
		press('\x1b[A');
		expect(e.buffer).toBe('second');
		press('\x1b[A');
		expect(e.buffer).toBe('first');
		expect(press('\x1b[A')).toEqual(['bell']);
		press('\x1b[B');
		expect(e.buffer).toBe('second');
		press('\x1b[B');
		expect(e.buffer).toBe('draft');
		expect(press('\x1b[B')).toEqual(['bell']);
	});

	it('edits a recalled entry in place and stops browsing', () => {
		const { e, press } = editor(['get pods']);
		press('\x1b[A');
		press('\x7f\x7f\x7f\x7f');
		press('nodes');
		expect(e.buffer).toBe('get nodes');
		expect(press('\x1b[B')).toEqual(['bell']);
	});

	it('rings when there is no history', () => {
		const { press } = editor();
		expect(press('\x1b[A')).toEqual(['bell']);
	});
});

describe('LineEditor completion', () => {
	function typed(line: string) {
		const { e, press } = editor();
		press(line);
		return e;
	}

	it('inserts a single candidate followed by a space', () => {
		const e = typed('kubectl ge');
		expect(e.applyCompletion({ start: 8, candidates: ['get'] })).toEqual({ kind: 'applied' });
		expect([e.buffer, e.cursor]).toEqual(['kubectl get ', 12]);
	});

	it('does not add a space after a slash or equals sign', () => {
		let e = typed('kubectl get pod/');
		e.applyCompletion({ start: 12, candidates: ['pod/'] });
		expect(e.buffer).toBe('kubectl get pod/');
		e = typed('kubectl get --output');
		e.applyCompletion({ start: 12, candidates: ['--output='] });
		expect(e.buffer).toBe('kubectl get --output=');
	});

	it('does not add a second space when one already follows', () => {
		const { e, press } = editor();
		press('kubectl ge pods');
		press('\x01' + '\x1b[C'.repeat(10)); // cursor right after "ge"
		e.applyCompletion({ start: 8, candidates: ['get'] });
		expect(e.buffer).toBe('kubectl get pods');
		expect(e.cursor).toBe(11);
	});

	it('extends several candidates to their common prefix, then lists them', () => {
		const e = typed('kubectl get pods p');
		const candidates = ['payments-api-1', 'payments-api-2', 'postgres-0'];
		// "p" is already the common prefix: nothing to add, so the choices are listed.
		expect(e.applyCompletion({ start: 17, candidates })).toEqual({ kind: 'list', items: candidates });
		expect(e.buffer).toBe('kubectl get pods p');

		const f = typed('kubectl get pods pay');
		expect(f.applyCompletion({ start: 17, candidates: ['payments-api-1', 'payments-api-2'] })).toEqual({ kind: 'applied' });
		expect(f.buffer).toBe('kubectl get pods payments-api-');
	});

	it('reports nothing to complete', () => {
		expect(typed('x').applyCompletion({ start: 0, candidates: [] })).toEqual({ kind: 'none' });
	});
});
