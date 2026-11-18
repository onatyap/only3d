#!/usr/bin/env bash
# Fails when the tree holds something personal: a real home folder (which names its user), a
# private network address (a printer's, a router's), or, on a machine with printers set up, that
# printer's address, serial or access code. CI runs it; so can you before a commit.
#   tools/readiness/audit.sh
# Portable on purpose: macOS's git grep ignores \b, so the patterns spell out their boundaries.
set -uo pipefail
cd "$(dirname "$0")/../.."
bad=0

# home folders, unless a placeholder name (me, you, user…) or a CI runner's
homes=$(git grep -n -I -E '(/Users/|/home/|[A-Za-z]:\\+Users\\+)[A-Za-z0-9._-]+' -- . ':!Cargo.lock' \
  | grep -v -E '(/Users/|/home/|Users\\+)(me|you|user|runner|ubuntu|Administrator|name|someone|USER|USERNAME)([^A-Za-z0-9._-]|$)|<(you|user)>' || true)
if [ -n "$homes" ]; then echo "Home folders (they name a user):"; echo "$homes"; bad=1; fi

# private network addresses, except the made-up ones that tests, docs and hints use, and the
# makers' sample print hosts in the vendored Bambu Studio and OrcaSlicer profiles
ips=$(git grep -n -I -E '(^|[^0-9.])(192\.168|10\.[0-9]{1,3}|172\.(1[6-9]|2[0-9]|3[01]))\.[0-9]{1,3}\.[0-9]{1,3}([^0-9]|$)' -- . ':!Cargo.lock' ':!profiles/orca' ':!profiles/bambu' \
  | perl -ne 'my @a = /(?<![0-9.])((?:192\.168|10\.\d{1,3}|172\.(?:1[6-9]|2\d|3[01]))\.\d{1,3}\.\d{1,3})(?![0-9])/g;
      print if grep { !/^(192\.168\.1\.(5|20|42|99)|192\.168\.0\.5|10\.0\.0\.[358])$/ } @a' || true)
if [ -n "$ips" ]; then echo "Private network addresses:"; echo "$ips"; bad=1; fi

# this machine's printers (only3d's own book): their values are never printed, only the files
book="${ONLY3D_HOME:-$HOME/.only3d}/printers.json"
if [ -f "$book" ]; then
  hits=$(python3 -c 'import json, sys
for p in json.load(open(sys.argv[1])).get("printers", []):
    for k in ("host", "serial", "access_code"):
        if p.get(k):
            print(p[k])' "$book" 2>/dev/null \
    | while IFS= read -r v; do git grep -l -I -F -e "$v" -- . ':!Cargo.lock'; done | sort -u)
  if [ -n "$hits" ]; then echo "A printer's address, serial or access code (from $book) is in:"; echo "$hits"; bad=1; fi
fi

[ "$bad" = 0 ] && echo "audit: nothing personal found"
exit "$bad"
