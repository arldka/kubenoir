/** How the case stands, as reported by the engine's judge. */
export interface FoundFinding {
	id: string;
	clue: string;
	atSeconds: number;
}

export interface Report {
	assertions: { label: string; ok: boolean }[];
	constraints: { label: string; ok: boolean; brokenAtSeconds: number | null }[];
	/** Clues the player never uncovered, revealed once the case is closed. */
	missedFindings: string[];
}

export interface CaseStatus {
	solved: boolean;
	outcome: 'closed' | 'closedWithCost' | null;
	solvedAtSeconds: number | null;
	elapsedSeconds: number;
	commands: number;
	findings: { total: number; found: FoundFinding[] };
	/** Present only once the case is closed. */
	report: Report | null;
}

type Obj = Record<string, unknown>;

const isObj = (v: unknown): v is Obj => typeof v === 'object' && v !== null && !Array.isArray(v);
const isNum = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v);

/** Parse the engine's status JSON; null if it is not the shape we expect. */
export function parseStatus(raw: string): CaseStatus | null {
	let data: unknown;
	try {
		data = JSON.parse(raw);
	} catch {
		return null;
	}
	if (!isObj(data) || typeof data.solved !== 'boolean') return null;
	if (!isNum(data.elapsedSeconds) || !isNum(data.commands)) return null;
	if (data.outcome !== null && data.outcome !== 'closed' && data.outcome !== 'closedWithCost') return null;
	if (!(data.solvedAtSeconds === null || isNum(data.solvedAtSeconds))) return null;

	const findings = data.findings;
	if (!isObj(findings) || !isNum(findings.total) || !Array.isArray(findings.found)) return null;
	const found: FoundFinding[] = [];
	for (const f of findings.found) {
		if (!isObj(f) || typeof f.id !== 'string' || typeof f.clue !== 'string' || !isNum(f.atSeconds)) return null;
		found.push({ id: f.id, clue: f.clue, atSeconds: f.atSeconds });
	}

	let report: Report | null = null;
	if (data.report !== null && data.report !== undefined) {
		const r = data.report;
		if (!isObj(r) || !Array.isArray(r.assertions) || !Array.isArray(r.constraints) || !Array.isArray(r.missedFindings)) {
			return null;
		}
		const assertions = r.assertions.map((a) => (isObj(a) ? { label: String(a.label), ok: a.ok === true } : null));
		const constraints = r.constraints.map((c) =>
			isObj(c)
				? { label: String(c.label), ok: c.ok === true, brokenAtSeconds: isNum(c.brokenAtSeconds) ? c.brokenAtSeconds : null }
				: null
		);
		if (assertions.includes(null) || constraints.includes(null)) return null;
		report = {
			assertions: assertions as Report['assertions'],
			constraints: constraints as Report['constraints'],
			missedFindings: r.missedFindings.filter((m): m is string => typeof m === 'string')
		};
	}

	return {
		solved: data.solved,
		outcome: data.outcome,
		solvedAtSeconds: data.solvedAtSeconds,
		elapsedSeconds: data.elapsedSeconds,
		commands: data.commands,
		findings: { total: findings.total, found },
		report
	};
}

/** `83` -> `01:23`, `3725` -> `1:02:05`. */
export function formatDuration(totalSeconds: number): string {
	const s = Math.max(0, Math.floor(totalSeconds));
	const h = Math.floor(s / 3600);
	const m = String(Math.floor((s % 3600) / 60)).padStart(2, '0');
	const sec = String(s % 60).padStart(2, '0');
	return h > 0 ? `${h}:${m}:${sec}` : `${m}:${sec}`;
}
