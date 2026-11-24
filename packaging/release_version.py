"""Prepare the workspace version for a release build."""

import argparse
import json
import os
import re
import subprocess
import tomllib
from pathlib import Path


VERSION = re.compile(r"v?(\d+)\.(\d+)\.(\d+)")
SOURCE_PREFIX = "Baudet release source "
SKIP_BRANCHES = ("norelease", "dependabot")


def run(*args):
    return subprocess.check_output(args, text=True).strip()


def skip_branch(branch):
    return any(branch == name or branch.startswith(f"{name}/") for name in SKIP_BRANCHES)


def eligible_commit(pulls):
    merged = [
        pr for pr in pulls
        if pr.get("merged_at") and pr.get("base", {}).get("ref") == "main"
    ]
    return not any(
        (pr.get("user") or {}).get("login") == "dependabot[bot]"
        or skip_branch((pr.get("head") or {}).get("ref") or "")
        for pr in merged
    )


def release_eligible(event, actor, associated_pulls):
    if event.get("ref") != "refs/heads/main" or actor == "dependabot[bot]" or event.get("deleted"):
        return False

    after = event["after"]
    before = event["before"]
    if not any(char != "0" for char in before):
        commits = [after]
    else:
        commits = run("git", "rev-list", "--first-parent", f"{before}..{after}").splitlines()

    return any(eligible_commit(associated_pulls(commit)) for commit in commits)


def associated_pulls(repo, commit):
    pages = json.loads(run("gh", "api", "--paginate", "--slurp", f"repos/{repo}/commits/{commit}/pulls"))
    return [pr for page in pages for pr in page]


def version_tuple(value):
    match = VERSION.fullmatch(value)
    return tuple(map(int, match.groups())) if match else None


def next_version(current, tags):
    base = version_tuple(current)
    if base is None:
        raise ValueError(f"Expected a three-part workspace version, got {current!r}")
    published = [version_tuple(tag) for tag in tags if tag.startswith("v")]
    latest = max((version for version in published if version is not None), default=None)
    if latest is None or base > latest:
        return current
    major, minor, patch = latest
    return f"{major}.{minor}.{patch + 1}"


def version_for_source(source, tags):
    for tag, subject in tags:
        if tag.startswith("v") and version_tuple(tag) and subject == SOURCE_PREFIX + source:
            return tag[1:]
    return None


def set_version(root, version):
    if not version_tuple(version) or version.startswith("v"):
        raise ValueError(f"Invalid workspace version: {version!r}")
    manifest = root / "Cargo.toml"
    lockfile = root / "Cargo.lock"
    manifest_text = manifest.read_text()
    workspace = tomllib.loads(manifest_text)["workspace"]
    previous = workspace["package"]["version"]
    names = {
        tomllib.loads((root / member / "Cargo.toml").read_text())["package"]["name"]
        for member in workspace["members"]
    }
    pattern = rf'(\[workspace\.package\]\nversion = "){re.escape(previous)}(")'
    manifest_text, changed = re.subn(pattern, rf'\g<1>{version}\2', manifest_text)
    if changed != 1:
        raise ValueError("Expected one workspace.package version")

    blocks = lockfile.read_text().split("[[package]]")
    updated = set()
    for index in range(1, len(blocks)):
        block = blocks[index]
        package = tomllib.loads("[[package]]" + block)["package"][0]
        name = package["name"]
        if name in names and "source" not in package:
            if package["version"] != previous or name in updated:
                raise ValueError(f"Unexpected lockfile version for {name}")
            blocks[index], changed = re.subn(r'^version = "[^"]+"$', f'version = "{version}"', block, count=1, flags=re.M)
            if changed != 1:
                raise ValueError(f"Missing lockfile version for {name}")
            updated.add(name)
    if updated != names:
        raise ValueError(f"Workspace packages missing from Cargo.lock: {sorted(names - updated)}")

    manifest.write_text(manifest_text)
    lockfile.write_text("[[package]]".join(blocks))


def prepare():
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    repo = os.environ["GITHUB_REPOSITORY"]
    if not release_eligible(event, os.environ["GITHUB_ACTOR"], lambda sha: associated_pulls(repo, sha)):
        return
    source = event["after"]
    tags = [
        tuple(line.split("\t", 1))
        for line in run("git", "for-each-ref", "--format=%(refname:short)%09%(contents:subject)", "refs/tags/v*").splitlines()
        if "\t" in line
    ]
    current = tomllib.loads(Path("Cargo.toml").read_text())["workspace"]["package"]["version"]
    version = version_for_source(source, tags) or next_version(current, [tag for tag, _ in tags])
    set_version(Path.cwd(), version)
    with Path(os.environ["GITHUB_OUTPUT"]).open("a") as output:
        print(f"version={version}", file=output)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("version", nargs="?")
    args = parser.parse_args()
    if args.version:
        set_version(Path.cwd(), args.version)
    else:
        prepare()
