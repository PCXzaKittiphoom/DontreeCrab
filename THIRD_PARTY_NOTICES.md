# Third-party notices

DontreeCrab itself is MIT-licensed. The assets and Rust crates below keep their
own licenses.

## Bundled assets

| Asset | License | License text |
|---|---|---|
| Sarabun Regular/Bold (`assets/fonts/Sarabun-*.ttf`) | SIL Open Font License 1.1 | `assets/fonts/Sarabun-License.txt` |
| E1234 Regular (`assets/fonts/E1234-Regular.ttf`) | SIL Open Font License 1.1 | `assets/fonts/E1234-License.txt` |
| Lucide icon font (`assets/fonts/lucide.ttf`) | ISC | `assets/fonts/lucide-LICENSE.txt` |
| egui default fonts (Hack, NotoEmoji, Ubuntu Light, emoji-icon, egui-icons) | MIT / OFL-1.1 / Ubuntu Font Licence 1.0 | `assets/fonts/egui/` |

The fonts are embedded in the executable. Modified versions must not use the
reserved font names; see the OFL and Ubuntu font licence texts for details.

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

### Transitive dependencies

The complete dependency tree contains about 300 crates. All are OSI-approved
open-source licenses; no paid license or proprietary SDK is required. Most are
MIT, Apache-2.0, BSD, ISC, Zlib or Unicode-3.0.

Notable licenses in the transitive tree:

| Crate | License | Note |
|---|---|---|
| option-ext | MPL-2.0 | Weak, file-level copyleft; used unmodified via `directories` |
| self_cell | Apache-2.0 OR GPL-2.0-only | The Apache-2.0 option is used |
| r-efi | MIT OR Apache-2.0 OR LGPL-2.1-or-later | A permissive option is used |
| libloading | ISC | Permissive |
| clipboard-win, error-code | BSL-1.0 | Permissive |
| tiny-skia, tiny-skia-path | BSD-3-Clause | Permissive |
| unicode-* data crates | Unicode-3.0 | Permissive data licence |

To generate a full report for a release:

```sh
cargo install cargo-about
cargo about generate about.hbs > THIRD_PARTY_LICENSES.html
```

or inspect each crate's metadata with `cargo metadata`. The short table above is
not a substitute for the license texts shipped by the crates.

No SoundFont (`.sf2`) and no karaoke song or lyric is distributed with this
repository. MIDI files under `testdata/` were generated from scratch for the
test suite.
