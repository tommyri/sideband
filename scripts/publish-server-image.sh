#!/usr/bin/env bash
set -euo pipefail

context="$RUNNER_TEMP/image"
mkdir -p "$context/data"
install -m 0755 dist/ss-server-x86_64-unknown-linux-musl "$context/ss-server-amd64"
install -m 0755 dist/ss-server-aarch64-unknown-linux-musl "$context/ss-server-arm64"
printf '%s' "$GH_TOKEN" | docker login ghcr.io -u "$GITHUB_ACTOR" --password-stdin
tags=(--tag "$IMAGE:$VERSION")
if [[ "$VERSION" == *-alpha.* ]]; then
  [[ "$MOVE_LATEST" == false ]]
  digest=$(node scripts/release-gate.mjs existing-alpha-image)
  if [[ -n "$digest" ]]; then
    echo "digest=$digest" >> "$GITHUB_OUTPUT"
    echo "Reusing $IMAGE:$VERSION at $digest" >> "$GITHUB_STEP_SUMMARY"
    exit 0
  fi
else
  tags+=(--tag "$IMAGE:sha-$GITHUB_SHA")
fi
if [ "$MOVE_LATEST" = true ]; then tags+=(--tag "$IMAGE:latest"); fi
# Dockerfile.release only copies files, so both platforms build without emulation
# and are pushed as one index: no per-architecture tags are ever created.
# Pinned by tag and digest in .github/buildkit/Dockerfile, which Dependabot updates.
buildkit=$(sed -n 's/^FROM //p' .github/buildkit/Dockerfile)
if [[ ! "$buildkit" =~ ^moby/buildkit:v[0-9]+\.[0-9]+\.[0-9]+@sha256:[0-9a-f]{64}$ ]]; then
  echo "::error::.github/buildkit/Dockerfile must have one FROM moby/buildkit:vX.Y.Z@sha256:<digest> line"
  exit 1
fi
docker buildx create --name sideband-release --driver docker-container \
  --driver-opt "image=$buildkit"
# The login stays for Attest image; the last step of this job logs out.
trap 'docker buildx rm sideband-release' EXIT
docker buildx build --builder sideband-release --platform linux/amd64,linux/arm64 \
  -f deploy/Dockerfile.release --provenance=false --sbom=false \
  --label "org.opencontainers.image.source=https://github.com/$GITHUB_REPOSITORY" \
  --label "org.opencontainers.image.revision=$GITHUB_SHA" \
  --label "org.opencontainers.image.version=$VERSION" \
  "${tags[@]}" --metadata-file "$RUNNER_TEMP/image.json" --push "$context"
digest=$(jq -r '."containerimage.digest"' "$RUNNER_TEMP/image.json")
[[ "$digest" =~ ^sha256:[0-9a-f]{64}$ ]]
echo "digest=$digest" >> "$GITHUB_OUTPUT"
docker buildx imagetools inspect "$IMAGE@$digest"
{
  echo "Published \`$IMAGE:$VERSION\` (linux/amd64 and linux/arm64), digest \`$digest\`."
  if [ "$MOVE_LATEST" = true ]; then echo "\`latest\` moved to $VERSION."; fi
} >> "$GITHUB_STEP_SUMMARY"
