# Third-party notices

DontreeCrab itself is MIT-licensed. The assets and Rust crates below keep their
own licenses.

## Bundled assets

| Asset | License | License text |
|---|---|---|
| Sarabun Regular/Bold (`assets/fonts/Sarabun-*.ttf`) | SIL Open Font License 1.1 | `assets/fonts/Sarabun-License.txt` |
| E1234 Regular (`assets/fonts/E1234-Regular.ttf`) | SIL Open Font License 1.1 | `assets/fonts/E1234-License.txt` |
| Lucide icon font (`assets/fonts/lucide.ttf`) | ISC | `assets/fonts/lucide-LICENSE.txt` |

The fonts are embedded in the executable. Modified versions must not use the
reserved font names; see the OFL text for details.

## Rust dependencies

The main dependencies and their upstream licenses are listed below. This table
is a convenience summary — use `Cargo.lock` and each crate's own metadata for
the exact version and license of a release.

| Crate | License |
|---|---|
| anyhow | MIT OR Apache-2.0 |
| clap | MIT OR Apache-2.0 |
| cpal | Apache-2.0 |
| directories | MIT OR Apache-2.0 |
| eframe / egui | MIT OR Apache-2.0 |
| encoding_rs | (Apache-2.0 OR MIT) AND BSD-3-Clause |
| flate2 | MIT OR Apache-2.0 |
| hound | Apache-2.0 |
| midly | MIT |
| rfd | MIT |
| roxmltree | MIT OR Apache-2.0 |
| rustysynth | MIT |
| serde / serde_json | MIT OR Apache-2.0 |
| winreg | MIT |
| windows-sys | MIT OR Apache-2.0 |

No SoundFont (`.sf2`) and no karaoke song or lyric is distributed with this
repository. MIDI files under `testdata/` were generated from scratch for the
test suite.
