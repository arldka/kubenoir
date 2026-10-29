/**
 * Which sound files exist and what role each plays, decided purely by file name so that adding a
 * recording to `assets/` is all it takes (and a missing one is simply skipped):
 *
 *   ambience.ogg                                            the whole soundtrack (music, vinyl,
 *                                                           rain, breathing, smoking), looped
 *                                                           with a crossfade
 *   key-1.ogg  key-2.ogg ...                                typing, picked at random
 *   key-space.ogg  key-return.ogg  key-backspace.ogg        typing specials
 */

export interface Manifest {
	ambience?: string;
	keys: string[];
	space?: string;
	return?: string;
	backspace?: string;
}

/** Formats in order of preference. Ogg decodes gaplessly, which loops need; MP3 adds a gap. */
const FORMATS = ['ogg', 'opus', 'm4a', 'mp3', 'wav', 'flac'] as const;

const MIME: Record<string, string> = {
	ogg: 'audio/ogg; codecs="vorbis"',
	opus: 'audio/ogg; codecs="opus"',
	m4a: 'audio/mp4; codecs="mp4a.40.2"',
	mp3: 'audio/mpeg',
	wav: 'audio/wav',
	flac: 'audio/flac'
};

/** Whether this browser can play a file extension. */
export function browserCanPlay(extension: string): boolean {
	try {
		const mime = MIME[extension];
		return !!mime && document.createElement('audio').canPlayType(mime) !== '';
	} catch {
		return false;
	}
}

/**
 * Group the available files (path -> URL) into roles. If one recording exists in several
 * formats, the first format in preference order that the browser can play wins.
 */
export function buildManifest(files: Record<string, string>, canPlay: (extension: string) => boolean = browserCanPlay): Manifest {
	// logical name (e.g. "key-3") -> extension -> URL
	const byName = new Map<string, Map<string, string>>();
	for (const [path, url] of Object.entries(files)) {
		const match = /([^/\\]+)\.([a-z0-9]+)$/i.exec(path);
		if (!match) continue;
		const name = match[1].toLowerCase();
		const extension = match[2].toLowerCase();
		if (!byName.has(name)) byName.set(name, new Map());
		byName.get(name)!.set(extension, url);
	}

	const pick = (name: string): string | undefined => {
		const formats = byName.get(name);
		if (!formats) return undefined;
		const extension = FORMATS.find((f) => formats.has(f) && canPlay(f));
		return extension ? formats.get(extension) : undefined;
	};

	const numbered = (prefix: string): string[] => {
		const found: { index: number; url: string }[] = [];
		for (const name of byName.keys()) {
			const m = new RegExp(`^${prefix}-(\\d+)$`).exec(name);
			const url = m && pick(name);
			if (m && url) found.push({ index: Number(m[1]), url });
		}
		return found.sort((a, b) => a.index - b.index).map((f) => f.url);
	};

	return {
		ambience: pick('ambience'),
		keys: numbered('key'),
		space: pick('key-space'),
		return: pick('key-return'),
		backspace: pick('key-backspace')
	};
}
