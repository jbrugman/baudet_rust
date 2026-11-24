#!/usr/bin/env bash
set -euo pipefail

tag="v${VERSION:?Missing release version}"
source_sha="${SOURCE_SHA:?Missing release source}"
subject="Baudet release source $source_sha"
assets=(
    release-assets/Baudet-macOS-universal.dmg
    release-assets/Baudet-windows-x64.zip
    "release-assets/baudet_${VERSION}_amd64.deb"
)
for asset in "${assets[@]}"; do
    test -s "$asset"
done

git config user.name 'github-actions[bot]'
git config user.email '41898282+github-actions[bot]@users.noreply.github.com'
git fetch origin refs/heads/main:refs/remotes/origin/main --tags

if ! git show-ref --verify --quiet "refs/tags/$tag"; then
    # The tag must contain the exact source and manifests used for all three packages.
    if ! git diff --quiet HEAD -- Cargo.toml Cargo.lock; then
        git add -- Cargo.toml Cargo.lock
        git commit -m "Release $tag"
    fi
    release_commit="$(git rev-parse HEAD)"
    git tag -a "$tag" -m "$subject" "$release_commit"

    version_script="$(mktemp)"
    trap 'rm -f "$version_script"' EXIT
    cp packaging/release_version.py "$version_script"
    for attempt in 1 2 3; do
        git fetch origin refs/heads/main:refs/remotes/origin/main
        git merge-base --is-ancestor "$source_sha" origin/main
        if [[ "$(git rev-parse origin/main)" == "$source_sha" ]]; then
            git switch --detach "$release_commit"
        else
            # When main advanced during packaging, update its version without dropping the new commits.
            git switch --detach origin/main
            python3 "$version_script" "$VERSION"
            if ! git diff --quiet HEAD -- Cargo.toml Cargo.lock; then
                git add -- Cargo.toml Cargo.lock
                git commit -m "Release $tag"
            fi
        fi
        if git push --atomic origin HEAD:refs/heads/main "refs/tags/$tag"; then
            break
        fi
        if [[ "$attempt" == 3 ]]; then
            exit 1
        fi
    done
fi

test "$(git for-each-ref --format='%(contents:subject)' "refs/tags/$tag")" = "$subject"
if draft="$(gh release view "$tag" --json isDraft --jq .isDraft 2>/dev/null)"; then
    if [[ "$draft" == true ]]; then
        gh release upload "$tag" "${assets[@]}" --clobber
        gh release edit "$tag" --draft=false
    fi
else
    gh release create "$tag" "${assets[@]}" --verify-tag --title "Baudet $VERSION" --generate-notes
fi
