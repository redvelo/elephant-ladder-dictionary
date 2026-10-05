# Elephant Ladder Dictionary

Elephant Ladder Dictionary builds immutable offline dictionary packs from raw
Wiktextract JSONL and provides exact lookup for Elephant Ladder. The format and API
are experimental and may change without compatibility guarantees.

- `dictionary-pack` opens, validates, and queries packs.
- `dictionary-pack-builder` builds packs and provides the maintenance CLI.

Reader integration and pack acquisition are outside this repository.

## Build

Building requires Rust 1.88 or newer, a C compiler, and Python 3.11 or newer for
source capture. The CLI does not download source data.

Packs are built only from immutable source snapshots; see
[Source Snapshots](docs/sources.md). To build locally from a snapshot's mirrored
archive:

```sh
elephant-dictionary-pack source manifest \
  --edition editions/EDITION.json --snapshot sources/SNAPSHOT.json \
  --builder-revision REVISION > build.json
gzip -dc SOURCE.jsonl.gz | \
  elephant-dictionary-pack build --manifest build.json --input - --output PACK_DIRECTORY
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
relative to a manifest obtained through a trusted channel, such as a signed catalog.

## Release Catalog

```sh
scripts/publish_catalog.py CATALOG_CONFIG.json
```

That assembles the release, checks every asset against the catalog, uploads the assets
under the tag the base URL names, and commits the catalog here, leaving only `git push`.
The assembly step alone is:

```sh
elephant-dictionary-pack catalog assemble --config CATALOG_CONFIG.json --output RELEASE_DIRECTORY
```

`CATALOG_CONFIG.json` names the catalog revision, generation time, release base URL,
and each built pack directory with its qualified views. A pack published by an earlier
release keeps its own `release_base_url` so its assets are referenced rather than
uploaded again. See the [format documentation](docs/format.md).

Pack assets are published as a release under the tag the base URL names. The catalog
is committed to this repository instead, and the application reads it from the default
branch over HTTPS. Assets need no trust of their own: the catalog declares each one's
SHA-256, and the application installs nothing that does not match.

## Development

```sh
cargo fmt --all -- --check
cargo test --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

## License

Source code is available under Apache-2.0 or MIT. Dictionary data retains its source
licenses; see [Third-Party Notices](THIRD_PARTY_NOTICES.md).
