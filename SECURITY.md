# Security policy

## Reporting a vulnerability

Please report security issues privately to the maintainers instead of opening a
public issue. Include the version, operating system, and a minimal reproduction.

There is no network service or telemetry in DontreeCrab. The main attack surface
is parsing untrusted MIDI/karaoke files and reading user-selected SoundFonts.
Reports about crashes, excessive memory use, or unsafe handling of malformed
files are welcome.

Do not include copyrighted songs or third-party SoundFonts in a report; a
synthetic file or a byte-level description is enough.

## Supported versions

Security fixes are applied to the latest release on the default branch.
