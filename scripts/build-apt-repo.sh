#!/bin/sh
# Builds a flat APT repo from .deb files. Signs Release if APT_REPO_GPG_KEY
# (armored private key) and APT_REPO_GPG_KEY_ID are set; otherwise the repo
# is left unsigned and needs [trusted=yes] in sources.list.
set -eu

DEB_DIR="${1:?usage: build-apt-repo.sh <deb-dir> <repo-out-dir>}"
OUT_DIR="${2:?usage: build-apt-repo.sh <deb-dir> <repo-out-dir>}"

mkdir -p "$OUT_DIR"
cp "$DEB_DIR"/*.deb "$OUT_DIR"/

cd "$OUT_DIR"
dpkg-scanpackages --multiversion . > Packages
gzip -9c Packages > Packages.gz

{
	echo "Origin: Tarno OS"
	echo "Label: Tarno OS"
	echo "Suite: tarno"
	echo "Codename: tarno"
	echo "Architectures: amd64"
	echo "Components: main"
	echo "Description: Tarno OS package repository"
	echo "Date: $(date -Ru)"
	echo "SHA256:"
	for f in Packages Packages.gz; do
		printf ' %s %d %s\n' "$(sha256sum "$f" | cut -d' ' -f1)" "$(wc -c < "$f")" "$f"
	done
} > Release

if [ -n "${APT_REPO_GPG_KEY:-}" ]; then
	: "${APT_REPO_GPG_KEY_ID:?APT_REPO_GPG_KEY_ID not set}"
	echo "$APT_REPO_GPG_KEY" | gpg --batch --import
	gpg --batch --yes --local-user "${APT_REPO_GPG_KEY_ID}" \
		--detach-sign --armor -o Release.gpg Release
	gpg --batch --yes --local-user "${APT_REPO_GPG_KEY_ID}" \
		--clearsign -o InRelease Release
else
	echo "APT_REPO_GPG_KEY not set, repo is unsigned - needs [trusted=yes]" >&2
fi
