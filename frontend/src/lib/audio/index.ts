import { assetFiles } from './assets';
import { buildManifest } from './manifest';
import { soundSettings } from './settings';
import { Soundscape } from './soundscape';

export { soundSettings } from './settings';
export { soundForKey } from './typing';

let instance: Soundscape | undefined;

/** The one soundscape for the page, following the saved settings. */
export function getSoundscape(): Soundscape {
	if (!instance) {
		const soundscape = new Soundscape({ manifest: buildManifest(assetFiles) });
		soundSettings.subscribe((settings) => soundscape.setSettings(settings));
		instance = soundscape;
	}
	return instance;
}

/**
 * Start the sound on the player's first click or key press (browsers block audio before a
 * gesture), and pause it while the tab is hidden. Returns a function that undoes this.
 */
export function armSound(): () => void {
	const soundscape = getSoundscape();
	const unlock = () => {
		void soundscape.start();
	};
	const onVisibility = () => (document.hidden ? soundscape.suspend() : soundscape.resume());

	window.addEventListener('pointerdown', unlock);
	window.addEventListener('keydown', unlock);
	document.addEventListener('visibilitychange', onVisibility);
	return () => {
		window.removeEventListener('pointerdown', unlock);
		window.removeEventListener('keydown', unlock);
		document.removeEventListener('visibilitychange', onVisibility);
	};
}
