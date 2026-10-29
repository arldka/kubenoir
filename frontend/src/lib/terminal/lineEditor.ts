import type { History } from './history';
import type { CtrlKey, Key } from './keys';

/** What the engine says could complete the word before the cursor. */
export interface Completion {
	/** Index in the line where the word being completed starts. */
	start: number;
	/** Full replacement words. */
	candidates: string[];
}

/** What the session must do after a key. The editor itself only changes its own state. */
export type Action =
	| 'none'
	| 'redraw'
	| 'bell'
	| 'interrupt'
	| 'clear'
	| 'complete'
	| { submit: string };

export type CompletionResult = { kind: 'none' } | { kind: 'applied' } | { kind: 'list'; items: string[] };

function wordStart(text: string, from: number): number {
	let i = from;
	while (i > 0 && text[i - 1] === ' ') i--;
	while (i > 0 && text[i - 1] !== ' ') i--;
	return i;
}

function wordEnd(text: string, from: number): number {
	let i = from;
	while (i < text.length && text[i] === ' ') i++;
	while (i < text.length && text[i] !== ' ') i++;
	return i;
}

function commonPrefix(words: string[]): string {
	let prefix = words[0] ?? '';
	for (const word of words) {
		let i = 0;
		while (i < prefix.length && i < word.length && prefix[i] === word[i]) i++;
		prefix = prefix.slice(0, i);
	}
	return prefix;
}

/**
 * A shell-style line editor as a pure state machine: no DOM, no terminal. It owns the text, the
 * cursor and the history position; rendering and running commands belong to the session.
 */
export class LineEditor {
	buffer = '';
	cursor = 0;

	/** Index into history while browsing it, otherwise null. */
	private position: number | null = null;
	/** What was typed before the first Up, restored when browsing past the newest entry. */
	private draft = '';

	constructor(private readonly history: History) {}

	reset(): void {
		this.buffer = '';
		this.cursor = 0;
		this.position = null;
		this.draft = '';
	}

	handle(key: Key): Action {
		switch (key.type) {
			case 'char':
				this.replace(this.cursor, this.cursor, key.text);
				return 'redraw';
			case 'enter':
				return { submit: this.buffer };
			case 'tab':
				return 'complete';
			case 'backspace':
				if (this.cursor === 0) return 'bell';
				this.replace(this.cursor - 1, this.cursor, '');
				return 'redraw';
			case 'delete':
				if (this.cursor >= this.buffer.length) return 'bell';
				this.replace(this.cursor, this.cursor + 1, '');
				return 'redraw';
			case 'left':
				return this.moveTo(this.cursor - 1);
			case 'right':
				return this.moveTo(this.cursor + 1);
			case 'wordLeft':
				return this.moveTo(wordStart(this.buffer, this.cursor));
			case 'wordRight':
				return this.moveTo(wordEnd(this.buffer, this.cursor));
			case 'home':
				return this.moveTo(0);
			case 'end':
				return this.moveTo(this.buffer.length);
			case 'up':
				return this.browse(-1);
			case 'down':
				return this.browse(1);
			case 'ctrl':
				return this.control(key.key);
			case 'escape':
				return 'none';
		}
	}

	private control(key: CtrlKey): Action {
		switch (key) {
			case 'a':
				return this.moveTo(0);
			case 'e':
				return this.moveTo(this.buffer.length);
			case 'c':
				return 'interrupt';
			case 'l':
				return 'clear';
			case 'u':
				if (this.cursor === 0) return 'none';
				this.replace(0, this.cursor, '');
				return 'redraw';
			case 'k':
				if (this.cursor >= this.buffer.length) return 'none';
				this.replace(this.cursor, this.buffer.length, '');
				return 'redraw';
			case 'w': {
				const start = wordStart(this.buffer, this.cursor);
				if (start === this.cursor) return 'none';
				this.replace(start, this.cursor, '');
				return 'redraw';
			}
			case 'd':
				// Like a shell, Ctrl+D deletes under the cursor; on an empty line there is nothing to exit.
				return this.buffer ? this.handle({ type: 'delete' }) : 'bell';
			default:
				// Ctrl+R and Ctrl+Z mean something to the editor, nothing to the prompt.
				return 'none';
		}
	}

	/** Replace [from, to) with `text`, leave the cursor after it and stop browsing history. */
	private replace(from: number, to: number, text: string): void {
		this.buffer = this.buffer.slice(0, from) + text + this.buffer.slice(to);
		this.cursor = from + text.length;
		this.position = null;
		this.draft = '';
	}

	private moveTo(index: number): Action {
		const clamped = Math.max(0, Math.min(this.buffer.length, index));
		if (clamped === this.cursor) return 'none';
		this.cursor = clamped;
		return 'redraw';
	}

	private browse(direction: -1 | 1): Action {
		const entries = this.history.entries();
		if (direction === -1) {
			if (entries.length === 0) return 'bell';
			if (this.position === null) {
				this.draft = this.buffer;
				this.position = entries.length - 1;
			} else if (this.position > 0) {
				this.position -= 1;
			} else {
				return 'bell';
			}
			this.buffer = entries[this.position];
		} else {
			if (this.position === null) return 'bell';
			if (this.position < entries.length - 1) {
				this.position += 1;
				this.buffer = entries[this.position];
			} else {
				this.position = null;
				this.buffer = this.draft;
				this.draft = '';
			}
		}
		this.cursor = this.buffer.length;
		return 'redraw';
	}

	/**
	 * Apply the engine's candidates to the word before the cursor: a single match is inserted
	 * (plus a space when the word is finished), several are extended to their common prefix, and
	 * when nothing more can be added they are returned to be listed.
	 */
	applyCompletion(completion: Completion): CompletionResult {
		const { start, candidates } = completion;
		if (candidates.length === 0) return { kind: 'none' };

		const typed = this.buffer.slice(start, this.cursor);
		if (candidates.length === 1) {
			const word = candidates[0];
			const finished = !word.endsWith('/') && !word.endsWith('=');
			const spaceFollows = this.buffer[this.cursor] === ' ';
			this.replace(start, this.cursor, word + (finished && !spaceFollows ? ' ' : ''));
			return { kind: 'applied' };
		}

		const prefix = commonPrefix(candidates);
		if (prefix.length > typed.length) {
			this.replace(start, this.cursor, prefix);
			return { kind: 'applied' };
		}
		return { kind: 'list', items: candidates };
	}
}
