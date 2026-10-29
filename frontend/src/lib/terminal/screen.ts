/**
 * Turning the editor's text and cursor into terminal escape sequences.
 *
 * The input line (prompt + text) is redrawn from scratch on every change: go back to where the
 * block started, clear downwards, reprint, then park the cursor. Wrapped lines span several rows,
 * so the previous layout is needed to find the start. Every character is assumed one cell wide.
 */

export interface Layout {
	/** Rows the block occupies. */
	rows: number;
	/** Row (0-based, from the top of the block) the cursor was left on. */
	cursorRow: number;
}

const ANSI = /\x1b\[[0-9;]*[A-Za-z]/g;

/** Cells a string takes up on screen once escape sequences are removed. */
export function visibleLength(text: string): number {
	return text.replace(ANSI, '').length;
}

/**
 * Where the block and cursor end up for the given size. A line that exactly fills a row still
 * gets the next row (see `render`), so end-of-text always has a well-defined row.
 */
export function layoutFor(promptLength: number, bufferLength: number, cursor: number, cols: number): Layout {
	const total = promptLength + bufferLength;
	return {
		rows: Math.floor(total / cols) + 1,
		cursorRow: Math.floor((promptLength + cursor) / cols)
	};
}

export function render(
	prompt: string,
	buffer: string,
	cursor: number,
	cols: number,
	previous: Layout | null
): { data: string; layout: Layout } {
	const promptLength = visibleLength(prompt);
	const total = promptLength + buffer.length;
	const layout = layoutFor(promptLength, buffer.length, cursor, cols);

	let data = '';
	if (previous && previous.cursorRow > 0) data += `\x1b[${previous.cursorRow}A`;
	data += '\r\x1b[J' + prompt + buffer;
	// A line that exactly fills the row leaves the terminal in a pending-wrap state; force the wrap
	// so the cursor really sits at the start of the next row.
	if (total > 0 && total % cols === 0) data += ' \r';

	const endRow = Math.floor(total / cols);
	const up = endRow - layout.cursorRow;
	if (up > 0) data += `\x1b[${up}A`;
	const column = (promptLength + cursor) % cols;
	data += '\r' + (column > 0 ? `\x1b[${column}C` : '');
	return { data, layout };
}

/** Move from wherever the cursor is in the block to the line below its last row. */
export function toNextLine(layout: Layout | null): string {
	const down = layout ? layout.rows - 1 - layout.cursorRow : 0;
	return (down > 0 ? `\x1b[${down}B` : '') + '\r\n';
}

/** Lay `items` out in aligned columns, like a shell listing completions. */
export function columns(items: string[], cols: number): string {
	const width = Math.max(...items.map((i) => i.length)) + 2;
	const perRow = Math.max(1, Math.floor(cols / width));
	const rows = Math.ceil(items.length / perRow);
	const lines: string[] = [];
	for (let r = 0; r < rows; r++) {
		let line = '';
		for (let c = 0; c < perRow; c++) {
			const item = items[r + c * rows];
			if (item !== undefined) line += item.padEnd(width);
		}
		lines.push(line.trimEnd());
	}
	return lines.join('\n');
}
