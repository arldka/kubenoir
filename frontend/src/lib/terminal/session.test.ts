import { describe, expect, it } from 'vitest';

import { createHarness, KEY } from './testHarness';

describe('TerminalSession editing', () => {
	it('shows the banner and prompt', async () => {
		const t = createHarness({ banner: 'CASE FILE: test\nType help.\n' });
		await t.start();
		expect(t.lines()).toEqual(['CASE FILE: test', 'Type help.', '$']);
		expect(t.cursor()).toEqual({ x: 2, y: 2 });
	});

	it('echoes typing and places the cursor', async () => {
		const t = createHarness();
		await t.start();
		await t.type('get pods');
		expect(t.lines()).toEqual(['$ get pods']);
		expect(t.cursor()).toEqual({ x: 10, y: 0 });
	});

	it('edits in the middle of the line', async () => {
		const t = createHarness();
		await t.start();
		await t.type('get pds' + KEY.left + KEY.left + 'o');
		expect(t.lines()).toEqual(['$ get pods']);
		expect(t.cursor()).toEqual({ x: 8, y: 0 });
		await t.type(KEY.backspace);
		expect(t.lines()).toEqual(['$ get pds']);
		await t.type(KEY.delete);
		expect(t.lines()).toEqual(['$ get ps']);
		await t.type(KEY.home + 'x');
		expect(t.lines()).toEqual(['$ xget ps']);
		expect(t.cursor()).toEqual({ x: 3, y: 0 });
		await t.type(KEY.end);
		expect(t.cursor()).toEqual({ x: 9, y: 0 });
	});

	it('supports the kill and move shortcuts', async () => {
		const t = createHarness();
		await t.start();
		await t.type('get pods -n kube');
		await t.type(KEY.ctrlW);
		expect(t.lines()).toEqual(['$ get pods -n']);
		await t.type(KEY.ctrlA + KEY.ctrlK);
		expect(t.lines()).toEqual(['$']);
		await t.type('one two three' + KEY.ctrlLeft + KEY.ctrlLeft + KEY.ctrlK);
		expect(t.lines()).toEqual(['$ one']);
		await t.type(KEY.ctrlU);
		expect(t.lines()).toEqual(['$']);
	});

	it('wraps long lines and keeps editing them correctly', async () => {
		const t = createHarness({ cols: 20 });
		await t.start();
		await t.type('abcdefghijklmnopqrstuvwxyz1234'); // 2 + 30 = 32 cells
		expect(t.lines()).toEqual(['$ abcdefghijklmnopqr', 'stuvwxyz1234']);
		expect(t.cursor()).toEqual({ x: 12, y: 1 });

		await t.type(KEY.home + 'X');
		expect(t.lines()).toEqual(['$ Xabcdefghijklmnopq', 'rstuvwxyz1234']);
		expect(t.cursor()).toEqual({ x: 3, y: 0 });

		await t.type(KEY.ctrlK);
		expect(t.lines()).toEqual(['$ X']);
		expect(t.cursor()).toEqual({ x: 3, y: 0 });
	});

	it('shrinks back to one row when text is deleted', async () => {
		const t = createHarness({ cols: 20 });
		await t.start();
		await t.type('a'.repeat(25));
		expect(t.lines()).toHaveLength(2);
		await t.type(KEY.backspace.repeat(20));
		expect(t.lines()).toEqual(['$ aaaaa']);
		expect(t.cursor()).toEqual({ x: 7, y: 0 });
	});

	it('handles a line that exactly fills the row', async () => {
		const t = createHarness({ cols: 20 });
		await t.start();
		await t.type('a'.repeat(18)); // 2 + 18 = 20 cells
		expect(t.lines()).toEqual(['$ ' + 'a'.repeat(18)]);
		expect(t.cursor()).toEqual({ x: 0, y: 1 });
		await t.type('z');
		expect(t.lines()).toEqual(['$ ' + 'a'.repeat(18), 'z']);
		expect(t.cursor()).toEqual({ x: 1, y: 1 });
		await t.type(KEY.backspace + KEY.backspace);
		expect(t.lines()).toEqual(['$ ' + 'a'.repeat(17)]);
		expect(t.cursor()).toEqual({ x: 19, y: 0 });
	});

	it('redraws a wrapped line correctly when the terminal gets narrower, too', async () => {
		const t = createHarness({ cols: 60 });
		await t.start();
		await t.type('get pods --output=wide --namespace=kube-system');
		await t.resize(24);
		// 2 + 46 = 48 cells: exactly two full rows, so the cursor waits on the row after them.
		expect(t.lines()).toEqual(['$ get pods --output=wide', ' --namespace=kube-system']);
		expect(t.cursor()).toEqual({ x: 0, y: 2 });
		await t.type('!');
		expect(t.lines()).toEqual(['$ get pods --output=wide', ' --namespace=kube-system', '!']);
	});

	it('redraws for a new width after a resize', async () => {
		const t = createHarness({ cols: 30 });
		await t.start();
		await t.type('get pods --output=wide --namespace=kube-system');
		expect(t.lines()).toHaveLength(2);
		await t.resize(60);
		expect(t.lines()).toEqual(['$ get pods --output=wide --namespace=kube-system']);
		expect(t.cursor()).toEqual({ x: 48, y: 0 });
	});
});

describe('TerminalSession commands', () => {
	it('runs a command and prints its output', async () => {
		const t = createHarness({ commands: { 'kubectl get pods': 'NAME   READY\nweb    1/1' } });
		await t.start();
		await t.type('kubectl get pods' + KEY.enter);
		expect(t.lines()).toEqual(['$ kubectl get pods', 'NAME   READY', 'web    1/1', '$']);
		expect(t.evaluated).toEqual(['kubectl get pods']);
		expect(t.cursor()).toEqual({ x: 2, y: 3 });
	});

	it('prints nothing for empty output and ignores blank lines', async () => {
		const t = createHarness({ commands: { quiet: '' } });
		await t.start();
		await t.type('quiet' + KEY.enter + '   ' + KEY.enter);
		expect(t.lines()).toEqual(['$ quiet', '$', '$']);
		expect(t.evaluated).toEqual(['quiet']);
	});

	it('reports engine exceptions as errors', async () => {
		const t = createHarness({
			commands: () => {
				throw new Error('boom');
			}
		});
		await t.start();
		await t.type('x' + KEY.enter);
		expect(t.lines()).toEqual(['$ x', 'error: boom', '$']);
	});

	it('keeps a long wrapped command intact when submitted from the middle', async () => {
		const t = createHarness({ cols: 20, commands: { [`${'a'.repeat(30)}`]: 'done' } });
		await t.start();
		await t.type('a'.repeat(30) + KEY.home + KEY.enter);
		expect(t.lines().slice(-2)).toEqual(['done', '$']);
		expect(t.evaluated).toEqual(['a'.repeat(30)]);
	});

	it('cancels the line with Ctrl+C', async () => {
		const t = createHarness();
		await t.start();
		await t.type('half typed' + KEY.ctrlC);
		expect(t.lines()).toEqual(['$ half typed', '^C', '$']);
		expect(t.evaluated).toEqual([]);
	});

	it('clears the screen with Ctrl+L but keeps the line being typed', async () => {
		const t = createHarness({ commands: { a: 'out' } });
		await t.start();
		await t.type('a' + KEY.enter + 'half' + KEY.ctrlL);
		expect(t.lines()).toEqual(['$ half']);
		expect(t.cursor()).toEqual({ x: 6, y: 0 });
	});

	it('handles the built-in help and clear commands without asking the engine', async () => {
		const t = createHarness();
		await t.start();
		await t.type('help' + KEY.enter);
		expect(t.lines().join('\n')).toContain('kubectl --help');
		await t.type('clear' + KEY.enter);
		expect(t.lines()).toEqual(['$']);
		expect(t.evaluated).toEqual([]);
	});

	it('runs every line of a multi-line paste in order', async () => {
		const t = createHarness({ commands: { one: '1', two: '2' } });
		await t.start();
		await t.type('one\ntwo\npartial');
		expect(t.lines()).toEqual(['$ one', '1', '$ two', '2', '$ partial']);
		expect(t.evaluated).toEqual(['one', 'two']);
	});

	it('recalls earlier commands with the arrow keys and persists them', async () => {
		const t = createHarness({ commands: { 'get pods': 'ok', 'get nodes': 'ok' } });
		await t.start();
		await t.type('get pods' + KEY.enter + 'get nodes' + KEY.enter);
		await t.type('typing' + KEY.up);
		expect(t.lines().slice(-1)).toEqual(['$ get nodes']);
		await t.type(KEY.up);
		expect(t.lines().slice(-1)).toEqual(['$ get pods']);
		await t.type(KEY.down + KEY.down);
		expect(t.lines().slice(-1)).toEqual(['$ typing']);
		expect(t.history.entries()).toEqual(['get pods', 'get nodes']);
	});

	it('processes input strictly in order while a command is still running', async () => {
		let release: () => void = () => {};
		const gate = new Promise<void>((resolve) => (release = resolve));
		const t = createHarness({ commands: async (c) => (await gate, `ran ${c}`) });
		await t.start();
		const first = t.type('slow' + KEY.enter);
		const second = t.type('next');
		release();
		await Promise.all([first, second]);
		expect(t.lines()).toEqual(['$ slow', 'ran slow', '$ next']);
	});
});

describe('TerminalSession tab completion', () => {
	const completions = (prefix: string) => {
		if (prefix === 'kubectl ge') return { start: 8, candidates: ['get'] };
		if (prefix === 'kubectl get pods pa') return { start: 17, candidates: ['payments-api-a', 'payments-api-b'] };
		if (prefix === 'kubectl get pods ') return { start: 17, candidates: ['payments-api-a', 'web-1', 'web-2'] };
		return { start: prefix.length, candidates: [] };
	};

	it('completes a single candidate and adds a space', async () => {
		const t = createHarness({ completions });
		await t.start();
		await t.type('kubectl ge' + KEY.tab);
		expect(t.lines()).toEqual(['$ kubectl get']);
		expect(t.cursor()).toEqual({ x: 14, y: 0 });
	});

	it('extends to the common prefix', async () => {
		const t = createHarness({ completions });
		await t.start();
		await t.type('kubectl get pods pa' + KEY.tab);
		expect(t.lines()).toEqual(['$ kubectl get pods payments-api-']);
	});

	it('lists the choices when nothing more can be added, then keeps the line', async () => {
		const t = createHarness({ completions, cols: 80 });
		await t.start();
		await t.type('kubectl get pods ' + KEY.tab);
		const lines = t.lines();
		expect(lines).toHaveLength(3);
		expect([lines[0], lines[2]]).toEqual(['$ kubectl get pods', '$ kubectl get pods']);
		expect(lines[1].split(/\s+/)).toEqual(['payments-api-a', 'web-1', 'web-2']);
		await t.type('w');
		expect(t.lines().slice(-1)).toEqual(['$ kubectl get pods w']);
	});

	it('rings the bell when there is nothing to complete', async () => {
		const bells: string[] = [];
		const t = createHarness({ completions });
		const original = t.term.onBell(() => bells.push('bell'));
		await t.start();
		await t.type('zzz' + KEY.tab);
		expect(t.lines()).toEqual(['$ zzz']);
		expect(bells).toEqual(['bell']);
		original.dispose();
	});

	it('also completes the terminal built-ins at the first word', async () => {
		const t = createHarness();
		await t.start();
		await t.type('he' + KEY.tab);
		expect(t.lines()).toEqual(['$ help']);
		await t.type(KEY.ctrlU + 'c' + KEY.tab);
		expect(t.lines()).toEqual(['$ clear']);
	});

	it('merges built-ins with engine suggestions for the first word', async () => {
		const t = createHarness({ completions: () => ({ start: 0, candidates: ['kubectl'] }) });
		await t.start();
		await t.type('k' + KEY.tab);
		expect(t.lines()).toEqual(['$ kubectl']);
		await t.type(KEY.ctrlU + KEY.tab);
		expect(t.lines()[1].split(/\s+/)).toEqual(['clear', 'help', 'kubectl']);
	});
});

describe('TerminalSession key reporting', () => {
	it('reports every key, in order, before handling it', async () => {
		const seen: string[] = [];
		const t = createHarness({ onKey: (key) => seen.push(key.type === 'char' ? `char:${key.text}` : key.type) });
		await t.start();
		await t.type('ls' + KEY.left + KEY.backspace + KEY.enter);
		expect(seen).toEqual(['char:ls', 'left', 'backspace', 'enter']);
	});

	it('reports a paste as text plus an enter per newline', async () => {
		const seen: string[] = [];
		const t = createHarness({ onKey: (key) => seen.push(key.type), commands: { a: '' } });
		await t.start();
		await t.type('a\na');
		expect(seen).toEqual(['char', 'enter', 'char']);
	});
});

describe('TerminalSession editor', () => {
	const open = async (files: Record<string, string> = {}) => {
		const t = createHarness({ cols: 80, files, prompt: '$ ' });
		await t.start();
		await t.type('vim pod.yaml' + KEY.enter);
		return t;
	};

	it('takes the whole screen for vim and gives the prompt back when it quits', async () => {
		const t = await open({ 'pod.yaml': 'kind: Pod\nname: x\n' });
		expect(t.inEditor()).toBe(true);
		const lines = t.lines();
		expect(lines.slice(0, 2)).toEqual(['kind: Pod', 'name: x']);
		expect(lines[2]).toBe('~');
		expect(lines.at(-1)).toMatch(/^"pod.yaml" 2L, 18B\s+pod.yaml {2}1,1$/);
		expect(t.cursor()).toEqual({ x: 0, y: 0 });
		await t.type(':q' + KEY.enter);
		expect(t.inEditor()).toBe(false);
		expect(t.lines().at(-1)).toBe('$');
		// The command that opened it is back in the scrollback, with the prompt below it.
		expect(t.lines()).toContain('$ vim pod.yaml');
	});

	it('edits, shows [+], saves to the terminal file and leaves', async () => {
		const t = await open({ 'pod.yaml': 'kind: Pod\n' });
		await t.type('A: v1' + KEY.escape);
		expect(t.lines().at(-1)).toMatch(/pod.yaml \[\+\] {2}1,/);
		await t.type(':w' + KEY.enter);
		expect(t.files['pod.yaml']).toBe('kind: Pod: v1\n');
		expect(t.lines().at(-1)).toMatch(/^"pod.yaml" 1L, 14B written/);
		await t.type(':q' + KEY.enter);
		expect(t.inEditor()).toBe(false);
	});

	it('creates a file that did not exist only when it is saved', async () => {
		const t = await open();
		expect(t.lines().at(-1)).toMatch(/^"pod.yaml" \[New\]/);
		await t.type('ikind: Pod' + KEY.escape + ':wq' + KEY.enter);
		expect(t.files['pod.yaml']).toBe('kind: Pod\n');
		expect(t.inEditor()).toBe(false);
		const quit = await open();
		await quit.type('ix' + KEY.escape + ':q!' + KEY.enter);
		expect(quit.files['pod.yaml']).toBeUndefined();
	});

	it('refuses to leave with unsaved changes, and scolds the third failed attempt', async () => {
		const t = await open({ 'pod.yaml': 'a\n' });
		await t.type('xi' + KEY.escape);
		await t.type(':q' + KEY.enter);
		expect(t.inEditor()).toBe(true);
		expect(t.lines().at(-1)).toContain('E37: No write since last change (add ! to override)');
		await t.type(KEY.ctrlC);
		expect(t.inEditor()).toBe(true);
		await t.type(KEY.ctrlZ);
		expect(t.inEditor()).toBe(true);
		expect(t.lines().at(-1)).toContain("I knew you were a rookie but I didn't know you were this incapable");
		await t.type(':q!' + KEY.enter);
		expect(t.inEditor()).toBe(false);
		expect(t.files['pod.yaml']).toBe('a\n');
	});

	it('shows what it does not understand on the status line', async () => {
		const t = await open({ 'pod.yaml': 'a\n' });
		await t.type(':set number' + KEY.enter);
		expect(t.lines().at(-1)).toContain('E492: Not an editor command: set number');
		await t.type('Z');
		expect(t.lines().at(-1)).toContain('E492: Not an editor command: Z');
	});

	it('redraws for the new size when the window changes', async () => {
		const t = await open({ 'pod.yaml': 'a\n' });
		await t.resize(70);
		expect(t.inEditor()).toBe(true);
		expect(t.lines().at(-1)).toMatch(/pod.yaml {2}1,1$/);
		await t.type(':q' + KEY.enter);
		expect(t.lines().at(-1)).toBe('$');
	});
});
