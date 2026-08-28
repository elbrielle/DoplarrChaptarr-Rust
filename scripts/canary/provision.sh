#!/usr/bin/env bash
# Bring up the disposable canary instance and provision its two roots.
#
#   export CHAPTARR_CANARY_API_KEY="$(openssl rand -hex 16)"
#   scripts/canary/provision.sh
#
# Waits for /ping 503 -> 200, then for the seeded profiles (seeding runs
# after /ping goes green, AppLifetime.cs:57-68), resolves the seeded profile
# ids, and POSTs the two typed root folders from the sprint packet's §7.5
# minimal bodies. Root profile ids are accepted without existence validation
# (RootFolderController.cs:47-48 injects but never attaches the validators),
# so the ids are resolved from the live instance first, never assumed.
# The API key travels only in the X-Api-Key header.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
scratch="$root/scratch/canary"
url="${CHAPTARR_CANARY_URL:-http://127.0.0.1:8789}"
key="${CHAPTARR_CANARY_API_KEY:?set CHAPTARR_CANARY_API_KEY}"

mkdir -p "$scratch/config" "$scratch/audiobooks" "$scratch/ebooks"
docker compose -f "$root/scripts/canary/docker-compose.yml" \
    --project-directory "$scratch" up -d

echo "Recording image digest ..."
docker inspect --format '{{index .RepoDigests 0}}' chaptarr/chaptarr:0.9.936 \
    | tee "$scratch/image-digest.txt"

echo "Waiting for /ping ..."
for _ in $(seq 1 120); do
    status="$(curl -s -o /dev/null -w '%{http_code}' "$url/ping" || true)"
    [[ "$status" == "200" ]] && break
    sleep 2
done
[[ "${status:-}" == "200" ]] || { echo "/ping never went green" >&2; exit 1; }
echo "/ping is green."

echo "Waiting for first-boot profile seeding ..."
for _ in $(seq 1 120); do
    count="$(curl -s -H "X-Api-Key: $key" "$url/api/v1/qualityprofile" | jq 'length' || echo 0)"
    [[ "$count" -ge 2 ]] && break
    sleep 2
done
[[ "${count:-0}" -ge 2 ]] || { echo "quality profiles never seeded" >&2; exit 1; }
echo "Seeded: $count quality profiles."

ebook_quality="$(curl -s -H "X-Api-Key: $key" "$url/api/v1/qualityprofile" \
    | jq '[.[] | select(.profileType == "ebook")][0].id')"
audio_quality="$(curl -s -H "X-Api-Key: $key" "$url/api/v1/qualityprofile" \
    | jq '[.[] | select(.profileType == "audiobook")][0].id')"
# Fresh installs seed only General (0) metadata profiles; Standard is the
# usable one (None is the filter-everything sentinel).
metadata="$(curl -s -H "X-Api-Key: $key" "$url/api/v1/metadataprofile" \
    | jq '[.[] | select(.name == "Standard")][0].id')"
echo "Profiles: ebook quality $ebook_quality, audiobook quality $audio_quality, metadata $metadata"

provision_root() {
    local body="$1"
    curl -s -o /dev/null -w '%{http_code}\n' -X POST \
        -H "X-Api-Key: $key" -H "Content-Type: application/json" \
        -d "$body" "$url/api/v1/rootfolder"
}

echo "Provisioning roots (each add queues a RescanFolders on the empty dir - harmless) ..."
provision_root "{\"name\":\"Audiobooks\",\"path\":\"/audiobooks\",\"folderType\":1,
  \"audiobookQualityProfileId\":$audio_quality,\"audiobookMetadataProfileId\":$metadata}"
provision_root "{\"name\":\"Ebooks\",\"path\":\"/ebooks\",\"folderType\":2,
  \"ebookQualityProfileId\":$ebook_quality,\"ebookMetadataProfileId\":$metadata}"

curl -s -H "X-Api-Key: $key" "$url/api/v1/rootfolder" \
    | jq '[.[] | {id, name, path, folderType, accessible}]'
echo "Canary instance ready at $url"
