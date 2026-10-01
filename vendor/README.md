# Vendored crates

## ironrdp-graphics

IronRDP's `crates/ironrdp-graphics` at rev `9b151c4c` (the rev `uwurdp-core`
pins), with one change: `src/srl.rs` decodes RemoteFX Progressive SRL
refinement streams the way Windows writes them and FreeRDP reads them
(`progressive_rfx_srl_read`). Windows ends the stream without a trailing zero
byte and cuts it off inside the last zero run; upstream rejects both, so the
tile stays at its blurry first pass (text looked pixelated). Bits past the end
now read as zero and zero runs are handed out one value at a time.

The manifest points the sibling crates at the same git rev instead of paths;
tests and dev-dependencies are dropped. Remove this copy and the `[patch]` in
the root `Cargo.toml` once upstream decodes Windows' streams.
