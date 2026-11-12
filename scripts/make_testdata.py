from pathlib import Path
import struct

ROOT = Path(__file__).resolve().parent.parent


def vlq(n: int) -> bytes:
    out = [n & 0x7F]
    n >>= 7
    while n:
        out.append((n & 0x7F) | 0x80)
        n >>= 7
    return bytes(reversed(out))


def meta(kind: int, data: bytes) -> bytes:
    return bytes([0xFF, kind, len(data)]) + data


def text(s: str) -> bytes:
    return meta(0x01, s.encode("ascii"))


def lyric(s: str) -> bytes:
    return meta(0x05, s.encode("ascii"))


def write_midi(path: Path, events, division=480):
    events = sorted(events, key=lambda e: (e[0], e[1]))
    track = bytearray()
    last = 0
    for tick, _order, data in events:
        track += vlq(tick - last)
        track += data
        last = tick
    track += vlq(0) + meta(0x2F, b"")
    header = b"MThd" + struct.pack(">IHHH", 6, 0, 1, division)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(header + b"MTrk" + struct.pack(">I", len(track)) + bytes(track))


# ---------------------------------------------------------------- KAR sample
events = []
order = 0


def add(tick, data):
    global order
    events.append((tick, order, data))
    order += 1


add(0, meta(0x51, bytes([0x07, 0xA1, 0x20])))  # 120 BPM
add(0, meta(0x58, bytes([4, 2, 24, 8])))  # 4/4
add(0, text("@KMIDI KARAOKE FILE"))
add(0, text("@T Test Song"))
add(0, text("@I DontreeCrab Tests"))
add(0, text("@L EN"))
for tick, word in [
    (0, "Hel/"),
    (480, "lo/"),
    (960, " world/"),
    (1920, "This/"),
    (2400, " is/"),
    (2880, " a/"),
    (3360, " test/"),
    (7680, "Second/"),
    (8160, " verse/"),
]:
    add(tick, lyric(word))

add(0, bytes([0xC8, 73]))  # melody program
add(0, bytes([0xC0, 0]))  # accompaniment program
for tick in range(0, 38400, 960):
    add(tick, bytes([0x98, 60, 100]))
    add(tick + 480, bytes([0x88, 60, 0]))
for tick in range(0, 38400, 1920):
    for key in (48, 52, 55):
        add(tick, bytes([0x90, key, 72]))
        add(tick + 960, bytes([0x80, key, 0]))
write_midi(ROOT / "testdata" / "sample.kar", events)

# -------------------------------------------------------------- plain sample
events = []
order = 0
add(0, meta(0x51, bytes([0x07, 0xA1, 0x20])))
add(0, meta(0x58, bytes([4, 2, 24, 8])))
add(0, bytes([0xC0, 0]))
add(0, bytes([0x90, 60, 100]))
add(960, bytes([0x80, 60, 0]))
write_midi(ROOT / "testdata" / "plain.mid", events)

# ----------------------------------------------------------------- NCN sample
events = []
order = 0
add(0, meta(0x51, bytes([0x07, 0xA1, 0x20])))
add(0, meta(0x58, bytes([4, 2, 24, 8])))
add(0, bytes([0xC0, 0]))
for tick in range(0, 9600, 960):
    add(tick, bytes([0x90, 60, 100]))
    add(tick + 480, bytes([0x80, 60, 0]))
write_midi(ROOT / "testdata" / "ncn" / "Song" / "1" / "test.mid", events)
(ROOT / "testdata" / "ncn" / "Lyrics" / "1").mkdir(parents=True, exist_ok=True)
(ROOT / "testdata" / "ncn" / "Lyrics" / "1" / "test.lyr").write_text(
    "Test Song\nDontreeCrab\nC\n\nhello\nworld\n", encoding="utf-8"
)
cursor = bytearray()
for index in range(24):
    cursor += struct.pack("<H", index * 20)
cursor += struct.pack("<H", 0xFFFF)
(ROOT / "testdata" / "ncn" / "Cursor" / "1").mkdir(parents=True, exist_ok=True)
(ROOT / "testdata" / "ncn" / "Cursor" / "1" / "test.cur").write_bytes(bytes(cursor))

print("wrote synthetic test fixtures")
