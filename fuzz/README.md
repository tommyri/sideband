# Media header fuzzing

`media_headers` parses arbitrary audio datagrams and video stream prefixes.
Accepted input must re-encode byte-for-byte, including the borrowed payload.

Run from the repository root with nightly Rust and `cargo-fuzz` installed:

```sh
cargo +nightly fuzz run media_headers -- -max_len=2048 -max_total_time=30
```

This is a separate workspace so libFuzzer and sanitizer flags do not affect
normal builds. Its dependency manifest and lockfile are managed separately.
Corpora, crash artifacts, coverage output, and targets are ignored. T-061
adds sustained fuzzing and nightly CI; this target is ready to use now.
