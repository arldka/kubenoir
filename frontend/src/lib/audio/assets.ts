/**
 * Every audio file in `assets/`, found at build time. A file that is not there is simply absent
 * from this map, so nothing is requested for it (no 404s) and the game stays quiet in its place.
 */
export const assetFiles = import.meta.glob<string>('./assets/*.{ogg,opus,m4a,mp3,wav,flac}', {
	eager: true,
	query: '?url',
	import: 'default'
}) as Record<string, string>;
