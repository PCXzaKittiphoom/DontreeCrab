# Changelog

## 0.1.2

First full release. Adds the song database, external MIDI output with
per-channel routing, a song search popup, image backgrounds and many player
and UI refinements.

### Song database and search

- **Song Data** page and SQLite library: scan configured folders, incremental
  rescans, duplicate detection and a per-file type filter.
- **Check missing** and **Compact** run in the background with a status line,
  and **Backup** writes a consistent copy of the database.
- **Add a song** copies a file (and the NCN companions) into the program
  folder and rolls the copies back if the database insert fails.
- The player checks the library for missing files once in the background at
  startup.
- Search popup built from the library: type to search by code, title, artist or
  lyrics, browse the results with `?`/`?`, `Enter` plays and `Esc` closes.
  The panel keeps a song info bar, a lyric preview and a translucent
  black/white look, and it no longer covers the volume readout.

### MIDI

- **MIDI Out** lists the real output ports on the machine and sends playback to
  the selected device instead of the SoundFont.
- Voice locks and the drum-kit lock apply live and also reach external outputs;
  every output receives all-notes-off on pause, stop, seek and at the end.
- **Split MIDI** window: one row per channel, with drag/Ctrl selection and a
  right-click menu to route the selected channels to the default port, the
  built-in RustySynth SoundFont or any device.
- **MIDI Reset** selection and `Ctrl+Backspace` send the reset immediately.
- Metronome (enable, first/other beat notes, hi-hat option) and voice locks
  (bass, drum kit, kick, snare) on the MIDI page.
- Key-change modes **All**, **Wolf** and **Recovery**.

### Player and UI

- Playlist is a floating card over the lyrics instead of a panel that shrank
  the lyric area.
- Background page offers the artwork shipped with the player, a flat colour or
  an external `.png`/`.jpg`, scaled to the window, with 4:3 / 16:9 previews.
- "Delete played songs", "Play immediately" and "Automatically play" are on by
  default.
- Regrouped right-click menu with icons, **Song Player**, **Song Data**,
  **Split MIDI** and **MIDI Out** submenus and **About**.
- `Esc` closes the search popup, the context menu, the playlist and the
  Split MIDI window.
- Keyboard shortcuts no longer fire behind settings windows, and the mouse
  wheel no longer changes the volume while a window is open.
- Many alignment and layout fixes (settings pages, key/reset labels, background
  page, MIDI page).
- Thai and English strings for every new feature.

### Packaging

- `assets/backgrounds/` ships with the source and is embedded in the binary.
- Refreshed splash artwork.

## 0.1.1 Beta

Patch release for the 0.1.0 Beta launcher problem.

- Double-clicking `DontreeCrab.exe` now opens the player instead of printing
  help and exiting.
- The console window is hidden when the GUI is launched directly on Windows.

## 0.1.0 Beta

Initial public beta release.

- KAR, NCN, KMID and plain MIDI playback.
- Timed lyric display with Thai leading-vowel handling and two cursor styles.
- Tempo-map-aware BPM readout and constant BPM offset.
- Chord-aware key transposition.
- Playlist, drag & drop, playlist JSON import/export.
- Runtime `.po` translations with English and Thai.
- Lucide icon set.
- Configurable SoundFont polyphony and buffer.
- In-memory repair of altered SMF headers for interoperability.
- Windows file associations and release packaging.
- Startup splash with version and SoundFont loading status.
- Fullscreen state is remembered between launches.
- Usage notice below the MIT license in About → License.
- Fade-out animation when closing the About license and credits popups.
