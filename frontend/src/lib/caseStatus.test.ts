import { describe, expect, it } from 'vitest';

import { formatDuration, parseStatus } from './caseStatus';

const open = {
	solved: false,
	outcome: null,
	solvedAtSeconds: null,
	elapsedSeconds: 12,
	commands: 2,
	findings: { total: 3, found: [{ id: 'logs', clue: 'c', atSeconds: 5 }] },
	report: null
};

const closed = {
	...open,
	solved: true,
	outcome: 'closedWithCost',
	solvedAtSeconds: 21,
	report: {
		assertions: [{ label: 'a', ok: true }],
		constraints: [{ label: 'c', ok: false, brokenAtSeconds: 3 }, { label: 'd', ok: true, brokenAtSeconds: null }],
		missedFindings: ['unseen clue']
	}
};

describe('parseStatus', () => {
	it('reads an open case', () => {
		const s = parseStatus(JSON.stringify(open))!;
		expect(s.solved).toBe(false);
		expect(s.findings.found[0].id).toBe('logs');
		expect(s.report).toBeNull();
	});

	it('reads a closed case with its report', () => {
		const s = parseStatus(JSON.stringify(closed))!;
		expect(s.outcome).toBe('closedWithCost');
		expect(s.solvedAtSeconds).toBe(21);
		expect(s.report?.constraints[0]).toEqual({ label: 'c', ok: false, brokenAtSeconds: 3 });
		expect(s.report?.missedFindings).toEqual(['unseen clue']);
	});

	it('rejects anything that is not the expected shape', () => {
		expect(parseStatus('{nope')).toBeNull();
		expect(parseStatus('[]')).toBeNull();
		expect(parseStatus('{}')).toBeNull();
		expect(parseStatus(JSON.stringify({ ...open, solved: 'yes' }))).toBeNull();
		expect(parseStatus(JSON.stringify({ ...open, outcome: 'won' }))).toBeNull();
		expect(parseStatus(JSON.stringify({ ...open, commands: 'many' }))).toBeNull();
		expect(parseStatus(JSON.stringify({ ...open, findings: { total: 1, found: [{ id: 1 }] } }))).toBeNull();
		expect(parseStatus(JSON.stringify({ ...closed, report: { assertions: [], constraints: [1], missedFindings: [] } }))).toBeNull();
	});
});

describe('formatDuration', () => {
	it('formats minutes and hours', () => {
		expect(formatDuration(0)).toBe('00:00');
		expect(formatDuration(83)).toBe('01:23');
		expect(formatDuration(3725)).toBe('1:02:05');
		expect(formatDuration(-5)).toBe('00:00');
	});
});
