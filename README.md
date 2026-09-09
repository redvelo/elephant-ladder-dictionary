# Elephant Ladder Dictionary

Elephant Ladder Dictionary builds immutable offline dictionary packs from raw
Wiktextract JSONL and provides exact lookup for Elephant Ladder. The format and API
are experimental and may change without compatibility guarantees.

- `dictionary-pack` opens, validates, and queries packs.
- `dictionary-pack-builder` builds packs and provides the maintenance CLI.

Reader integration and pack acquisition are outside this repository.

## Build

Building requires Rust 1.88 or newer, a C compiler, a raw Wiktextract JSONL source,
and a matching build manifest. The CLI does not download source data.

```sh
gzip -dc SOURCE.jsonl.gz | \
  cargo run --release \
    -p elephant-ladder-dictionary-pack-builder \
    --bin elephant-dictionary-pack -- \
    build --manifest MANIFEST.json --input - --output PACK_DIRECTORY
```

The output directory must not already exist.

## Validate

```sh
cargo run --release \
  -p elephant-ladder-dictionary-pack-builder \
  --bin elephant-dictionary-pack -- \
  validate --pack PACK_DIRECTORY
```

Validation scans every record and lookup row. Pack integrity is only meaningful
relative to a manifest obtained through a trusted channel.

## Development

```sh
cargo fmt --all -- --check
cargo test --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

## License

Source code is available under Apache-2.0 or MIT. Dictionary data retains its source
licenses; see [Third-Party Notices](THIRD_PARTY_NOTICES.md).
