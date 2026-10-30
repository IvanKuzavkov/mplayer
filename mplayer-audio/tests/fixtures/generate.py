"""Generate original, deterministic test audio using only Python's standard library.

FLAC uses RFC 9639 verbatim subframes (no encoder executable is needed).
MP3 contains independent MPEG-1 Layer III silent frames with zero spectral data.
These generated fixtures contain no third-party recordings.
"""

import hashlib
import math
from pathlib import Path

ROOT = Path(__file__).resolve().parent
RATE = 44100
FRAMES = 4410


def crc(data, width, polynomial):
    value = 0
    mask = (1 << width) - 1
    for byte in data:
        value ^= byte << (width - 8)
        for _ in range(8):
            value = ((value << 1) ^ (polynomial if value & (1 << (width - 1)) else 0)) & mask
    return value


for bits in (16, 24):
    size = bits // 8
    samples = [round(math.sin(2 * math.pi * 440 * i / RATE) * ((1 << (bits - 1)) - 1) / 4) for i in range(FRAMES)]
    pcm = b"".join(value.to_bytes(size, "little", signed=True) * 2 for value in samples)
    streaminfo = (4096).to_bytes(2, "big") * 2 + bytes(6)
    streaminfo += ((RATE << 44) | (1 << 41) | ((bits - 1) << 36) | FRAMES).to_bytes(8, "big")
    streaminfo += hashlib.md5(pcm).digest()
    encoded = bytearray(b"fLaC\x80\x00\x00\x22" + streaminfo)
    for number, start in enumerate(range(0, FRAMES, 4096)):
        block = samples[start:start + 4096]
        header = bytearray([0xFF, 0xF8, 0x79, 0x10 | ({16: 4, 24: 6}[bits] << 1), number])
        header += (len(block) - 1).to_bytes(2, "big")
        header.append(crc(header, 8, 0x07))
        subframe = b"\x02" + b"".join(value.to_bytes(size, "big", signed=True) for value in block)
        frame = header + subframe * 2
        encoded += frame + crc(frame, 16, 0x8005).to_bytes(2, "big")
    (ROOT / f"tone{bits}.flac").write_bytes(encoded)

(ROOT / "silence.mp3").write_bytes((b"\xff\xfb\x90\x00" + bytes(413)) * 20)


class Bits:
    """Vorbis packets encode fields least-significant bit first."""
    def __init__(self):
        self.bits = []

    def put(self, value, count):
        self.bits.extend((value >> bit) & 1 for bit in range(count))

    def bytes(self):
        return bytes(sum(bit << offset for offset, bit in enumerate(self.bits[start:start + 8]))
                     for start in range(0, len(self.bits), 8))


# Minimal valid Vorbis setup: one codebook, floor1, empty residue, and one mode.
# Audio packets have absent floors on both channels, representing exact silence.
setup = Bits()
for value, count in [
    (0, 8), (0x564342, 24), (1, 16), (1, 24), (0, 1), (0, 1), (0, 5), (0, 4),
    (0, 6), (0, 16),
    (0, 6), (1, 16), (0, 5), (0, 2), (1, 4),
    (0, 6), (0, 16), (0, 24), (0, 24), (0, 24), (0, 6), (0, 8), (0, 3), (0, 1),
    (0, 6), (0, 16), (0, 1), (0, 1), (0, 2), (0, 8), (0, 8), (0, 8),
    (0, 6), (0, 1), (0, 16), (0, 16), (0, 8), (1, 1),
]:
    setup.put(value, count)
identification = b"\x01vorbis" + bytes(4) + b"\x02" + RATE.to_bytes(4, "little") + bytes(12) + b"\x66\x01"
comment = b"\x03vorbis" + bytes(8) + b"\x01"
configuration = b"\x05vorbis" + setup.bytes()


def ogg_page(sequence, flags, granule, packets):
    assert all(len(packet) < 255 for packet in packets)
    header = bytearray(b"OggS\x00" + bytes([flags]) + granule.to_bytes(8, "little")
                       + (1).to_bytes(4, "little") + sequence.to_bytes(4, "little")
                       + bytes(4) + bytes([len(packets)]) + bytes(map(len, packets)))
    page = header + b"".join(packets)
    page[22:26] = crc(page, 32, 0x04C11DB7).to_bytes(4, "little")
    return page


vorbis = ogg_page(0, 2, 0, [identification])
vorbis += ogg_page(1, 0, 0, [comment, configuration])
vorbis += ogg_page(2, 4, 320, [b"\x00"] * 11)
(ROOT / "silence.ogg").write_bytes(vorbis)
