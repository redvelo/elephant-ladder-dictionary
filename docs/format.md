# Pack Format

Format version 1. Experimental; no compatibility guarantee.

## Pack Directory

```text
pack-manifest-v1.json
<pack-id>-<revision>-index.eldict
<pack-id>-<revision>-data-000.eldict
...
build-report-v1.json
```

- `pack-manifest-v1.json` lists every index and data asset with role, size, and
  SHA-256, plus the pack identity and selected-record totals.
- The index asset is an SQLite database (`application_id` `ELDI`) holding pack
  metadata, data-shard routes, entries, lookup keys, and source shape observations.
- Each data asset is an SQLite database (`application_id` `ELDD`) holding
  independently Zstandard-compressed original record bytes.
- `build-report-v1.json` is qualification evidence and is never read at runtime.

Schemas are in `crates/dictionary-pack/src/schema`. Databases use a 4096-byte page
size, UTF-8, `user_version` 1, and `STRICT` tables.

## Identity

- `PackId`: lowercase ASCII letters, digits, and single hyphens, at most 64 bytes.
- Pack revision: domain-separated SHA-256 over every metadata input, the format
  version, and the Unicode profile. Because the builder revision is an input, packs
  are byte-identical only when built by the same builder revision.
- Entry ID: domain-separated SHA-256 over pack revision, source line number, and
  record SHA-256.
- Snapshot revision: domain-separated SHA-256 over ordered view identities.

## Admission

| Operation | Checks |
| --- | --- |
| `DictionaryPack::open` | Manifest shape, every asset byte digest, database identity, exact schema, `integrity_check`, metadata cross-checks, foreign keys, entry and shard aggregates. Trust is relative to the pack's own manifest. |
| `DictionaryPack::verify` | `open`, plus manifest bytes, pack ID, and revision equal to a trusted `ExpectedPack`. Use once before installation. |
| `DictionaryPack::open_installed` | Manifest bytes and identity equal to `ExpectedPack`, asset sizes, database identity, exact schema, metadata cross-checks, shard metadata rows. No byte hashing, `integrity_check`, or table scans. |
| `DictionaryPack::validate_all` | Decompresses, hashes, and projects every record exhaustively; recomputes stream and shard digests; matches every lookup key row. |

Every record served by lookup, entry, or section reads is decompressed within
limits, hashed, and matched to its entry ID and index route, in every admission mode.

## Lookup

Lookup runs six exact stages and stops at the first stage with results:

1. Authored headword.
2. NFC headword.
3. NFC with full default case folding, renormalized to NFC.
4. Authored form.
5. NFC form.
6. Folded form.

Forms are authored `forms[].form` values other than `""` and `"-"`. Results keep
source order. `truncated` reports more rows in the returning stage.

## Projection

Records are projected at read time into typed values; unknown fields remain only in
the stored record. `ProjectionOptions` bounds text bytes and the item count of every
list. A zero count reports a list's total without its items.

- `ProjectionOptions::summary()` is used for lookup results.
- `ProjectionOptions::detail()` is used for entry reads.
- `ProjectionOptions::exhaustive()` is used by `validate_all`.

Every list is a `Bounded<T>` window with `offset` and `total`. `section_page` returns
one window of one list, including per-sense examples, translations, and relations.
Descendants are flattened depth-first with a depth.

Known shapes from the English, French, German, Spanish, Italian, Japanese, and
Chinese extractors are projected, including `etymology_text` and `etymology_texts`,
integer or string sense indexes, ruby pairs, emphasis offsets in Unicode scalar
values, and legacy `english`, `code`, and `zh_pron` fields. A known field with an
incompatible type fails the read as corrupt.

Bilingual views filter translations by exact `lang_code` (or legacy `code`) at entry
and sense level. `sense_translation_total` counts matching sense translations across
all senses, so a view can distinguish "no entry" from "no translation."

## Catalog

A release publishes `catalog-v1.json` and `catalog-v1.json.sig`.

- The signature is a lowercase hexadecimal Ed25519 signature over the exact catalog
  bytes, optionally followed by one LF.
- `verify_catalog` checks size, signature against trusted keys, then parses with
  unknown-field rejection and validates identities, asset names, sizes, URLs, views,
  and licenses.
- Each pack lists every installed file, including `pack-manifest-v1.json`, with its
  release URL. Manifests are published as `<pack-id>-<revision>-pack-manifest-v1.json`
  so release asset names stay unique.
- `CatalogPack::expected_pack` supplies the `ExpectedPack` used by `verify` and
  `open_installed` for corpus packs and audio collections.

`elephant-dictionary-pack catalog assemble` verifies built packs and writes a release
directory; `catalog sign` signs it; `catalog keygen` creates an owner-only signing
key.

## Audio Collections

An audio collection holds pronunciation recordings for one corpus language, keyed by
normalized Wikimedia Commons file name and usable with any corpus revision.

```text
audio-manifest-v1.json
<pack-id>-<revision>-index.eldict
<pack-id>-chunk-<NNN>-<sha256 prefix>.eldict
...
```

- The index (`application_id` `ELAI`) holds collection metadata, chunk routes,
  licenses, and one row per referenced file name with its status (`available`,
  `unqualified`, `missing`, or `failed`), reason, Commons facts (title, SHA-1, revision
  timestamp, media type, size, description page), license, plain-text author and
  linked pages, attribution requirement, and, when available, the Opus blob digest,
  size, duration, and chunk.
- Chunks (`application_id` `ELAC`) hold Opus blobs keyed by SHA-256. A recording
  always lives in chunk `sha256(file_name) mod chunk_count`, and identical blobs are
  deduplicated within a chunk. Chunk metadata excludes the collection revision and
  chunk names are content addressed, so unchanged chunks keep their bytes and names
  across revisions.
- Every recording is Ogg Opus, mono, 48 kHz, 24 kbps VBR, speech application, 20 ms
  frames (`ogg-opus-v1-mono-48khz-24kbps-voip-20ms`). The stream serial derives from
  the source SHA-1, and the final granule trims padding to the exact source length.
- The collection revision derives from the pack ID, corpus language, source corpus
  identity, acquisition completion time, encoder profile, builder revision, chunk
  count, minimum application version, and a digest of every recording's acquired
  facts.

`AudioCollection::verify` and `open_installed` mirror corpus admission against an
`ExpectedPack` whose manifest digest is that of `audio-manifest-v1.json`. `read`
verifies each blob's SHA-256; `validate_all` also checks chunk assignment, Ogg framing,
`OpusHead`, `OpusTags`, durations, totals, and the recording input digest.

### Pipeline

1. `audio references --pack CORPUS` writes every normalized file name referenced by
   `sounds[].audio`, with counts, and lists authored values that are not file names.
2. `audio acquire --references REFS --state DIRECTORY --user-agent AGENT` resolves
   files through the Commons API in batches of 50 with `maxlag`, follows normalized
   titles and redirects, downloads originals with bounded concurrency and
   `Retry-After` handling, verifies SHA-1, and persists progress in
   `acquisition.sqlite`. Rerunning resumes and retries failed files up to three
   attempts.
3. `audio build --state DIRECTORY --edition EDITION --builder-revision REVISION
   --output COLLECTION` classifies licenses (CC0, public domain, CC BY, CC BY-SA, GFDL;
   anything with Commons restrictions is unqualified), converts authors to text, and
   transcodes in parallel. Lingua Libre WAV files, which declare a RIFF length four
   bytes short, are corrected in memory before decoding.
4. `audio validate --collection COLLECTION` performs full admission and validation.

`audio transcode --input SOURCE --output RECORDING` transcodes one file for inspection.
