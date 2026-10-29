import { describe, expect, it } from 'vitest';

import { caseNumber, parseScenarioMeta } from './scenario';

import realScenario from '../../../scenarios/crashloop-env-typo/scenario.json?raw';

const valid = { schemaVersion: 2, id: 'x', title: 'T', briefing: 'B', objective: 'O' };

describe('parseScenarioMeta', () => {
	it('reads the shipped scenario', () => {
		const meta = parseScenarioMeta(realScenario);
		expect(meta.id).toBe('crashloop-env-typo');
		expect(meta.title).toContain('Payments');
		expect(meta.hints).toHaveLength(3);
		expect(meta.objective).toContain('payments-api');
		expect(meta.briefing).toContain(meta.objective);
	});

	it('defaults hints to an empty list', () => {
		expect(parseScenarioMeta(JSON.stringify(valid)).hints).toEqual([]);
	});

	it('explains what is wrong', () => {
		const parse = (o: unknown) => () => parseScenarioMeta(JSON.stringify(o));
		expect(() => parseScenarioMeta('{nope')).toThrow(/not valid JSON/);
		expect(() => parseScenarioMeta('[]')).toThrow(/needs|schemaVersion/);
		expect(parse({ ...valid, schemaVersion: 1 })).toThrow(/schemaVersion 1/);
		expect(parse({ ...valid, schemaVersion: undefined })).toThrow(/schemaVersion/);
		expect(parse({ ...valid, title: '' })).toThrow(/"title"/);
		expect(parse({ ...valid, briefing: 5 })).toThrow(/"briefing"/);
		expect(parse({ ...valid, objective: undefined })).toThrow(/"objective"/);
		expect(parse({ ...valid, hints: 'no' })).toThrow(/"hints"/);
		expect(parse({ ...valid, hints: [1] })).toThrow(/"hints"/);
	});
});

describe('caseNumber', () => {
	it('is stable and four digits', () => {
		expect(caseNumber('crashloop-env-typo')).toBe(caseNumber('crashloop-env-typo'));
		const n = caseNumber('anything');
		expect(n).toBeGreaterThanOrEqual(1000);
		expect(n).toBeLessThan(10000);
		expect(caseNumber('a')).not.toBe(caseNumber('b'));
	});
});
