# Elephant Ladder Dictionary

The dictionaries inside [Elephant Ladder](https://github.com/redvelo/elephant-ladder):
the pack format, the tools that build packs from Wiktionary, and the catalog the
application installs them from. Readers never see this repository; they install a
language from the application's Dictionaries dialog and look words up offline.

Each pack is one language's Wiktionary edition, extracted by
[Wiktextract](https://github.com/tatuylonen/wiktextract) and published by
[Kaikki](https://kaikki.org), kept whole: every selected record is stored byte for
byte, so a later version of the reader can show more of it without rebuilding the pack.
Pronunciation recordings come from Wikimedia Commons as separate audio collections, either
bundled as Ogg Opus for offline use or fetched on demand.

Seven languages are published: English, French, German, Spanish, Italian, Japanese, and
Chinese, each with a monolingual view and a bilingual view to or from English.

## What is here

- `dictionary-pack`: opens, verifies, and queries packs and audio collections. Lookup is
  exact, in six stages from the authored headword down to a case-folded inflected form,
  and every record served is re-verified against its digest.
- `dictionary-pack-builder`: the `elephant-dictionary-pack` command line: build,
  validate, acquire and transcode audio, and assemble a catalog.
- `editions/`: one file per Wiktionary edition with its pack identity and source URLs.
- `catalog-v1.json`: the published catalog. The application reads it from this
  repository's default branch over HTTPS and accepts each asset only on the SHA-256 the
  catalog declares.

## Build

The Rust toolchain pinned in `rust-toolchain.toml`, a C compiler and CMake for libopus
and SQLite, and Python 3.11 or newer for source capture. The CLI never downloads source
data: packs are built only from snapshots mirrored into immutable releases of this
repository, so a build reproduces after Kaikki replaces its archive.

```sh
gzip -dc SOURCE.jsonl.gz | elephant-dictionary-pack build \
  --edition editions/EDITION.json --snapshot SNAPSHOT.json \
  --builder-revision REVISION --input - --output PACK_DIRECTORY
elephant-dictionary-pack validate --pack PACK_DIRECTORY
```

The output directory must not already exist. Validation scans every record and lookup
row; building the same snapshot twice with the same builder revision yields identical
packs.

## Publish

```sh
scripts/publish_catalog.py CATALOG_CONFIG.json
```

The config names the catalog revision, the release base URL, each built pack directory
with its views, and each audio collection. The script assembles the release, checks
every asset against the catalog, uploads the assets under the tag the base URL names,
and commits the catalog, leaving only `git push`. Publishing new packs needs no
application release. Releases in this repository are pack releases only; the code is
not released here.

## Development

```sh
cargo fmt --all -- --check
cargo test --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

## License

Source code is Apache-2.0 or MIT, at your option. Dictionary data keeps the license of
its Wiktionary edition, CC BY-SA and GFDL, and each recording keeps its own Commons
license, carried inside the pack with its author; see
[Third-Party Notices](THIRD_PARTY_NOTICES.md) for the fixtures in this repository.
