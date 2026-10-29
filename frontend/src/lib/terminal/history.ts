/** The slice of the Web Storage API we use, so tests can pass a fake. */
export interface KeyValueStorage {
	getItem(key: string): string | null;
	setItem(key: string, value: string): void;
}

const DEFAULT_LIMIT = 200;

/** The browser's localStorage, or null where it is missing or blocked (private mode, etc.). */
function browserStorage(): KeyValueStorage | null {
	try {
		return globalThis.localStorage ?? null;
	} catch {
		return null;
	}
}

/**
 * Command history, newest last. Persisted per scenario when storage is available; everything
 * still works in memory when it is not.
 */
export class History {
	private items: string[] = [];

	constructor(
		private readonly key: string,
		private readonly storage: KeyValueStorage | null = browserStorage(),
		private readonly limit = DEFAULT_LIMIT
	) {
		try {
			const raw = this.storage?.getItem(this.key);
			const parsed: unknown = raw ? JSON.parse(raw) : [];
			if (Array.isArray(parsed)) {
				this.items = parsed.filter((x): x is string => typeof x === 'string').slice(-this.limit);
			}
		} catch {
			this.items = [];
		}
	}

	entries(): readonly string[] {
		return this.items;
	}

	/** Remember a command; blank lines and immediate repeats are skipped. */
	add(line: string): void {
		const entry = line.trim();
		if (!entry || this.items[this.items.length - 1] === entry) return;
		this.items.push(entry);
		if (this.items.length > this.limit) this.items.splice(0, this.items.length - this.limit);
		try {
			this.storage?.setItem(this.key, JSON.stringify(this.items));
		} catch {
			// Storage full or blocked: keep the in-memory history.
		}
	}
}
