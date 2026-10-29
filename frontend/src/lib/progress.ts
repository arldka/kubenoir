/** Which cases the player has finished, kept in this browser (a progress service may replace it). */

const KEY = 'k8sd:progress';

export function loadCompleted(storage: Pick<Storage, 'getItem'> | null = safeStorage()): Set<string> {
	try {
		const raw = storage?.getItem(KEY);
		const list: unknown = raw ? JSON.parse(raw) : [];
		return new Set(Array.isArray(list) ? list.filter((id): id is string => typeof id === 'string') : []);
	} catch {
		return new Set();
	}
}

/** Remember that `id` was solved; returns the new set. Quietly does nothing if storage is blocked. */
export function markCompleted(id: string, storage: Pick<Storage, 'getItem' | 'setItem'> | null = safeStorage()): Set<string> {
	const done = loadCompleted(storage);
	done.add(id);
	try {
		storage?.setItem(KEY, JSON.stringify([...done]));
	} catch {
		// Private windows and blocked storage: the campaign just does not remember.
	}
	return done;
}

function safeStorage(): Storage | null {
	try {
		return typeof localStorage === 'undefined' ? null : localStorage;
	} catch {
		return null;
	}
}

/** A case is open when it is finished, or every case before it is. */
export function caseState(
	cases: { id: string }[],
	completed: ReadonlySet<string>
): { id: string; state: 'done' | 'current' | 'locked' }[] {
	const firstOpen = cases.findIndex((c) => !completed.has(c.id));
	return cases.map((c, i) => ({
		id: c.id,
		state: completed.has(c.id) ? 'done' : i === firstOpen ? 'current' : 'locked'
	}));
}
