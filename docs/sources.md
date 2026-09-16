# Source Snapshots

Kaikki.org publishes each Wiktextract extraction at a stable URL and replaces it in
place, roughly weekly, without keeping older archives. A build that downloads from
Kaikki therefore stops reproducing as soon as the archive changes. Packs are built
only from snapshots mirrored into immutable releases.

## Files

- `editions/<edition>.json`: hand-maintained edition facts: pack ID, corpus language,
  Kaikki archive and information page URLs, shard target, minimum application
  version, and license manifest.
- `sources/<edition>-<dump>-<sha12>.json`: a generated snapshot record: capture time,
  origin ETag and Last-Modified, the information page statement, dump and extraction
  dates, published Wiktextract revisions, compressed and uncompressed digests, and the
  mirror release with its part and information page digests.

Build manifests are never edited by hand. `elephant-dictionary-pack source manifest`
derives one from an edition and a snapshot, including the source and license
manifest digests. The pack revision therefore changes whenever the source snapshot,
edition facts, or builder revision change.

## Capturing a Snapshot

Run the **Snapshot Source** workflow with an edition name. It:

1. Downloads the archive linked from the edition's information page, and fails if
   the ETag or Last-Modified header changes during the download.
2. Requires the information page to link that exact archive and to describe an
   extraction dated the same day as, or one day before, the archive's
   Last-Modified date. Otherwise the page and archive describe different runs, and
   capture must be retried later.
3. Records compressed and uncompressed digests, size, and line count.
4. Splits the archive into parts of at most 1 GiB.
5. Validates the snapshot and derives a build manifest.
6. Creates release `source-<edition>-<dump>-<sha12>` in the packs repository with the
   parts and the information page, then downloads and verifies every published asset.
   An existing tag fails the run; snapshots are never replaced.
7. Opens a pull request adding the snapshot record.

Capture can also run locally without publishing:

```sh
python3 scripts/capture_source.py --edition editions/EDITION.json \
  --mirror-repository OWNER/PACKS_REPOSITORY --output CAPTURE_DIRECTORY
```

## Qualifying a Pack

Run the **Qualify Pack** workflow with a committed snapshot path. It validates the
snapshot, downloads and verifies every part, reassembles and verifies the archive,
builds twice, validates, compares the builds, and uploads the pack and evidence.
Nothing contacts Kaikki.

## Repository Setup

- Create the public packs repository and enable release immutability.
- Set the repository variable `PACKS_REPOSITORY` to `owner/name`.
- Create the `source-snapshot` environment with required reviewers and the secret
  `PACKS_RELEASE_TOKEN`: a fine-grained token limited to the packs repository with
  contents write permission.
- Allow GitHub Actions to create pull requests in this repository.
