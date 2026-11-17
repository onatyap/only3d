#!/bin/sh
# Shallow-clone the upstream projects only3d derives data and behaviour from, at the
# revisions recorded in docs/upstream.md, into third_party/upstream/ (git-ignored).
# Generators such as crates/only3d-profiles/tools/gen_bambu_options.py read from there.
#
#   sh tools/fetch-upstream.sh            # all four
#   sh tools/fetch-upstream.sh bambustudio
set -eu
cd "$(dirname "$0")/.."
dest=third_party/upstream
mkdir -p "$dest"

fetch() { # name url commit
  dir="$dest/$1"
  if [ ! -d "$dir/.git" ]; then
    git init -q "$dir"
    git -C "$dir" remote add origin "$2"
  fi
  git -C "$dir" fetch -q --depth 1 origin "$3"
  git -C "$dir" checkout -q --detach FETCH_HEAD
  echo "$1 @ $(git -C "$dir" rev-parse --short HEAD)"
}

want() { [ $# -eq 0 ] || [ "$1" = "$2" ]; }
sel="${1:-}"
want "$sel" bambustudio && fetch bambustudio https://github.com/bambulab/BambuStudio.git 930fe93851afbc76b1c64dd37d8728b6a9afc89c
want "$sel" orcaslicer && fetch orcaslicer https://github.com/OrcaSlicer/OrcaSlicer.git 3bfabc9831622fad0785c41625b4f9a62b63ac21
want "$sel" bambuddy && fetch bambuddy https://github.com/maziggy/bambuddy.git 1aa7bce166682fde929fec0d535bdaaa22685aad
want "$sel" open-bamboo-networking && fetch open-bamboo-networking https://github.com/ClusterM/open-bamboo-networking.git 77de55a8c696c53125ca297bdf3fdf1258772335
