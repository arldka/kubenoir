import type { Key } from './keys';

/**
 * A small vi: a modal editor as a pure state machine, for the terminal's own files (manifests
 * above all). Keys go in, a screen model comes out; it knows nothing of xterm or the engine.
 *
 * Normal mode: h j k l w b e 0 $ gg G, i a I A o O, x dd yy p P, u Ctrl-R, counts, `:`.
 * Ex commands: :w :q :wq :x :q! and :N. Anything else says so on the status line (never silently).
 * Tab inserts two spaces and a new line keeps the previous line's indent, so YAML stays YAML.
 */

export const UNSAVED_MESSAGE = 'E37: No write since last change (add ! to override)';
/** What the boss thinks after the third failed attempt to leave. */
export const ROOKIE_MESSAGE = "I knew you were a rookie but I didn't know you were this incapable";
/** Failed exit attempts (`:q` refused, Ctrl-C, Ctrl-Z) before the scolding. */
const SCOLD_AFTER = 3;
const INDENT = '  ';

type Mode = 'normal' | 'insert' | 'ex';

interface Cursor {
	row: number;
	col: number;
}

interface Snapshot {
	lines: string[];
	cursor: Cursor;
}

interface Register {
	lines: string[];
	/** Whole lines (yy, dd) rather than characters (x). */
	linewise: boolean;
}

/** What the caller must do after a key. */
export interface VimResult {
	/** The screen changed. */
	redraw: boolean;
	/** Write this text to the file (`:w`, `:wq`, `:x`). */
	save?: string;
	/** Leave the editor. */
	quit?: boolean;
	/** Ring the bell. */
	bell?: boolean;
}

/** What to draw: `rows` text rows (padded with `~`), the status line, and the cursor. */
export interface VimScreen {
	rows: string[];
	status: string;
	cursor: { row: number; col: number };
}

export interface VimOptions {
	/** The name shown on the status line. */
	name: string;
	/** Terminal size; the last row is the status line. */
	rows: number;
	cols: number;
	/** The file did not exist yet. */
	isNew?: boolean;
}

const isBlank = (c: string | undefined) => c === undefined || c === '' || c === ' ' || c === '\t';
const isWord = (c: string) => /[A-Za-z0-9_]/.test(c);
/** 0 blank, 1 word character, 2 punctuation. */
const kind = (c: string) => (isBlank(c) ? 0 : isWord(c) ? 1 : 2);
const leadingIndent = (line: string) => /^[ \t]*/.exec(line)![0];

/** The text of a file as buffer lines: a final newline ends the last line, it does not add one. */
function toLines(text: string): string[] {
	if (text === '') return [''];
	const lines = text.split('\n');
	if (lines[lines.length - 1] === '') lines.pop();
	return lines;
}

/** What `:w` writes: the lines, each ending in a newline (nothing for an empty buffer). */
function toText(lines: string[]): string {
	return lines.length === 1 && lines[0] === '' ? '' : lines.join('\n') + '\n';
}

export class Vim {
	private lines: string[];
	private cursor: Cursor = { row: 0, col: 0 };
	private mode: Mode = 'normal';
	private saved: string;
	private message = '';
	private count = '';
	private pending: '' | 'd' | 'y' | 'g' = '';
	private ex = '';
	private register: Register | null = null;
	private undoStack: Snapshot[] = [];
	private redoStack: Snapshot[] = [];
	private failedExits = 0;
	/** The column vertical moves aim for (vim's `curswant`); Infinity after `$`. */
	private want = 0;
	private top = 0;
	private left = 0;
	/** An insert session is one undo step. */
	private insertStarted = false;

	constructor(
		text: string,
		private readonly options: VimOptions
	) {
		this.lines = toLines(text);
		this.saved = toText(this.lines);
		const bytes = text.length;
		this.message = options.isNew
			? `"${options.name}" [New]`
			: `"${options.name}" ${this.lines.length}L, ${bytes}B`;
	}

	/** The buffer as `:w` would write it. */
	get text(): string {
		return toText(this.lines);
	}

	/** Unsaved changes (the `[+]` flag). */
	get modified(): boolean {
		return this.text !== this.saved;
	}

	get currentMode(): Mode {
		return this.mode;
	}

	get failedExitAttempts(): number {
		return this.failedExits;
	}

	resize(rows: number, cols: number): void {
		this.options.rows = rows;
		this.options.cols = cols;
	}

	/** Feed one key from the terminal. */
	handle(key: Key): VimResult {
		// A pasted run is typed one character at a time.
		const chars = key.type === 'char' ? [...key.text] : [];
		if (chars.length > 1) {
			let merged: VimResult = { redraw: false };
			for (const text of chars) {
				const result = this.handle({ type: 'char', text });
				merged = { ...merged, ...result, redraw: merged.redraw || result.redraw };
				if (result.quit) break;
			}
			return merged;
		}
		const before = this.message;
		const result = this.dispatch(key);
		if (this.message !== before) result.redraw = true;
		return result;
	}

	// ---- input ------------------------------------------------------------------------------

	private dispatch(key: Key): VimResult {
		if (key.type === 'ctrl' && (key.key === 'c' || key.key === 'z')) return this.failedExit(key.key);
		if (this.mode === 'insert') return this.insert(key);
		if (this.mode === 'ex') return this.exKey(key);
		return this.normal(key);
	}

	private normal(key: Key): VimResult {
		this.message = '';
		switch (key.type) {
			case 'escape':
				this.count = '';
				this.pending = '';
				return { redraw: true };
			case 'left':
				return this.key('h');
			case 'right':
				return this.key('l');
			case 'up':
				return this.key('k');
			case 'down':
				return this.key('j');
			case 'home':
				return this.key('0');
			case 'end':
				return this.key('$');
			case 'delete':
				return this.key('x');
			case 'backspace':
				return this.key('h');
			case 'enter':
				return this.key('j');
			case 'ctrl':
				if (key.key === 'r') return this.redo();
				return this.unsupported(`Ctrl-${key.key.toUpperCase()}`);
			case 'char':
				return this.key(key.text);
			default:
				return this.unsupported(key.type);
		}
	}

	/** One normal-mode character. */
	private key(c: string): VimResult {
		const before = { ...this.cursor };
		const result = this.command(c);
		// Sideways moves and edits set the column that up and down then aim for.
		if (c !== 'j' && c !== 'k' && !/[1-9]/.test(c) && this.cursor.col !== before.col) {
			this.want = c === '$' ? Infinity : this.cursor.col;
		} else if (c === '$') {
			this.want = Infinity;
		}
		return result;
	}

	private command(c: string): VimResult {
		// Counts: 1-9 start one, 0 continues one (else it is a motion).
		if (/[1-9]/.test(c) || (c === '0' && this.count !== '')) {
			this.count += c;
			return { redraw: false };
		}
		const n = this.count === '' ? null : parseInt(this.count, 10);
		const times = n ?? 1;

		if (this.pending) {
			const op = this.pending;
			this.pending = '';
			this.count = '';
			if (op === 'g' && c === 'g') return this.go(n === null ? 0 : n - 1);
			if (op === 'd' && c === 'd') return this.deleteLines(times);
			if (op === 'y' && c === 'y') return this.yankLines(times);
			return this.unsupported(op + c);
		}

		this.count = '';
		switch (c) {
			case 'h':
				return this.moveCol(this.cursor.col - times);
			case 'l':
				return this.moveCol(this.cursor.col + times);
			case 'j':
				return this.moveRow(this.cursor.row + times);
			case 'k':
				return this.moveRow(this.cursor.row - times);
			case '0':
				return this.moveCol(0);
			case '$':
				return this.moveCol(this.lineLength(this.cursor.row) - 1);
			case 'G':
				return this.go(n === null ? this.lines.length - 1 : n - 1);
			case 'g':
			case 'd':
			case 'y':
				this.pending = c;
				if (n !== null) this.count = String(n);
				return { redraw: false };
			case 'w':
				return this.repeat(times, () => this.wordForward());
			case 'b':
				return this.repeat(times, () => this.wordBack());
			case 'e':
				return this.repeat(times, () => this.wordEnd());
			case 'i':
				return this.startInsert(this.cursor.col);
			case 'a':
				return this.startInsert(Math.min(this.cursor.col + 1, this.lineLength(this.cursor.row)));
			case 'I':
				return this.startInsert(this.firstNonBlank(this.cursor.row));
			case 'A':
				return this.startInsert(this.lineLength(this.cursor.row));
			case 'o':
				return this.openLine(1);
			case 'O':
				return this.openLine(0);
			case 'x':
				return this.deleteChars(times);
			case 'p':
				return this.put(true, times);
			case 'P':
				return this.put(false, times);
			case 'u':
				return this.undo();
			case ':':
				this.mode = 'ex';
				this.ex = '';
				return { redraw: true };
			default:
				return this.unsupported(c);
		}
	}

	private insert(key: Key): VimResult {
		this.message = '';
		switch (key.type) {
			case 'escape':
				this.leaveInsert();
				return { redraw: true };
			case 'char':
				this.type(key.text);
				return { redraw: true };
			case 'enter':
				this.newline();
				return { redraw: true };
			case 'tab':
				this.type(INDENT);
				return { redraw: true };
			case 'backspace':
				this.backspace();
				return { redraw: true };
			case 'delete': {
				const line = this.lines[this.cursor.row];
				if (this.cursor.col < line.length) {
					this.lines[this.cursor.row] = line.slice(0, this.cursor.col) + line.slice(this.cursor.col + 1);
				} else if (this.cursor.row < this.lines.length - 1) {
					this.lines.splice(this.cursor.row, 2, line + this.lines[this.cursor.row + 1]);
				}
				return { redraw: true };
			}
			case 'left':
				this.cursor.col = Math.max(0, this.cursor.col - 1);
				return { redraw: true };
			case 'right':
				this.cursor.col = Math.min(this.lineLength(this.cursor.row), this.cursor.col + 1);
				return { redraw: true };
			case 'up':
				this.cursor.row = Math.max(0, this.cursor.row - 1);
				this.clampInsertCol();
				return { redraw: true };
			case 'down':
				this.cursor.row = Math.min(this.lines.length - 1, this.cursor.row + 1);
				this.clampInsertCol();
				return { redraw: true };
			case 'home':
				this.cursor.col = 0;
				return { redraw: true };
			case 'end':
				this.cursor.col = this.lineLength(this.cursor.row);
				return { redraw: true };
			default:
				return { redraw: false };
		}
	}

	private exKey(key: Key): VimResult {
		switch (key.type) {
			case 'escape':
				this.mode = 'normal';
				this.ex = '';
				return { redraw: true };
			case 'backspace':
				if (this.ex === '') this.mode = 'normal';
				else this.ex = this.ex.slice(0, -1);
				return { redraw: true };
			case 'char':
				this.ex += key.text;
				return { redraw: true };
			case 'enter': {
				const command = this.ex.trim();
				this.mode = 'normal';
				this.ex = '';
				return this.runEx(command);
			}
			default:
				return { redraw: false };
		}
	}

	// ---- ex commands ------------------------------------------------------------------------

	private runEx(command: string): VimResult {
		if (command === '') return { redraw: true };
		if (/^\d+$/.test(command)) return this.go(parseInt(command, 10) - 1);
		switch (command) {
			case 'w':
				return this.write(false);
			case 'wq':
			case 'wq!':
			case 'x':
			case 'x!':
				return this.write(true);
			case 'q':
				if (this.modified) return this.failedExit('q');
				return { redraw: false, quit: true };
			case 'q!':
				return { redraw: false, quit: true };
			default:
				this.message = `E492: Not an editor command: ${command}`;
				return { redraw: true };
		}
	}

	private write(quit: boolean): VimResult {
		const text = this.text;
		this.saved = text;
		const bytes = text.length;
		this.message = `"${this.options.name}" ${this.lines.length}L, ${bytes}B written`;
		return quit ? { redraw: true, save: text, quit: true } : { redraw: true, save: text };
	}

	/** A try to leave that did not work: `:q` with unsaved changes, Ctrl-C, Ctrl-Z. */
	private failedExit(how: string): VimResult {
		this.failedExits++;
		// Ctrl-C leaves insert mode like Escape does; the others change nothing else.
		if (this.mode === 'insert' && how === 'c') this.leaveInsert();
		if (this.mode === 'ex') {
			this.mode = 'normal';
			this.ex = '';
		}
		this.count = '';
		this.pending = '';
		if (this.failedExits >= SCOLD_AFTER) this.message = ROOKIE_MESSAGE;
		else if (how === 'q') this.message = UNSAVED_MESSAGE;
		else if (how === 'c') this.message = 'Type  :qa!  and press <Enter> to abandon all changes and exit Vim';
		else this.message = 'Suspending is not available here; save with :w and leave with :q';
		return { redraw: true };
	}

	// ---- motions ----------------------------------------------------------------------------

	private lineLength(row: number): number {
		return this.lines[row]?.length ?? 0;
	}

	private firstNonBlank(row: number): number {
		const line = this.lines[row];
		const at = line.search(/[^ \t]/);
		return at === -1 ? line.length : at;
	}

	/** Normal mode keeps the cursor on a character, except on an empty line. */
	private clampNormal(): void {
		this.cursor.row = Math.max(0, Math.min(this.lines.length - 1, this.cursor.row));
		this.cursor.col = Math.max(0, Math.min(this.lineLength(this.cursor.row) - 1, this.cursor.col));
	}

	private clampInsertCol(): void {
		this.cursor.col = Math.min(this.cursor.col, this.lineLength(this.cursor.row));
	}

	private moveCol(col: number): VimResult {
		const before = this.cursor.col;
		this.cursor.col = col;
		this.clampNormal();
		return { redraw: this.cursor.col !== before };
	}

	private moveRow(row: number): VimResult {
		const before = { ...this.cursor };
		this.cursor.row = row;
		this.cursor.col = this.want;
		this.clampNormal();
		return { redraw: before.row !== this.cursor.row || before.col !== this.cursor.col };
	}

	private go(row: number): VimResult {
		this.cursor.row = Math.max(0, Math.min(this.lines.length - 1, row));
		this.cursor.col = this.firstNonBlank(this.cursor.row);
		this.clampNormal();
		return { redraw: true };
	}

	private repeat(times: number, step: () => boolean): VimResult {
		let moved = false;
		for (let i = 0; i < times; i++) {
			if (!step()) break;
			moved = true;
		}
		this.clampNormal();
		return { redraw: moved };
	}

	private charAt(pos: Cursor): string {
		return this.lines[pos.row][pos.col] ?? '';
	}

	/** Step to the next position in the buffer (the end of a line counts as a blank). */
	private next(pos: Cursor): Cursor | null {
		if (pos.col < this.lineLength(pos.row) - 1) return { row: pos.row, col: pos.col + 1 };
		if (pos.row < this.lines.length - 1) return { row: pos.row + 1, col: 0 };
		return null;
	}

	private prev(pos: Cursor): Cursor | null {
		if (pos.col > 0) return { row: pos.row, col: pos.col - 1 };
		if (pos.row > 0) return { row: pos.row - 1, col: Math.max(0, this.lineLength(pos.row - 1) - 1) };
		return null;
	}

	private wordForward(): boolean {
		const from = { ...this.cursor };
		let pos = { ...this.cursor };
		// Out of the word the cursor is in...
		const k = kind(this.charAt(pos));
		if (k !== 0) {
			while (pos.col < this.lineLength(pos.row) - 1 && kind(this.lines[pos.row][pos.col + 1]) === k) pos.col++;
		}
		// ...then on to the next word: past blanks and line ends, but an empty line is a word.
		for (;;) {
			const n = this.next(pos);
			if (!n) {
				const last = this.lines.length - 1;
				pos = { row: last, col: Math.max(0, this.lineLength(last) - 1) };
				break;
			}
			pos = n;
			if (this.lineLength(pos.row) === 0 || kind(this.charAt(pos)) !== 0) break;
		}
		this.cursor = pos;
		return pos.row !== from.row || pos.col !== from.col;
	}

	private wordBack(): boolean {
		let pos = this.prev(this.cursor);
		if (!pos) return false;
		while (pos && kind(this.charAt(pos)) === 0 && this.lineLength(pos.row) > 0) pos = this.prev(pos);
		if (!pos) {
			this.cursor = { row: 0, col: 0 };
			return true;
		}
		const k = kind(this.charAt(pos));
		for (;;) {
			const p = this.prev(pos);
			if (!p || p.row !== pos.row || kind(this.charAt(p)) !== k) break;
			pos = p;
		}
		this.cursor = pos;
		return true;
	}

	private wordEnd(): boolean {
		let pos = this.next(this.cursor);
		if (!pos) return false;
		while (pos && kind(this.charAt(pos)) === 0) pos = this.next(pos);
		if (!pos) {
			this.cursor = { row: this.lines.length - 1, col: Math.max(0, this.lineLength(this.lines.length - 1) - 1) };
			return true;
		}
		const k = kind(this.charAt(pos));
		for (;;) {
			const n = this.next(pos);
			if (!n || n.row !== pos.row || kind(this.charAt(n)) !== k) break;
			pos = n;
		}
		this.cursor = pos;
		return true;
	}

	// ---- changes ----------------------------------------------------------------------------

	private snapshot(): Snapshot {
		return { lines: [...this.lines], cursor: { ...this.cursor } };
	}

	/** Remember the buffer before a change, so `u` can bring it back. */
	private checkpoint(): void {
		this.undoStack.push(this.snapshot());
		this.redoStack = [];
	}

	private undo(): VimResult {
		const last = this.undoStack.pop();
		if (!last) {
			this.message = 'Already at oldest change';
			return { redraw: true };
		}
		this.redoStack.push(this.snapshot());
		this.lines = last.lines;
		this.cursor = last.cursor;
		this.clampNormal();
		return { redraw: true };
	}

	private redo(): VimResult {
		const next = this.redoStack.pop();
		if (!next) {
			this.message = 'Already at newest change';
			return { redraw: true };
		}
		this.undoStack.push(this.snapshot());
		this.lines = next.lines;
		this.cursor = next.cursor;
		this.clampNormal();
		return { redraw: true };
	}

	private startInsert(col: number): VimResult {
		this.checkpoint();
		this.insertStarted = true;
		this.mode = 'insert';
		this.cursor.col = col;
		return { redraw: true };
	}

	private leaveInsert(): void {
		this.mode = 'normal';
		// An insert that changed nothing is not an undo step.
		const last = this.undoStack[this.undoStack.length - 1];
		if (this.insertStarted && last && last.lines.join('\n') === this.lines.join('\n')) this.undoStack.pop();
		this.insertStarted = false;
		this.cursor.col = Math.max(0, this.cursor.col - 1);
		this.clampNormal();
	}

	private openLine(below: 0 | 1): VimResult {
		this.checkpoint();
		this.insertStarted = true;
		const indent = leadingIndent(this.lines[this.cursor.row]);
		const row = this.cursor.row + below;
		this.lines.splice(row, 0, indent);
		this.cursor = { row, col: indent.length };
		this.mode = 'insert';
		return { redraw: true };
	}

	private type(text: string): void {
		const line = this.lines[this.cursor.row];
		this.lines[this.cursor.row] = line.slice(0, this.cursor.col) + text + line.slice(this.cursor.col);
		this.cursor.col += text.length;
	}

	private newline(): void {
		const line = this.lines[this.cursor.row];
		const indent = leadingIndent(line);
		const head = line.slice(0, this.cursor.col);
		const tail = line.slice(this.cursor.col);
		this.lines[this.cursor.row] = head;
		this.lines.splice(this.cursor.row + 1, 0, indent + tail);
		this.cursor = { row: this.cursor.row + 1, col: indent.length };
	}

	private backspace(): void {
		const { row, col } = this.cursor;
		if (col > 0) {
			const line = this.lines[row];
			this.lines[row] = line.slice(0, col - 1) + line.slice(col);
			this.cursor.col--;
		} else if (row > 0) {
			const joinAt = this.lineLength(row - 1);
			this.lines.splice(row - 1, 2, this.lines[row - 1] + this.lines[row]);
			this.cursor = { row: row - 1, col: joinAt };
		}
	}

	private deleteChars(times: number): VimResult {
		const line = this.lines[this.cursor.row];
		if (line === '') return { redraw: false, bell: true };
		this.checkpoint();
		const { col } = this.cursor;
		const removed = line.slice(col, col + times);
		this.lines[this.cursor.row] = line.slice(0, col) + line.slice(col + times);
		this.register = { lines: [removed], linewise: false };
		this.clampNormal();
		return { redraw: true };
	}

	private deleteLines(times: number): VimResult {
		this.checkpoint();
		const { row } = this.cursor;
		const removed = this.lines.splice(row, times);
		this.register = { lines: removed, linewise: true };
		if (this.lines.length === 0) this.lines = [''];
		this.cursor.row = Math.min(row, this.lines.length - 1);
		this.cursor.col = this.firstNonBlank(this.cursor.row);
		this.clampNormal();
		return { redraw: true };
	}

	private yankLines(times: number): VimResult {
		const { row } = this.cursor;
		this.register = { lines: this.lines.slice(row, row + times), linewise: true };
		const n = this.register.lines.length;
		if (n > 2) this.message = `${n} lines yanked`;
		return { redraw: true };
	}

	private put(after: boolean, times: number): VimResult {
		const register = this.register;
		if (!register) {
			this.message = 'E353: Nothing in register "';
			return { redraw: true };
		}
		this.checkpoint();
		if (register.linewise) {
			const block = Array.from({ length: times }, () => register.lines).flat();
			const at = this.cursor.row + (after ? 1 : 0);
			this.lines.splice(at, 0, ...block);
			this.cursor = { row: at, col: 0 };
			this.cursor.col = this.firstNonBlank(at);
		} else {
			const text = register.lines.join('\n').repeat(times);
			const line = this.lines[this.cursor.row];
			const at = line === '' ? 0 : this.cursor.col + (after ? 1 : 0);
			this.lines[this.cursor.row] = line.slice(0, at) + text + line.slice(at);
			this.cursor.col = at + text.length - 1;
		}
		this.clampNormal();
		return { redraw: true };
	}

	private unsupported(what: string): VimResult {
		this.count = '';
		this.pending = '';
		this.message = `E492: Not an editor command: ${what}`;
		return { redraw: true };
	}

	// ---- output -----------------------------------------------------------------------------

	/** The screen for the current state, scrolling to keep the cursor in view. */
	screen(): VimScreen {
		const height = Math.max(1, this.options.rows - 1);
		const width = Math.max(1, this.options.cols);
		if (this.cursor.row < this.top) this.top = this.cursor.row;
		if (this.cursor.row >= this.top + height) this.top = this.cursor.row - height + 1;
		if (this.cursor.col < this.left) this.left = this.cursor.col;
		if (this.cursor.col >= this.left + width) this.left = this.cursor.col - width + 1;

		const rows: string[] = [];
		for (let i = 0; i < height; i++) {
			const line = this.lines[this.top + i];
			rows.push(line === undefined ? '~' : line.slice(this.left, this.left + width));
		}

		const flag = this.modified ? ' [+]' : '';
		const where = `${this.cursor.row + 1},${this.cursor.col + 1}`;
		let left: string;
		if (this.mode === 'ex') left = ':' + this.ex;
		else if (this.message) left = this.message;
		else if (this.mode === 'insert') left = '-- INSERT --';
		else left = this.pendingText();
		const right = `${this.options.name}${flag}  ${where}`;
		// A message takes the whole line when it needs it, like vim's own.
		// One column short of the terminal's width: writing the last one would wait to wrap.
		const room = width - 1;
		const gap = room - left.length - right.length;
		const status = gap >= 1 ? left + ' '.repeat(gap) + right : left.slice(0, room);
		const cursor =
			this.mode === 'ex'
				? { row: height, col: Math.min(width - 1, 1 + this.ex.length) }
				: { row: this.cursor.row - this.top, col: this.cursor.col - this.left };
		return { rows, status, cursor };
	}

	/** Half-typed normal-mode commands, as vim shows them in the corner. */
	private pendingText(): string {
		return this.count + this.pending;
	}
}
