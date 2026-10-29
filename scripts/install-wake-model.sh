#!/bin/sh
# One-time model installation; inference never uses the network.
set -eu
repo=sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
curl -fL --retry 2 "https://github.com/k2-fsa/sherpa-onnx/releases/download/kws-models/$repo.tar.bz2" -o "$tmp/model.tar.bz2"
printf '%s  %s\n' f170013b4716e41b62b9bfd809687c207cef798ef9bc6534d524e17af9b6561a "$tmp/model.tar.bz2" | sha256sum -c -
mkdir -p "$root/models"
tar xjf "$tmp/model.tar.bz2" -C "$root/models"
printf 'Installed wake detector in %s/models/%s\n' "$root" "$repo"
