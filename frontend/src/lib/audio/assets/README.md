# Audio assets

Drop recordings in this folder. The role of each file is decided by its **name**; a file that is
not here is simply skipped (nothing is requested, nothing errors), so the game works with any
subset. Until real key sounds exist, typing uses a synthesized click.

| file | role |
|---|---|
| `ambience.ogg` | the whole background soundtrack: slow 1940s sax, vinyl crackle, rain, the detective's slow breathing and the cigarette, all mixed into one track. Looped with a crossfade. |
| `key-1.ogg`, `key-2.ogg`, ... | typewriter / old keyboard key presses, picked at random (never the same twice in a row) |
| `key-space.ogg` | space bar (optional) |
| `key-return.ogg` | Enter: carriage return, perhaps a bell (optional) |
| `key-backspace.ogg` | Backspace and the line-kill shortcuts (optional) |

## The ambience track

- **Length:** at least 30 seconds, ideally a few minutes, so the repetition is hard to notice.
- **Seamless looping:** the last 4 seconds fade out while the first 4 seconds fade in over them
  (equal-power crossfade, in `loop.ts`). So avoid loud one-off events (a sax solo start, a
  cigarette lighter) in the first and last 4 seconds, or they will be heard twice at the seam.
  Steady material at both ends loops best.
- **Level:** master it quietly (around -23 LUFS integrated) with headroom; the game mixes it at
  80% and the player controls overall volume.

## Format

Use **Ogg Vorbis or Opus** (`.ogg` / `.opus`): they decode without the gaps MP3 adds. `.m4a`, `.mp3`,
`.wav` and `.flac` are also picked up. If the same recording exists in several formats the first
one the browser can play wins, in the order ogg, opus, m4a, mp3, wav, flac.

```sh
# Ogg Vorbis, quality 5, from any source file
ffmpeg -i source.wav -c:a libvorbis -q:a 5 ambience.ogg
# Short key sounds: mono is enough
ffmpeg -i key.wav -ac 1 -c:a libvorbis -q:a 4 key-1.ogg
```

Key sounds should be short (well under half a second), trimmed tight at the start so the click
lands the instant the key is pressed.

## Licensing

This is an open-source project: only add sound you may redistribute (CC0, CC-BY, or your own).
List the source and license of **every file** in `CREDITS.md` in this folder.
