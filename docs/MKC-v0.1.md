# MKC v0.1 — MIDI Karaoke Container

MKC is an open, compressed container for MIDI karaoke songs. It packages a
Standard MIDI File together with timed lyrics and metadata in one `.mkc` file.

MKC is **not** encrypted. The archive is ordinary ZIP and can be inspected with
any ZIP tool. There is no XOR layer and no DRM.

- Extension: `.mkc`
- MIME type: `application/mkc+zip`
- Current version: `1`
- Reference implementation: `karaoke_player::mkc`
- GUI: separate `mkc-converter` binary (`crates/tools/mkc-gui`)

## 0. Source and output folders

MKC keeps every format in its own prefix folder. The converter accepts NCN,
KMID, KAR and lyric-bearing MIDI sources, and always writes results under an
`MKC` folder:

```text
NCN\Song\S\SONG0001.mid   ->  MKC\S\SONG0001.mkc
NCN\S\SONG0001.mid        ->  MKC\S\SONG0001.mkc
KMID\S\SONG0002.mid       ->  MKC\S\SONG0002.mkc
KAR\S\SONG0003.kar        ->  MKC\S\SONG0003.mkc
```

The letter folder is the first character of the song code (`SONG0001` lives in
`S`), which is how the source libraries group their songs.

A game/folder scan can therefore find all converted songs under the `MKC` folder.
The CLI's `--output` option is the **base** folder; it creates `MKC\...`
inside it.

## 1. Archive layout

An MKC file is a ZIP archive. A v1 writer must write these entries:

| Entry | Required | Compression | Meaning |
|---|---|---|---|
| `mimetype` | yes | stored | exactly `application/mkc+zip`, no trailing newline |
| `manifest.json` | yes | deflate or stored | UTF-8 metadata |
| `song.mid` | yes | deflate or stored | the original Standard MIDI File |
| `lyrics.json` | recommended | deflate or stored | UTF-8 timed lyrics |
| `lyrics.txt` | optional | deflate or stored | UTF-8 plain lyrics |

`mimetype` must be the first entry and must not be compressed. This follows the
same pattern as EPUB/ODF and lets tools identify the file without reading the
whole archive.

Readers must ignore unknown entries. They must reject a package that is missing
`manifest.json` or `song.mid`, or whose `mimetype` has the wrong value.

ZIP32 limits apply in v1: each entry must be smaller than 4 GiB.

## 2. `manifest.json`

```json
{
  "format": "MKC",
  "version": 1,
  "code": "SONG0001",
  "title": "Sample Title",
  "artist": "Sample Artist",
  "key": "C",
  "language": "en",
  "tempo": 120.0,
  "duration_ms": 215000,
  "ticks_per_quarter": 480,
  "track_count": 8,
  "vocal_channel": 9,
  "created_by": "DontreeCrab MKC 0.1.3",
  "sections": {
    "midi": "song.mid",
    "lyrics": "lyrics.json",
    "lyrics_text": "lyrics.txt"
  },
  "source": {
    "format": "ncn",
    "midi_file": "SONG0001.mid",
    "lyric_file": "SONG0001.lyr",
    "cursor_file": "SONG0001.cur",
    "lyric_encoding": "windows-874"
  }
}
```

Field rules:

- `format` is always `"MKC"`.
- `version` is the MKC major version. Readers must reject versions they do not
  understand.
- `title` and `artist` may be empty strings but must be present.
- `key`, `language`, and `vocal_channel` may be `null`.
- `language` is a BCP-47 style tag. Writers should use the primary subtag
  only: `th`, `en`, `ja`, `zh`, `ko`, `vi`, `lo`. Readers must accept regional
  forms (`th-TH`, `zh-CN`, `zh-TW`) and may treat them as their primary
  subtag. When the source provides no language, the converter detects it from
  the lyric text.
- `tempo` is beats per minute.
- `duration_ms` is the song duration in milliseconds.
- `ticks_per_quarter` is the MIDI division (PPQN).
- `created_by` is informational.
- `sections` maps logical sections to entry names. A reader must use these
  names rather than assuming fixed names, so future writers can rename entries.
- `source` is informational provenance. `format` may be `"ncn"`, `"kar"`,
  `"kmid"`, `"midi"`, or a future value.
- For non-NCN sources `lyric_file` and `cursor_file` are empty strings, and
  `lyric_encoding` is `"embedded"` or `"none"`.

All text in `manifest.json` is UTF-8. There is no BOM.

## 3. `lyrics.json`

```json
{
  "format": "mkc-lyrics",
  "version": 1,
  "time_unit": "us",
  "lines": [
    {
      "start_us": 120000,
      "end_us": 180000,
      "text": "Sample line",
      "syllables": [
        { "start_us": 120000, "end_us": 150000, "text": "Sam" },
        { "start_us": 150000, "end_us": 180000, "text": "ple line" }
      ]
    }
  ]
}
```

- All times are integer **microseconds** from the start of the song.
- `text` is the full line used for display.
- `syllables` is ordered. A syllable's `text` may be one or more characters.
  Empty syllable arrays are allowed when timing is unavailable.
- `start_us <= end_us` for every line and syllable.
- Lines are sorted by `start_us`.
- Text is UTF-8.

The v1 NCN converter uses one syllable per displayed character because that is
how the NCN cursor stores timing. Other writers may group characters into
linguistic syllables without changing the format.

## 4. `lyrics.txt`

Optional plain-text convenience entry, UTF-8, using `\n` line endings:

```text
Title
Artist
Key

first lyric line
second lyric line
```

The canonical timed representation remains `lyrics.json`.

## 5. MIDI

`song.mid` is the original Standard MIDI File, byte-for-byte. MKC does not
rewrite the MIDI. This means other applications can extract `song.mid` and play
it with any standard MIDI player.

## 6. Compression

- `mimetype` is stored (method 0).
- Other entries may use ZIP deflate (method 8) or stored (method 0).
- A writer may choose stored when deflate would make an entry larger.
- CRC32 from the ZIP entry must be correct. Readers should verify it.

## 7. Security and limits

- Do not execute anything from an MKC file.
- Reject entry names containing absolute paths, `..`, or drive prefixes when
  extracting.
- Enforce a size limit before allocating decompressed data, because ZIP files
  can be compression bombs.
- MKC v1 provides integrity (CRC32), not authenticity. If provenance matters,
  a future optional signature entry can be added without changing the archive
  layout.

## 8. NCN mapping

An NCN song is a `.mid` with `.lyr` and `.cur` companions.

MKC v1 conversion:

- `song.mid` = the original `.mid`
- `lyrics.json` = timed lines built from the NCN `.lyr` text and `.cur` cursor
- `lyrics.txt` = UTF-8 title/artist/key plus the plain lyric lines
- `manifest.source` = original file names and `lyric_encoding: "windows-874"`

NCN cursor values are converted to absolute microseconds through the MIDI tempo
map before being written to `lyrics.json`.

## 9. Future optional entries

These names are reserved for future versions and must be ignored by v1 readers:

- `cover.png` / `cover.jpg`
- `translations.json`
- `credits.txt`
- `signature.ed25519`
- `MIDI_ORIGINAL` (only if a future writer needs to preserve a pre-edit MIDI)

## 10. CLI

```text
mkc pack <input.mid|folder> -o <base-folder> [--recursive] [--overwrite]
mkc info <file.mkc>
mkc unpack <file.mkc> -o <folder>
```

Folder scans accept NCN, KMID, KAR and MIDI files that carry lyrics. Plain
MIDI files without lyrics are ignored. Outputs always go below `<base>\MKC\`.
