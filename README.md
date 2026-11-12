# DontreeCrab

A MIDI karaoke player for **KAR**, **NCN**, **KMID** and plain MIDI files, written
in Rust with `eframe`/`egui` and `rustysynth`.

[GitHub repository](https://github.com/PCXzaKittiphoom/DontreeCrab)

DontreeCrab focuses on accurate lyric timing, Thai karaoke conventions, a clean
monochrome interface, and offline playback. It does not collect telemetry and
does not need a network connection.

> **No songs or SoundFonts are bundled.** You supply your own karaoke files and
> a General MIDI SoundFont. The synthetic files under `testdata/` are only for
> tests and examples.

## Features

- KAR (`.kar`), NCN (`.mid` + `.lyr` + `.cur`), KMID (`LyrHdr` payload) and
  ordinary MIDI playback.
- Timed lyric lines with a feathered per-syllable wipe, Thai leading-vowel
  handling, and support for the classic alternating cursor.
- Bar/beat display that follows the MIDI time-signature map.
- Tempo map support: the BPM readout and speed follow tempo changes, with a
  constant user BPM offset.
- Key transposition with chord-aware display (`G [0]` → `A# [+3]`).
- Playlist, drag & drop, auto-advance, repeat, musical interval and JSON
  import/export.
- Volume, BPM, font and UI-zoom readouts.
- Runtime translations from GNU gettext `.po` files (English and Thai included).
- Window/file associations on Windows.
- Large, multi-layer SoundFont support with configurable polyphony.
- Startup splash that shows the loading progress while the SoundFont loads.
- Fullscreen state that is remembered between launches.
- About page with version, credits, GitHub link, MIT licence and a build/credits
  popup.

## Requirements

- Rust stable (edition 2024).
- A General MIDI SoundFont (`.sf2`) for audio playback.
- Windows, macOS or Linux. The GUI is tested mainly on Windows.

## Build

```sh
cargo build --release
```

The workspace has two crates: `karaoke_player` (library) and `dontreecrab`
(desktop app). The default member is the app, so the command above builds the
player. The executable is `target/release/DontreeCrab` (`.exe` on Windows).

## SoundFont

DontreeCrab looks for a SoundFont in this order:

1. `--sf2 <path>`
2. `$KARAOKE_SF2`
3. the first `.sf2` in `./soundfonts`
4. the first `.sf2` in the working directory

Put your SoundFont in `soundfonts/` or set `KARAOKE_SF2`. Do not commit
SoundFont files to the repository.

## Run

```sh
# GUI
cargo run --release -- gui testdata/sample.kar

# CLI information
cargo run -- info testdata/sample.kar
cargo run -- lyrics testdata/sample.kar

# Play through the sound card
cargo run -- play testdata/sample.kar

# Render to WAV
cargo run --release -- render testdata/sample.kar -o preview.wav --duration 12
```

`testdata/sample.kar` and `testdata/ncn/Song/1/test.mid` are synthetic fixtures
created for the test suite. They are not real songs.

## GUI controls

| Key | Action |
|---|---|
| `F5` | play / pause |
| `F6` | stop and unload the current song |
| `F7` / `F8` | previous / next bar (`BACK <<<` / `NEXT >>>`) |
| `Space` | show the song card when Song info is off and the playlist is closed |
| `Esc` | dismiss the song card, then close the playlist |
| `Ctrl+,` / `Ctrl+.` | transpose −1 / +1 |
| `Ctrl+;` / `Ctrl+'` | BPM −1 / +1 |
| `Ctrl+\` | mute / unmute the melody channel |
| `Alt+-` / `Alt++` | smaller / larger lyric text |
| `Ctrl+-` / `Ctrl++` / `Ctrl+0` | UI zoom out / in / reset |
| `Ctrl+O` | open a file or import a playlist JSON |
| `Ctrl+R` | toggle playlist repeat |
| `Delete` | remove the highlighted or current playlist entry |

Right-click the lyrics for **Song Player** (open/add files, add folder,
import/export playlist, repeat, clear), fullscreen, minimize, cursor visibility,
playlist visibility, settings and exit.

The playlist is also a drop target for `.kar`, `.mid` and `.midi` files.

## Settings

Open Settings from the right-click menu. The pages are:

- **General** — readout/playlist display durations, top-bar visibility, language.
- **Text** — lyric font size, outline, distance, visibility, style and colours.
- **SoundFont** — active SoundFont, polyphony (8–256), buffer (0 = driver
  default, or 10–100 ms), equalizer and MIDI effect sends.
- **Player** — delete played songs, play immediately, auto-play and musical
  interval.
- **Background** — background colour and a 4:3 / 16:9 preview.

Settings are stored in the platform config directory (`%APPDATA%\DontreeCrab\
DontreeCrab\config\settings.json` on Windows).

### Polyphony and buffer

`rustysynth` defaults to 64 voices, which is too low for large multi-layer
SoundFonts. DontreeCrab defaults to **256 voices** (the library maximum) and lets
you lower it if the CPU cannot keep up.

The SoundFont buffer defaults to **0 = driver default**. The manual range is
**10–100 ms**; 20–30 ms is a good balance. Very small values are often rejected
by Windows shared-mode drivers and fall back to the driver default.

## Languages

English and Thai are built in. The UI reads `.po` files directly; no `.mo`
compilation is required.

`locales/dontreecrab.pot` is the template. Copy it to `locales/<code>.po`, fill
the `msgstr` lines, keep the `{}` placeholders, and place the file in one of:

```
locales/ next to DontreeCrab.exe
locales/ in the working directory
%APPDATA%\DontreeCrab\DontreeCrab\config\locales
```

Then restart and choose the language in Settings → General → **Language**.
Missing entries fall back to English.

## Formats

### KAR

A Standard MIDI File (`.kar`) with lyric meta events. DontreeCrab reads `@T`
(title), `@I` (artist), lyric events and `@INTRO` / `@SOLO` / `@OUTTRO` control
markers. Only the three control markers are hidden.

### NCN

Nick Karaoke's three parallel files:

```
NCN/Song/1/00001.mid
NCN/Lyrics/1/00001.lyr
NCN/Cursor/1/00001.cur
```

The `.lyr` file is Windows-874/UTF-8 text with a four-line header (title,
artist, key, blank) followed by lyric lines. The `.cur` file contains one
little-endian `u16` per displayed character, terminated by `0xFFFF`; a value `v`
maps to `tick = v * (division / 24)`.

### KMID

A Standard MIDI File with an embedded `LyrHdr` text event. The payload is
`LyrHdr` + one version character + Base64(zlib(XML)). DontreeCrab converts the
word-level lyric cursor into timed lyric lines.

### MIDI

Any other Standard MIDI File plays normally and is labelled `MIDI Common` when
no karaoke markers are found.

### Copy-protected headers

Some old authoring tools replaced the four-byte `MThd` magic with arbitrary
text (for example `Lock`) to stop the file opening in sequencers. DontreeCrab
checks that the rest of the header and the first `MTrk` chunk are intact and
repairs the magic **in memory only** when opening the file. The original file is
never modified. This is intended for interoperability with files the user
already owns.

## Testing

```sh
cargo test --workspace
```

The audio tests use any `.sf2` found in `soundfonts/` or `$KARAOKE_SF2` and
skip cleanly when none is present. The MIDI fixtures under `testdata/` are
synthetic and safe to redistribute.

## Packaging

```sh
powershell -ExecutionPolicy Bypass -File scripts/build-release.ps1
```

This creates `dist/DontreeCrab-<version>.zip` with the executable, README,
translation catalogs, and font licenses. A `soundfonts/` placeholder is added;
no SoundFont is bundled.

## Project layout

```
crates/
├─ core/               karaoke_player library
│  ├─ src/
│  │  ├─ model.rs      Song, LyricLine, Syllable, TimedEvent
│  │  ├─ tempo.rs      TempoMap: MIDI tick ↔ microseconds
│  │  ├─ midi.rs       SMF parsing and header repair
│  │  ├─ text.rs       UTF-8 / Windows-874 decoding
│  │  ├─ lyrics.rs     syllable list → timed lyric lines
│  │  ├─ kar.rs        KAR metadata + lyric extraction
│  │  ├─ kmid.rs       embedded LyrHdr XML lyrics
│  │  ├─ ncn.rs        .lyr/.cur loader
│  │  ├─ loader.rs     format sniffing
│  │  ├─ soundfont.rs  .sf2 discovery
│  │  ├─ i18n.rs       runtime .po catalogs
│  │  └─ audio/
│  │     ├─ player.rs  synth + event scheduler + transport
│  │     ├─ output.rs  cpal output stream
│  │     └─ wav.rs     offline render
│  ├─ tests/           integration tests
│  └─ examples/        melody and SoundFont inspection tools
└─ player/             dontreecrab desktop app
   ├─ build.rs         Windows icon and resources
   └─ src/
      ├─ main.rs       CLI (info / dump / lyrics / play / render)
      ├─ gui.rs        eframe/egui player
      ├─ icons.rs      embedded Lucide icon font
      └─ assoc.rs      Windows file associations

testdata/               synthetic fixtures
assets/ logo/ locales/  shared assets
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Bug reports and pull requests are
welcome. Please do not attach copyrighted karaoke files, lyrics, SoundFonts, or
screenshots containing commercial lyrics.

## License

MIT. See [LICENSE](LICENSE).

Third-party assets and dependencies are listed in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). The embedded fonts are
distributed under the SIL Open Font License 1.1 (Sarabun, E1234) and the ISC
License (Lucide). No SoundFont or karaoke song is included.
