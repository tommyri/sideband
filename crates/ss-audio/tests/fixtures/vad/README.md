# Voice activation fixture

`speech-keyboard.wav`: six seconds of mono 48 kHz PCM16. This is a small,
repeatable regression fixture, not a real-world speech/noise benchmark.

- 0–2 s: synthetic keyboard clicks only, one every 200 ms.
- 2–4 s: speech excerpt with the same clicks mixed in.
- 4–6 s: keyboard clicks only. Tests allow the configured 300 ms speech tail.

Speech: **Voices Obscured in Complex Environmental Settings (VOiCES)**,
SRI International and Lab41 / In-Q-Tel, recording
`Lab41-SRI-VOiCES-src-sp0307-ch127535-sg0042.wav`, accessed 2026-10-31.
Licensed under [Creative Commons Attribution 4.0 International](https://creativecommons.org/licenses/by/4.0/).
[Dataset and license](https://registry.opendata.aws/lab41-sri-voices/),
[source recording](https://download.pytorch.org/torchaudio/tutorial-assets/Lab41-SRI-VOiCES-src-sp0307-ch127535-sg0042.wav).
No endorsement by the original authors is implied.

Modifications: crop 0.6–2.6 s, linearly resample 16 to 48 kHz, scale the
original peak to 0.22, prepend/append two seconds, and mix generated clicks.
Clicks use seeded uniform noise with an exponential decay, not a recording
of someone's keyboard. `generate.py` reproduces the WAV using Python's
standard library and verifies the downloaded source's SHA-256.
The modified WAV is also provided under CC BY 4.0; code uses the repository
license. The original source need not be stored or downloaded to run tests.

Source SHA-256: `c65fcd726d6b08c82c1e5dc7558f863cd8d483e3ed2f4a7bcf271dc1865ada14`.
Fixture SHA-256: `1f0d589bcf4c84471ec7a81c27cc8e2d67b0db5afe5b6a7911719021ff029df2`.

At normal gain, the energy detector stays closed outside speech and its
hangover, and opens on the first speech quantum. At 4x gain, both detectors
receive identical PCM and default settings: the energy detector falsely
holds open for 370 keyboard-only quanta; RNNoise holds open for zero and
retains 198 of 200 speech quanta. Its opening is two quanta later, recovered
by the pre-roll. These numbers describe this fixture only. Loud keyboards,
breathing, music, and unfamiliar voices can still fool either detector.
