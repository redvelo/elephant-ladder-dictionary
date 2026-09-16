#!/usr/bin/env python3
"""Capture the current Kaikki extraction of one edition as immutable mirror assets.

Writes split archive parts, the information page, and a source snapshot record into a
new output directory. Nothing is uploaded; the snapshot workflow publishes the assets.
"""

import argparse
import datetime
import email.utils
import gzip
import hashlib
import html
import json
import re
import sys
import time
import urllib.parse
import urllib.request
from pathlib import Path

PART_BYTES = 1024 * 1024 * 1024
CHUNK_BYTES = 8 * 1024 * 1024
USER_AGENT = "elephant-ladder-dictionary-source-capture/1"
STATEMENT = re.compile(
    r"extracted on (\d{4}-\d{2}-\d{2}) from the (\w+) dump dated (\d{4}-\d{2}-\d{2})"
    r" using wiktextract \( ?([0-9a-f]+) and ([0-9a-f]+) ?\)"
)


def fail(message):
    print(f"error: {message}", file=sys.stderr)
    sys.exit(1)


def request(url, method="GET"):
    return urllib.request.Request(url, method=method, headers={"User-Agent": USER_AGENT})


def with_retries(action):
    for attempt in range(5):
        try:
            return action()
        except OSError as error:
            if attempt == 4:
                raise
            print(f"retrying after {error}", file=sys.stderr)
            time.sleep(10 * (attempt + 1))


def origin_headers(url):
    def head():
        with urllib.request.urlopen(request(url, "HEAD"), timeout=60) as response:
            return (
                response.headers.get("ETag"),
                response.headers.get("Last-Modified"),
                response.headers.get("Content-Length"),
            )

    return with_retries(head)


def download(url, path):
    def fetch():
        digest = hashlib.sha256()
        size = 0
        with urllib.request.urlopen(request(url), timeout=300) as response, path.open("wb") as file:
            while chunk := response.read(CHUNK_BYTES):
                file.write(chunk)
                digest.update(chunk)
                size += len(chunk)
        return size, digest.hexdigest()

    return with_retries(fetch)


def file_digest(path):
    digest = hashlib.sha256()
    with path.open("rb") as file:
        while chunk := file.read(CHUNK_BYTES):
            digest.update(chunk)
    return {"size_bytes": path.stat().st_size, "sha256": digest.hexdigest()}


def uncompressed_facts(path):
    digest = hashlib.sha256()
    size = 0
    lines = 0
    last = b"\n"
    with gzip.open(path, "rb") as file:
        while chunk := file.read(CHUNK_BYTES):
            digest.update(chunk)
            size += len(chunk)
            lines += chunk.count(b"\n")
            last = chunk[-1:]
    if last != b"\n":
        fail("uncompressed source does not end with a line feed")
    return {"size_bytes": size, "sha256": digest.hexdigest(), "line_count": lines}


def asset(path):
    return {"file_name": path.name, **file_digest(path)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--edition", required=True, type=Path)
    parser.add_argument("--mirror-repository", required=True)
    parser.add_argument("--output", required=True, type=Path)
    arguments = parser.parse_args()

    edition = json.loads(arguments.edition.read_text())
    origin = edition["origin"]
    name = edition["wiktionary_edition"]
    output = arguments.output
    output.mkdir(parents=True)
    captured_at = datetime.datetime.now(datetime.UTC).strftime("%Y-%m-%dT%H:%M:%SZ")

    before = origin_headers(origin["source_url"])
    archive = output / "source.jsonl.gz"
    size, sha256 = download(origin["source_url"], archive)
    after = origin_headers(origin["source_url"])
    if before[:2] != after[:2]:
        fail("the origin archive changed during capture; retry later")
    if before[2] is not None and int(before[2]) != size:
        fail("downloaded size differs from the origin Content-Length")

    info_path = output / "info.html"
    download(origin["info_url"], info_path)
    page = info_path.read_text()
    links = {
        urllib.parse.urljoin(origin["info_url"], html.unescape(href))
        for href in re.findall(r'href="([^"]*)"', page)
    }
    if origin["source_url"] not in links:
        fail("the information page does not link the configured source archive")
    text = " ".join(html.unescape(re.sub(r"<[^>]*>", " ", page)).split())
    match = STATEMENT.search(text)
    if match is None:
        fail("the information page has no recognizable extraction statement")
    extraction_date, dump_edition, dump_date, wiktextract, wikitextprocessor = match.groups()
    if dump_edition != name:
        fail(f"the information page describes `{dump_edition}`, not `{name}`")
    if before[1] is None:
        fail("the origin publishes no Last-Modified header to date the archive")
    modified = email.utils.parsedate_to_datetime(before[1]).date()
    age = (modified - datetime.date.fromisoformat(extraction_date)).days
    if age not in (0, 1):
        fail(
            f"the information page describes an extraction from {extraction_date}, but the "
            f"archive was modified {modified}; retry after Kaikki finishes publishing"
        )

    compressed = {"size_bytes": size, "sha256": sha256}
    stem = f"{name}-{dump_date.replace('-', '')}-{sha256[:12]}"
    info_mirror = output / f"{stem}.rawdata.html"
    info_path.rename(info_mirror)

    uncompressed = uncompressed_facts(archive)
    parts = []
    with archive.open("rb") as source:
        index = 0
        while chunk := source.read(PART_BYTES):
            part = output / f"{stem}.jsonl.gz.part{index:03}"
            part.write_bytes(chunk)
            parts.append(asset(part))
            index += 1
    archive.unlink()

    snapshot = {
        "schema_version": 1,
        "wiktionary_edition": name,
        "captured_at": captured_at,
        "origin": {
            "source_url": origin["source_url"],
            "info_url": origin["info_url"],
            "etag": before[0],
            "last_modified": before[1],
            "info_statement": match.group(0),
        },
        "dump_date": dump_date,
        "extraction_date": extraction_date,
        "wiktextract_revision": wiktextract,
        "wikitextprocessor_revision": wikitextprocessor,
        "compressed": compressed,
        "uncompressed": uncompressed,
        "mirror": {
            "repository": arguments.mirror_repository,
            "release_tag": f"source-{stem}",
            "parts": parts,
            "info_page": asset(info_mirror),
        },
    }
    snapshot_path = output / f"{stem}.json"
    snapshot_path.write_text(json.dumps(snapshot, ensure_ascii=False, indent=2) + "\n")
    print(f"snapshot={snapshot_path}")
    print(f"release_tag=source-{stem}")


if __name__ == "__main__":
    main()
