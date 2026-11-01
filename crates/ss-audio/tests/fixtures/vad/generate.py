"""Rebuild speech-keyboard.wav from the source listed in README.md.

Usage: python3 generate.py /path/to/Lab41-SRI-VOiCES-src-sp0307-ch127535-sg0042.wav
"""

import hashlib
import math
import struct
import sys
import wave
from pathlib import Path

source = Path(sys.argv[1])
assert hashlib.sha256(source.read_bytes()).hexdigest() == (
    "c65fcd726d6b08c82c1e5dc7558f863cd8d483e3ed2f4a7bcf271dc1865ada14"
)
output = Path(__file__).with_name("speech-keyboard.wav")
with wave.open(str(source)) as recording:
    values = struct.unpack(
        f"<{recording.getnframes()}h", recording.readframes(recording.getnframes())
    )

# 2 s keyboard, 2 s speech with typing, then 2 s keyboard again.
rate = 48000
pcm = [0.0] * (rate * 6)
state = 0x052
for start in range(0, len(pcm), rate // 5):
    for j in range(480):
        state = (1664525 * state + 1013904223) & 0xFFFFFFFF
        pcm[start + j] = 0.025 * (state / 2147483648 - 1) * math.exp(-j / 80)

# Crop at 0.6 s; linear interpolation from 16 to 48 kHz.
start = 9600
scale = 0.22 / max(abs(v) for v in values)
for i in range(rate * 2):
    index = start + i // 3
    fraction = (i % 3) / 3
    pcm[rate * 2 + i] += scale * (
        values[index] * (1 - fraction) + values[index + 1] * fraction
    )
with wave.open(str(output), "wb") as recording:
    recording.setparams((1, 2, rate, len(pcm), "NONE", "not compressed"))
    recording.writeframes(
        struct.pack(f"<{len(pcm)}h", *(round(v * 32767) for v in pcm))
    )
print("fixture sha256", hashlib.sha256(output.read_bytes()).hexdigest())
