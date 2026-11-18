# Vendored crates

## ironrdp-pdu

`ironrdp-pdu` 0.11.0 as published on crates.io (the version every IronRDP crate
`uwurdp-core` uses), with one change: `src/rdp/server_error_info.rs` decodes
a Set Error Info PDU with a code it doesn't know as `ErrorInfo::Unknown(code)`.
Upstream fails the whole PDU with "unexpected info code", which ended
connections to servers that send codes MS-RDPBCGR doesn't list; now the
connection goes on (or ends with the code in its message, if it isn't zero).

Tests and dev-dependencies are dropped. Remove this copy and the
`[patch.crates-io]` in the root `Cargo.toml` once upstream accepts unknown
codes.

A workspace that uses `uwurdp-core` as a git dependency gets the same
behaviour with the same patch, pointed at this repository:

```toml
[patch.crates-io]
ironrdp-pdu = { git = "https://github.com/MinifyX/UwURDP-Client", rev = "<rev>" }
```

Without it everything builds against the plain crates.io release; only
connections to servers sending unlisted codes end early.

## ironrdp-graphics (removed)

Until 0.2.0-beta.5 this held IronRDP's `ironrdp-graphics` at master rev
`9b151c4c` with Windows' Progressive SRL streams decoded. `ironrdp-graphics`
0.11 has that fix (IronRDP #2134) and the Progressive/ClearCodec ones, so the
copy and the pinned git dependency are gone.
