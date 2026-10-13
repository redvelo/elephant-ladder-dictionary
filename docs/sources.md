# Source Snapshots

Kaikki.org publishes each Wiktextract extraction at a stable URL and replaces it in
place, roughly weekly, without keeping older archives. A build that downloads from
Kaikki therefore stops reproducing as soon as the archive changes. Packs are built
only from snapshots mirrored into immutable releases of this repository.

## Files

- `editions/<edition>.json`: hand-maintained edition facts: pack ID, corpus language,
  Kaikki archive and information page URLs, shard target, minimum application
  version, license manifest, and the audio collection's pack ID and chunk count.
- A snapshot record, `<edition>-<dump>-<sha12>.json`, written by capture: capture time,
  origin ETag and Last-Modified, the information page statement, dump and extraction
  dates, published Wiktextract revisions, compressed and uncompressed digests, and the
  mirror release with its part and information page digests. Snapshot records live
  beside the captured data, not in this repository.

## Capturing a Snapshot

```sh
python3 scripts/capture_source.py --edition editions/EDITION.json \
  --mirror-repository redvelo/elephant-ladder-dictionary --output CAPTURE_DIRECTORY
```

Capture downloads the archive linked from the edition's information page and fails if
the ETag or Last-Modified header changes during the download. It requires the page to
link that exact archive and to describe an extraction dated within one day of the
archive's Last-Modified date; Kaikki dates extractions in its own time zone, ahead of
GMT, so a late upload can carry the next day's date. Otherwise the page and archive
describe different runs, and capture must be retried later. It records compressed
and uncompressed digests, size, and line count, and splits the archive into parts of
at most 1 GiB.

Check the record, then publish the parts and the information page as the release the
record names. An existing tag is never replaced.

```sh
elephant-dictionary-pack source validate --edition editions/EDITION.json --snapshot SNAPSHOT.json
gh release create "$(jq -r .mirror.release_tag SNAPSHOT.json)" CAPTURE_DIRECTORY/*.part* \
  CAPTURE_DIRECTORY/*.rawdata.html -R redvelo/elephant-ladder-dictionary
```

## Building From a Snapshot

```sh
cat CAPTURE_DIRECTORY/*.part* | gzip -dc | elephant-dictionary-pack build \
  --edition editions/EDITION.json --snapshot SNAPSHOT.json \
  --builder-revision "$(git rev-parse HEAD)" --input - --output PACK_DIRECTORY
elephant-dictionary-pack validate --pack PACK_DIRECTORY
```

The pack identity is derived from the edition, the snapshot, and the builder revision,
so the revision changes whenever any of them does. Building the same snapshot twice
with the same builder revision yields byte-identical packs.
