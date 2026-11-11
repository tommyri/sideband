# Fuzzing

Seven `cargo-fuzz` targets over the inputs an internet peer controls:

| Target | Input |
|---|---|
| `control_frames` | One control frame, both envelope directions: protobuf decode and round trip |
| `control_stream` | A control stream in arbitrary chunks through `ss_net::control::FrameDecoder` |
| `video_group` | A video group stream in arbitrary chunks through `ss_net::media::stream::GroupParser` |
| `media_headers` | Audio datagram and video stream headers; accepted input re-encodes byte-for-byte |
| `mentions` | Message bodies through the mention parser and preview URL extractor |
| `preview_html` | Fetched HTML (and oEmbed JSON after a NUL byte) through the preview scanner |
| `multipart_upload` | `POST /api/v1/attachments` bodies through the multipart reader |

The server stores raw message text, so there is no Markdown parser to fuzz.
Server-side parsers are reached through `ss_server::fuzzing`, which is not a
stable API.

Run from the repository root with nightly Rust and `cargo install cargo-fuzz`:

```sh
cargo +nightly fuzz run preview_html -- -max_total_time=600 -timeout=10 -rss_limit_mb=2048 -max_len=8192
```

`.github/workflows/fuzz.yml` runs every target for ten minutes on manual
dispatch and uploads any reproducer. This is a separate workspace so
libFuzzer and sanitizer flags do not affect normal builds. Corpora, crash
artifacts, coverage output, and targets are ignored by git.

`fuzz/Cargo.lock` is its own lockfile. The workflow runs
`cargo fetch --locked` first (cargo-fuzz 0.13 has no `--locked`) and fails if
the run changed the lockfile. When the workspace dependencies change, refresh
it from the workspace lockfile so shared crates keep the same versions:

```sh
cp Cargo.lock fuzz/Cargo.lock
cargo +nightly update --workspace --manifest-path fuzz/Cargo.toml
cargo +nightly fetch --locked --manifest-path fuzz/Cargo.toml
```
