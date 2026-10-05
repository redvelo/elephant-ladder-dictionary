#!/usr/bin/env python3
"""Publish one catalog revision: assemble, upload the assets, and commit the catalog.

The application reads `catalog-v1.json` from this repository's default branch, so a
release is only usable once both halves are done. Running them as one command is what
keeps a published release from being invisible to the application.
"""

import argparse
import hashlib
import json
import subprocess
import sys
from pathlib import Path

REPOSITORY = "redvelo/elephant-ladder-dictionary"
CHUNK_BYTES = 8 * 1024 * 1024
ATTEMPTS = 3


def fail(message):
    print(f"error: {message}", file=sys.stderr)
    sys.exit(1)


def run(arguments, **options):
    return subprocess.run(arguments, check=True, text=True, **options)


def tool(repository):
    run(
        [
            "cargo",
            "build",
            "--release",
            "-p",
            "elephant-ladder-dictionary-pack-builder",
            "--bin",
            "elephant-dictionary-pack",
        ],
        cwd=repository,
    )
    return repository / "target" / "release" / "elephant-dictionary-pack"


def assemble(builder, config, release):
    if release.exists():
        print(f"using the assembled release in {release}")
        return
    run(
        [
            builder,
            "catalog",
            "assemble",
            "--config",
            config.name,
            "--output",
            release.name,
        ],
        cwd=config.parent,
    )


def assets(catalog):
    """Every asset the catalog names, by file name, with the one tag they share."""
    tags, named = set(), {}
    for pack in catalog["packs"]:
        for asset in pack["release"]["assets"]:
            base, name = asset["url"].rsplit("/", 1)
            tags.add(base.rsplit("/", 1)[-1])
            named[name] = (asset["size_bytes"], asset["sha256"])
    if len(tags) != 1:
        fail(f"the catalog spans {len(tags)} release tags: {sorted(tags)}")
    return tags.pop(), named


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(CHUNK_BYTES):
            value.update(chunk)
    return value.hexdigest()


def verify(release, named):
    """Checks the assembled bytes against the catalog before anything is uploaded."""
    for index, (name, (size, sha256)) in enumerate(sorted(named.items()), 1):
        path = release / name
        if not path.is_file():
            fail(f"{name} is missing from {release}")
        if path.stat().st_size != size or digest(path) != sha256:
            fail(f"{name} does not match the catalog")
        print(f"{index}/{len(named)} verified {name}")


def published(tag):
    listed = subprocess.run(
        ["gh", "release", "view", tag, "-R", REPOSITORY, "--json", "assets"],
        capture_output=True,
        text=True,
    )
    if listed.returncode != 0:
        return None
    return {
        asset["name"]: asset["size"] for asset in json.loads(listed.stdout)["assets"]
    }


def upload(release, tag, revision, named):
    existing = published(tag)
    if existing is None:
        run(
            [
                "gh",
                "release",
                "create",
                tag,
                "-R",
                REPOSITORY,
                "--title",
                f"Packs {revision}",
                "--notes",
                f"Dictionary packs for catalog revision {revision}.",
            ]
        )
        existing = {}
    for index, (name, (size, _)) in enumerate(sorted(named.items()), 1):
        if existing.get(name) == size:
            print(f"{index}/{len(named)} present {name}")
            continue
        print(f"{index}/{len(named)} uploading {name} ({size} bytes)")
        for attempt in range(1, ATTEMPTS + 1):
            try:
                run(
                    ["gh", "release", "upload", tag, name, "-R", REPOSITORY, "--clobber"],
                    cwd=release,
                )
                break
            except subprocess.CalledProcessError:
                if attempt == ATTEMPTS:
                    fail(f"{name} could not be uploaded")
                print(f"retrying {name}", file=sys.stderr)


def commit(repository, release, revision):
    """Commits the catalog alone, so unrelated work in the tree stays out of it."""
    bytes_ = (release / "catalog-v1.json").read_bytes()
    head = subprocess.run(
        ["git", "show", "HEAD:catalog-v1.json"],
        cwd=repository,
        capture_output=True,
    )
    if head.returncode == 0 and head.stdout == bytes_:
        print("the committed catalog is already this revision")
        return
    (repository / "catalog-v1.json").write_bytes(bytes_)
    run(["git", "add", "catalog-v1.json"], cwd=repository)
    run(
        ["git", "commit", "-m", f"Publish catalog {revision}", "--", "catalog-v1.json"],
        cwd=repository,
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("config", type=Path, help="catalog configuration JSON")
    arguments = parser.parse_args()

    config = arguments.config.resolve()
    repository = Path(__file__).resolve().parent.parent
    revision = json.loads(config.read_text())["catalog_revision"]
    release = config.parent / f"release-{revision}"

    assemble(tool(repository), config, release)
    catalog = json.loads((release / "catalog-v1.json").read_text())
    if catalog["catalog_revision"] != revision:
        fail("the assembled catalog does not match the configured revision")
    tag, named = assets(catalog)
    verify(release, named)
    upload(release, tag, revision, named)
    commit(repository, release, revision)
    print(f"\npublished {revision} to {tag}; push to make it readable:\n  git push")


if __name__ == "__main__":
    main()
