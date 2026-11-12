# Release guide

This repository is the **public** DontreeCrab tree. Day-to-day development
happens in a separate working copy that contains private test songs, SoundFonts
and screenshots. This guide explains how to update the public tree for a new
version without publishing those assets.

## What belongs here

Copy only code and redistributable assets:

- `src/`
- `assets/fonts/` (Sarabun, E1234, Lucide and their license files)
- `locales/`
- `logo/`
- `examples/`
- `tests/`
- `testdata/` (synthetic fixtures only)
- `scripts/`
- `Cargo.toml`, `Cargo.lock`, `build.rs`
- `README.md`, `LICENSE`, `THIRD_PARTY_NOTICES.md`, `CHANGELOG.md`,
  `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`, `RELEASE_GUIDE.md`
- `.github/`
- `.gitignore`

## Never add

- `MIDIDEMO/` or any other real karaoke song
- `MIDI (Copy Protection)/` or other protected files
- `Cap/` or screenshots containing commercial lyrics
- `soundfonts/`, `*.sf2`, `*.wav`
- `dist/`, `target/`, local build output
- editor/OS files and personal paths

Check with:

```sh
git ls-files | grep -i -E "MIDIDEMO|Copy Protection|Cap/|\\.sf2$|\\.wav$"
git grep -n -i -E "MIDIDEMO|Copy Protection|Scc1t2|Yiwza|Cap/"
```

If a copyrighted file was already committed, removing it in a new commit is not
enough — rewrite history before publishing, for example with
`git filter-repo --path MIDIDEMO --invert-paths`.

## Update steps

1. Sync the source and redistributable assets from the development tree into
   this repository. Keep the public `README.md` and `testdata/` here; do not
   overwrite them with development copies that mention private files.
2. If parsing or timing changed, regenerate the synthetic fixtures:

   ```sh
   python scripts/make_testdata.py
   ```

3. Search for private references:

   ```sh
   git ls-files | grep -i -E "MIDIDEMO|Copy Protection|Cap/|\\.sf2$|\\.wav$"
   git grep -n -i -E "MIDIDEMO|Copy Protection|Scc1t2|Yiwza|Cap/"
   ```

   Replace examples with `testdata/sample.kar` or
   `testdata/ncn/Song/1/test.mid`.

4. Format and test:

   ```sh
   cargo fmt --all
   cargo test
   cargo clippy --all-targets
   ```

   Audio tests skip when no `.sf2` is present. To run them locally, set
   `KARAOKE_SF2` to a SoundFont you own; never commit it.

5. Update `CHANGELOG.md` and the version in `Cargo.toml` (`Cargo.lock` too).
6. Update `README.md` if a feature, setting or format behaviour changed.
7. Confirm third-party assets and licenses are still listed in
   `THIRD_PARTY_NOTICES.md`, and that `assets/fonts/*.txt` are present.
8. Commit and push:

   ```sh
   git add -A
   git commit -m "Release vX.Y.Z"
   git tag vX.Y.Z
   git push origin main --tags
   ```

   The GitHub release workflow builds `dist/DontreeCrab-<version>.zip` and
   attaches it to the tag.

## Public release checklist

- [ ] No karaoke songs, lyrics, SoundFonts, or private screenshots
- [ ] `git grep` for private filenames is clean
- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo test` passes (audio tests may skip without an SF2)
- [ ] `README.md` uses only `testdata/` examples
- [ ] `CHANGELOG.md` and version numbers updated
- [ ] `LICENSE` and `THIRD_PARTY_NOTICES.md` present
- [ ] Font licenses present under `assets/fonts/`
- [ ] CI is green on Windows
- [ ] Tag and GitHub release created
