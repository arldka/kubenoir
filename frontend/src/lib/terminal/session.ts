import { BUILTINS, runBuiltin } from './builtins';
import type { History } from './history';
import { LineEditor, type Completion } from './lineEditor';
import { parseKeys, type Key } from './keys';
import { Vim } from './vim';
import { columns, layoutFor, render, toNextLine, visibleLength, type Layout } from './screen';

const CLEAR_SCREEN = '\x1b[2J\x1b[3J\x1b[H';
const BELL = '\x07';
const ALT_SCREEN_ON = '\x1b[?1049h';
const ALT_SCREEN_OFF = '\x1b[?1049l';
/** Amber user, brass host, like a worn terminal label. */
const DEFAULT_PROMPT = '\x1b[38;5;179mdetective\x1b[0m@\x1b[38;5;137mcluster\x1b[0m:~$ ';

export interface SessionOptions {
	/** Sink for escape-sequence output (an xterm.js `Terminal.write`). */
	write(data: string): void;
	/** Current terminal width in columns. */
	cols(): number;
	/** Current terminal height in rows (for the editor); defaults to 24. */
	rows?(): number;
	evalCommand(command: string): Promise<string>;
	/** The file `vim` asked to open, once. */
	takeEditorRequest?(): string | undefined;
	readFile?(path: string): string | undefined;
	/** The editor saved the file. */
	writeFile?(path: string, text: string): void;
	complete(linePrefix: string): Completion | Promise<Completion>;
	history: History;
	/**
	 * How many rows above the cursor the current input line starts (following wrapped rows up
	 * to the real line start). Used after a resize, when the terminal may have re-flowed the
	 * text; without it the session assumes no re-flow.
	 */
	rowsAboveCursor?(): number;
	/** Called for every key before it is handled (used for typewriter sounds). */
	onKey?(key: Key): void;
	banner?: string;
	prompt?: string;
}

/**
 * One interactive shell: reads keys, edits the line, runs commands and prints their output.
 * It talks to the screen only through `write`, so it can be driven by xterm.js in the browser
 * and by a headless terminal in tests.
 */
export class TerminalSession {
	private readonly editor: LineEditor;
	private readonly prompt: string;
	private layout: Layout | null = null;
	/** Input is handled strictly in order, even while a command is still running. */
	private queue: Promise<void> = Promise.resolve();
	/** The editor that has the whole screen, while there is one. */
	private editing: { vim: Vim; path: string } | null = null;

	constructor(private readonly options: SessionOptions) {
		this.editor = new LineEditor(options.history);
		this.prompt = options.prompt ?? DEFAULT_PROMPT;
	}

	start(): void {
		if (this.options.banner) this.print(this.options.banner);
		this.newLine();
	}

	/** Feed raw terminal input. Resolves once everything in it has been handled. */
	onData(data: string): Promise<void> {
		this.queue = this.queue.then(() => this.handleData(data)).catch(() => {});
		return this.queue;
	}

	/** The terminal changed size: redraw the input line for the new width. */
	resize(): void {
		if (this.editing) return this.drawEditor(true);
		if (!this.layout) return;
		const layout = layoutFor(visibleLength(this.prompt), this.editor.buffer.length, this.editor.cursor, this.options.cols());
		this.layout = { ...layout, cursorRow: this.options.rowsAboveCursor?.() ?? layout.cursorRow };
		this.draw();
	}

	private async handleData(data: string): Promise<void> {
		if (this.editing) {
			for (const key of parseKeys(data, true)) this.handleEditorKey(key);
			return;
		}
		for (const key of parseKeys(data)) {
			await this.handleKey(key);
			// What is left of a paste belongs to the editor the command just opened.
			if (this.editing) break;
		}
	}

	private async handleKey(key: Key): Promise<void> {
		this.options.onKey?.(key);
		const action = this.editor.handle(key);
		if (action === 'none') return;
		if (action === 'redraw') return this.draw();
		if (action === 'bell') return this.options.write(BELL);
		if (action === 'clear') {
			this.options.write(CLEAR_SCREEN);
			this.layout = null;
			return this.draw();
		}
		if (action === 'interrupt') {
			this.options.write(toNextLine(this.layout) + '^C\r\n');
			return this.newLine();
		}
		if (action === 'complete') return this.complete();

		this.options.write(toNextLine(this.layout));
		this.layout = null;
		const command = action.submit.trim();
		if (command) {
			this.options.history.add(command);
			await this.run(command);
		}
		if (!this.editing) this.newLine();
	}

	private async run(command: string): Promise<void> {
		const builtin = runBuiltin(command);
		if (builtin) {
			if ('clear' in builtin) this.options.write(CLEAR_SCREEN);
			else this.print(builtin.output);
			return;
		}
		try {
			this.print(await this.options.evalCommand(command));
		} catch (e) {
			this.print(`error: ${e instanceof Error ? e.message : String(e)}`);
		}
		const file = this.options.takeEditorRequest?.();
		if (file !== undefined) this.openEditor(file);
	}

	// ---- the editor ----------------------------------------------------------------------------

	private openEditor(path: string): void {
		const text = this.options.readFile?.(path);
		const vim = new Vim(text ?? '', {
			name: path,
			rows: this.options.rows?.() ?? 24,
			cols: this.options.cols(),
			isNew: text === undefined
		});
		this.editing = { vim, path };
		this.options.write(ALT_SCREEN_ON);
		this.drawEditor();
	}

	private handleEditorKey(key: Key): void {
		const editing = this.editing;
		if (!editing) return;
		this.options.onKey?.(key);
		const result = editing.vim.handle(key);
		if (result.save !== undefined) this.options.writeFile?.(editing.path, result.save);
		if (result.quit) return this.closeEditor();
		if (result.bell) this.options.write(BELL);
		if (result.redraw) this.drawEditor();
	}

	private closeEditor(): void {
		this.editing = null;
		this.options.write(ALT_SCREEN_OFF);
		this.newLine();
	}

	/** Paint the whole editor: text rows, the status line in reverse video, then the cursor. */
	private drawEditor(clear = false): void {
		const editing = this.editing;
		if (!editing) return;
		const cols = this.options.cols();
		const rows = this.options.rows?.() ?? 24;
		editing.vim.resize(rows, cols);
		const screen = editing.vim.screen();
		// After a resize the old rows may have been cut or re-flowed: start from a clean screen.
		let out = (clear ? '\x1b[2J' : '') + '\x1b[?25l';
		screen.rows.forEach((row, i) => {
			const shown = row === '~' ? '\x1b[38;5;66m~\x1b[0m' : row;
			out += `\x1b[${i + 1};1H\x1b[2K${shown}`;
		});
		const status = screen.status.padEnd(Math.max(0, cols - 1));
		out += `\x1b[${rows};1H\x1b[2K\x1b[7m${status}\x1b[0m`;
		out += `\x1b[${screen.cursor.row + 1};${screen.cursor.col + 1}H\x1b[?25h`;
		this.options.write(out);
	}

	private async complete(): Promise<void> {
		const prefix = this.editor.buffer.slice(0, this.editor.cursor);
		let completion = await this.options.complete(prefix);
		if (completion.start === 0) {
			// The first word may also be one of the terminal's own commands.
			const own = BUILTINS.filter((b) => b.startsWith(prefix));
			completion = { start: 0, candidates: [...new Set([...completion.candidates, ...own])].sort() };
		}

		const result = this.editor.applyCompletion(completion);
		if (result.kind === 'none') return this.options.write(BELL);
		if (result.kind === 'applied') return this.draw();

		this.options.write(toNextLine(this.layout));
		this.print(columns(result.items, this.options.cols()));
		this.layout = null;
		this.draw();
	}

	/** Start a fresh, empty input line. */
	private newLine(): void {
		this.editor.reset();
		this.layout = null;
		this.draw();
	}

	private draw(): void {
		const { data, layout } = render(
			this.prompt,
			this.editor.buffer,
			this.editor.cursor,
			this.options.cols(),
			this.layout
		);
		this.options.write(data);
		this.layout = layout;
	}

	/** Print output followed by a newline, translating `\n` for the terminal. */
	private print(text: string): void {
		if (!text) return;
		this.options.write(text.replace(/\r?\n/g, '\r\n') + (text.endsWith('\n') ? '' : '\r\n'));
	}
}
