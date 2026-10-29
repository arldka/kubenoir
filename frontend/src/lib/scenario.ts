/** The player-facing parts of a scenario file. The engine reads the rest. */
export interface ScenarioMeta {
	id: string;
	title: string;
	briefing: string;
	/** The one visible goal, in the briefing's own words (no extra clues). */
	objective: string;
	hints: string[];
}

export const SUPPORTED_SCHEMA_VERSION = 1;

/** Parse and validate scenario metadata, with errors an author can act on. */
export function parseScenarioMeta(json: string): ScenarioMeta {
	let data: unknown;
	try {
		data = JSON.parse(json);
	} catch (e) {
		throw new Error(`Scenario is not valid JSON: ${e instanceof Error ? e.message : String(e)}`);
	}
	if (typeof data !== 'object' || data === null) throw new Error('Scenario must be a JSON object');
	const record = data as Record<string, unknown>;

	if (record.schemaVersion !== SUPPORTED_SCHEMA_VERSION) {
		throw new Error(
			`Unsupported scenario schemaVersion ${JSON.stringify(record.schemaVersion)}; expected ${SUPPORTED_SCHEMA_VERSION}`
		);
	}
	const text = (field: string): string => {
		const value = record[field];
		if (typeof value !== 'string' || !value.trim()) throw new Error(`Scenario needs a non-empty string "${field}"`);
		return value;
	};

	const hints = record.hints ?? [];
	if (!Array.isArray(hints) || !hints.every((h) => typeof h === 'string')) {
		throw new Error('Scenario "hints" must be an array of strings');
	}
	return { id: text('id'), title: text('title'), briefing: text('briefing'), objective: text('objective'), hints };
}

/** A stable four-digit case number derived from the scenario id, for the case-file header. */
export function caseNumber(id: string): number {
	let hash = 2166136261;
	for (const c of id) hash = Math.imul(hash ^ c.charCodeAt(0), 16777619) >>> 0;
	return 1000 + (hash % 9000);
}
