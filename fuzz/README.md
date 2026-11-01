# Fuzzing

Five `cargo-fuzz` targets over the inputs an internet peer controls:

| Target | Input |
|---|---|
| `control_frames` | Length-prefixed control frames, both envelope directions, chunked reads |
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

`.github/workflows/fuzz.yml` runs every target for ten minutes each night
and uploads any reproducer. This is a separate workspace so libFuzzer and
sanitizer flags do not affect normal builds; its lockfile is managed
separately. Corpora, crash artifacts, coverage output, and targets are
ignored by git.
