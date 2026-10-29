#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
case "${1:-turbo}" in
    turbo) model=ggml-large-v3-turbo.bin; checksum=1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69 ;;
    large-v3) model=ggml-large-v3.bin; checksum=64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2 ;;
    *) echo 'Use turbo or large-v3' >&2; exit 2 ;;
esac
mkdir -p "$root/models"
tmp=$(mktemp "$root/models/${model}.XXXXXX.download")
trap 'rm -f "$tmp"' EXIT HUP INT TERM
curl -fL --retry 2 "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/$model" -o "$tmp"
printf '%s  %s\n' "$checksum" "$tmp" | sha256sum -c -
mv "$tmp" "$root/models/$model"
