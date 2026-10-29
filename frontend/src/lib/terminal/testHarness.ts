/** Test support: runs a TerminalSession against a real (headless) xterm.js terminal. */
import { Terminal } from '@xterm/headless';

import { History } from './history';
import type { Key } from './keys';
import type { Completion } from './lineEditor';
import { TerminalSession } from './session';

export interface HarnessOptions {
	cols?: number;
	rows?: number;
	/** Engine stand-in: command -> output. Unknown commands return `error: unknown`. */
	commands?: Record<string, string> | ((command: string) => string | Promise<string>);
	completions?: (prefix: string) => Completion;
	history?: string[];
	banner?: string;
	prompt?: string;
	onKey?: (key: Key) => void;
	/** The terminal's own files; `vim FILE` opens the editor on them, saving writes them back. */
	files?: Record<string, string>;
}

/** Rows between the cursor and the start of its (possibly wrapped) logical line. */
export function rowsAboveCursor(term: Terminal): number {
	const buffer = term.buffer.active;
	let row = buffer.baseY + buffer.cursorY;
	let up = 0;
	while (row > 0 && buffer.getLine(row)?.isWrapped) {
		row--;
		up++;
	}
	return up;
}

export function createHarness(options: HarnessOptions = {}) {
	const term = new Terminal({ cols: options.cols ?? 40, rows: options.rows ?? 24, allowProposedApi: true });
	const pending: Promise<void>[] = [];
	const evaluated: string[] = [];

	const history = new History('test', null);
	for (const entry of options.history ?? []) history.add(entry);

	const commands = options.commands ?? {};
	const files: Record<string, string> = { ...(options.files ?? {}) };
	let editorRequest: string | undefined;
	const session = new TerminalSession({
		write: (data) => void pending.push(new Promise<void>((resolve) => term.write(data, resolve))),
		cols: () => term.cols,
		rows: () => term.rows,
		takeEditorRequest: () => {
			const path = editorRequest;
			editorRequest = undefined;
			return path;
		},
		readFile: (path) => files[path],
		writeFile: (path, text) => void (files[path] = text),
		evalCommand: async (command) => {
			evaluated.push(command);
			const vim = /^vim? (\S+)$/.exec(command);
			if (vim) {
				editorRequest = vim[1];
				return '';
			}
			return typeof commands === 'function' ? commands(command) : (commands[command] ?? 'error: unknown');
		},
		complete: options.completions ?? (() => ({ start: 0, candidates: [] })),
		history,
		rowsAboveCursor: () => rowsAboveCursor(term),
		onKey: options.onKey,
		banner: options.banner,
		prompt: options.prompt ?? '$ '
	});

	const settle = async () => {
		while (pending.length) await pending.shift();
	};

	return {
		term,
		session,
		history,
		evaluated,
		files,
		async start() {
			session.start();
			await settle();
		},
		async type(data: string) {
			await session.onData(data);
			await settle();
		},
		async resize(cols: number) {
			term.resize(cols, term.rows);
			session.resize();
			await settle();
		},
		/** Every screen row with trailing blanks trimmed, ignoring empty rows at the end. */
		lines(): string[] {
			const buffer = term.buffer.active;
			const rows: string[] = [];
			for (let i = 0; i < buffer.length; i++) rows.push((buffer.getLine(i)?.translateToString(true) ?? '').trimEnd());
			while (rows.length && rows[rows.length - 1] === '') rows.pop();
			return rows;
		},
		cursor: () => ({ x: term.buffer.active.cursorX, y: term.buffer.active.cursorY }),
		/** Whether the alternate screen (the editor) is showing. */
		inEditor: () => term.buffer.active.type === 'alternate'
	};
}

export const KEY = {
	enter: '\r',
	tab: '\t',
	backspace: '\x7f',
	up: '\x1b[A',
	down: '\x1b[B',
	right: '\x1b[C',
	left: '\x1b[D',
	home: '\x1b[H',
	end: '\x1b[F',
	delete: '\x1b[3~',
	ctrlA: '\x01',
	ctrlC: '\x03',
	ctrlD: '\x04',
	ctrlE: '\x05',
	ctrlK: '\x0b',
	ctrlL: '\x0c',
	ctrlU: '\x15',
	ctrlW: '\x17',
	ctrlR: '\x12',
	ctrlZ: '\x1a',
	escape: '\x1b',
	ctrlLeft: '\x1b[1;5D',
	ctrlRight: '\x1b[1;5C'
};
