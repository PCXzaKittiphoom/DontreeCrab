# Contributing

Thanks for helping with DontreeCrab.

## Before you start

- Use Rust stable (edition 2024).
- Do not add copyrighted karaoke files, lyrics, SoundFonts, screenshots with
  commercial lyrics, or other user-owned media.
- Test fixtures must be synthetic and safe to redistribute. Generate them with
  `scripts/make_testdata.py` or add an equally clearly synthetic file.

## Build and test

```sh
cargo build
cargo test
cargo clippy --all-targets
```

Audio tests need a SoundFont. Set `KARAOKE_SF2` or put any `.sf2` in
`soundfonts/`; the tests skip when none is available.

## Pull requests

- Keep changes focused and explain the user-visible behaviour.
- Add or update tests when changing parsing, timing or audio behaviour.
- Keep the existing code style and comments.
- Update `README.md` when a feature or setting changes.
- For translations, edit the `.po` files, not the compiled binary.

## Reporting bugs

Include:

- the DontreeCrab version or commit,
- the operating system and audio device,
- the steps to reproduce,
- the expected and actual result.

Do not attach copyrighted songs. A synthetic MIDI file or a short description
of the event timeline is enough.

## License

By contributing, you agree that your contribution is licensed under the MIT
License used by this project.
