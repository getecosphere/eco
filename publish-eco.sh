#!/usr/bin/env bash
# publish-eco.sh — build the latest eco from main and release it so the public
# installer serves it:
#
#   curl -fsSL https://getecosphere.com/install.sh | sh
#
# Pipeline: fetch eco main -> ./build-release.sh (all targets) -> copy
# dist/<triple>/eco into <getecosphere_composition>/frontend/downloads/ ->
# commit on the current branch -> merge the feature branch into main and push
# (deploying the estate, which serves the new binaries).
#
# The feature->main merge is a prod release, so it prompts for confirmation
# first unless run with -y.
#
# Usage:
#   ./publish-eco.sh            # build + publish + commit; prompts before merge
#   ./publish-eco.sh -y         # build + publish + commit + merge to main + push
#   ./publish-eco.sh --no-push  # build + copy only (no commit, no merge)
#
# Env overrides:
#   GETECOSPHERE_COMPOSITION  path to the getecosphere_composition repo
#   ECO_PUBLISH_BRANCH        eco branch to build from (default: main)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ECO_BRANCH="${ECO_PUBLISH_BRANCH:-main}"
ASSUME_YES=0
PUSH=1
for arg in "$@"; do
  case "$arg" in
    -y|--yes) ASSUME_YES=1 ;;
    --no-push) PUSH=0 ;;
  esac
done

# ---- locate the getecosphere composition repo --------------------------
GECO="${GETECOSPHERE_COMPOSITION:-}"
if [ -z "$GECO" ]; then
  for candidate in \
    "$SCRIPT_DIR/../getecosphere/getecosphere_composition" \
    "$SCRIPT_DIR/../../getecosphere/getecosphere_composition"; do
    if [ -d "$candidate/frontend/downloads" ]; then
      GECO="$candidate"
      break
    fi
  done
fi
if [ -z "$GECO" ] || [ ! -d "$GECO/frontend/downloads" ]; then
  echo "publish-eco: cannot find getecosphere_composition/frontend/downloads" >&2
  echo "  set GETECOSPHERE_COMPOSITION=/path/to/getecosphere_composition" >&2
  exit 1
fi

# ---- build from the latest eco main ------------------------------------
cd "$SCRIPT_DIR"
echo "=== [publish-eco] eco repo: $SCRIPT_DIR"
git fetch -q origin "$ECO_BRANCH"
if ! git diff --quiet; then
  echo "publish-eco: local changes in $SCRIPT_DIR would be overwritten by origin/$ECO_BRANCH" >&2
  echo "  stash or commit them first, then re-run" >&2
  exit 1
fi
git checkout -q "$ECO_BRANCH"
git reset -q --hard "origin/$ECO_BRANCH"

VERSION="$(grep '^version' rust/Cargo.toml | head -n1 | awk -F'"' '{print $2}')"
echo "=== [publish-eco] building eco release $VERSION from $ECO_BRANCH"
./build-release.sh

# ---- publish binaries into the getecosphere downloads dir --------------
DEST="$GECO/frontend/downloads"
echo "=== [publish-eco] publishing to $DEST"
for triple in x86_64-unknown-linux-musl x86_64-apple-darwin x86_64-pc-windows-gnu; do
  src="$SCRIPT_DIR/dist/$triple/eco"
  if [ "$triple" = "x86_64-pc-windows-gnu" ]; then src="$src.exe"; fi
  if [ -f "$src" ]; then
    mkdir -p "$DEST/$triple"
    cp "$src" "$DEST/$triple/$(basename "$src")"
    echo "  updated $triple/$(basename "$src") ($(du -h "$src" | cut -f1))"
  else
    echo "  WARN: missing artifact $src" >&2
  fi
done

if [ "$PUSH" = 0 ]; then
  echo "=== [publish-eco] --no-push: binaries copied to $DEST; commit + release manually"
  exit 0
fi

# ---- commit on the current composition branch ---------------------------
cd "$GECO"
echo "=== [publish-eco] getecosphere_composition: $(git branch --show-current)"
git add frontend/downloads
if git diff --cached --quiet; then
  echo "  downloads unchanged; nothing to commit"
else
  git commit -q -m "chore(eco): publish release $VERSION binaries for install.sh"
  echo "  committed downloads update"
fi

# ---- merge feature -> main (prod release) unless declined ---------------
CURRENT_BRANCH="$(git branch --show-current)"
if [ "$CURRENT_BRANCH" = "main" ]; then
  echo "=== [publish-eco] already on main; pushing"
  git push origin main
  exit 0
fi

if [ "$ASSUME_YES" = 1 ]; then
  CONFIRM="y"
else
  printf 'publish-eco: merge "%s" into main and push? This releases to PROD. [y/N] ' "$CURRENT_BRANCH"
  read -r CONFIRM
fi

if [ "${CONFIRM:-n}" = "y" ] || [ "${CONFIRM:-n}" = "Y" ]; then
  echo "=== [publish-eco] pushing $CURRENT_BRANCH"
  git push origin "$CURRENT_BRANCH"
  echo "=== [publish-eco] merging $CURRENT_BRANCH -> main and pushing (releases to prod)"
  git checkout -q main
  git merge -q "$CURRENT_BRANCH"
  git push origin main
  git checkout -q "$CURRENT_BRANCH"
else
  echo "=== [publish-eco] merge to main skipped; $CURRENT_BRANCH committed and pushed:"
  git push origin "$CURRENT_BRANCH"
fi

echo
echo "=== [publish-eco] done. Verify with:"
echo "    curl -fsSL https://getecosphere.com/install.sh | sh && eco --version"
