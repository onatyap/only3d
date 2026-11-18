"""THIRD-PARTY-NOTICES.txt for the release packages: the licence texts of every crate the
program is built from (MIT, BSD and Apache ask for their notices to travel with binaries), plus
the Bambu Studio printer profiles only3d embeds.

    python3 packaging/third_party_notices.py target/THIRD-PARTY-NOTICES.txt

Reads `cargo metadata` and each crate's own licence files; crates that ship the same text are
listed together under it.
"""

import json
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
NAMES = re.compile(r"^(LICEN[SC]E|COPYING|NOTICE|COPYRIGHT|OFL|UFL|UNLICENSE)", re.I)

HEADER = """only3d: third-party notices

only3d is free software under the GNU Affero General Public License, version 3 or later (see
LICENSE). It is built from the open-source packages below, each under its own licence, and it
embeds:

- Bambu Studio's printer, process and filament profiles and G-code templates
  (profiles/bambu), copyright Bambu Lab, under the GNU AGPL 3.0:
  https://github.com/bambulab/BambuStudio

only3d isn't affiliated with Bambu Lab.
"""


def used_packages(meta):
    """Packages reachable from the workspace through normal and build dependencies."""
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    todo = list(meta["workspace_members"])
    seen = set(todo)
    while todo:
        for dep in nodes[todo.pop()]["deps"]:
            if any(k["kind"] in (None, "build") for k in dep["dep_kinds"]) and dep["pkg"] not in seen:
                seen.add(dep["pkg"])
                todo.append(dep["pkg"])
    members = set(meta["workspace_members"])
    return [p for p in meta["packages"] if p["id"] in seen and p["id"] not in members]


def licence_files(pkg):
    root = os.path.dirname(pkg["manifest_path"])
    found = []
    if pkg.get("license_file"):
        found.append(os.path.join(root, pkg["license_file"]))
    for dirpath, dirs, files in os.walk(root):
        depth = dirpath[len(root):].count(os.sep)
        dirs[:] = [d for d in dirs if depth < 1 and d not in ("target", "tests", "benches", "examples", "src")]
        found += [os.path.join(dirpath, f) for f in sorted(files) if NAMES.match(f)]
    return list(dict.fromkeys(f for f in found if os.path.isfile(f)))


# Standard wording, found in the crates that ship it: (licence, how to recognise it, where its
# body starts). Crates that ship no file of their own are listed under one of these.
STANDARD = [
    ("MIT", "Permission is hereby granted, free of charge", "Permission is hereby granted"),
    ("Apache-2.0", "Version 2.0, January 2004", "Apache License"),
    ("BSD-2-Clause", "Redistribution and use in source and binary forms", "Redistribution and use"),
    ("BSL-1.0", "Boost Software License", "Boost Software License"),
]


def standard_texts(all_texts):
    """The most common body of each standard licence among the shipped files."""
    found = {}
    for spdx, mark, start in STANDARD:
        bodies = {}
        for t in all_texts:
            if mark in t and (spdx != "BSD-2-Clause" or "3. Neither the name" not in t and "* Neither the name" not in t):
                body = t[t.index(start):].strip()
                bodies[body] = bodies.get(body, 0) + 1
        if bodies:
            found[spdx] = max(bodies.items(), key=lambda kv: kv[1])[0]
    return found


def pick(expr, have):
    """The licence a crate is used under: MIT where the expression offers it."""
    ids = re.findall(r"[A-Za-z0-9.\-]+", expr or "")
    for spdx in ["MIT", "Apache-2.0", "BSD-2-Clause", "BSL-1.0"]:
        if spdx in ids and spdx in have:
            return spdx
    return None


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "target", "THIRD-PARTY-NOTICES.txt")
    meta = json.loads(subprocess.run(["cargo", "metadata", "--format-version", "1", "--locked"], cwd=ROOT, capture_output=True, text=True, check=True).stdout)
    pkgs = sorted(used_packages(meta), key=lambda p: (p["name"], p["version"]))
    texts = {}  # licence text -> ["name version (licence)"]
    bare = []  # crates that ship no licence file: their SPDX licence and where to find it
    for p in pkgs:
        label = f"{p['name']} {p['version']} ({p.get('license') or 'see its repository'})"
        files = licence_files(p)
        if not files:
            bare.append(p)
        for f in files:
            with open(f, encoding="utf-8", errors="replace") as fh:
                texts.setdefault(fh.read().strip(), []).append(label)
    parts = [HEADER, f"\n{len(pkgs)} packages.\n"]
    for text, users in sorted(texts.items(), key=lambda kv: kv[1][0]):
        parts.append("\n" + "=" * 78 + "\n" + "\n".join(dict.fromkeys(users)) + "\n" + "-" * 78 + "\n" + text + "\n")
    if bare:
        std = standard_texts(texts.keys())
        used = {}
        lines = []
        for p in bare:
            spdx = pick(p.get("license"), std)
            who = ", ".join(p.get("authors") or []) or "its authors"
            how = f"used under {spdx}" if spdx else f"licence {p.get('license')}, see {p.get('repository') or 'its repository'}"
            lines.append(f"{p['name']} {p['version']} ({p.get('license')}): copyright {who}; {how}")
            if spdx:
                used[spdx] = True
        parts.append("\n" + "=" * 78 + "\nThese packages ship no licence file of their own; each is used under the standard\ntext that follows, with the copyright of the authors named:\n" + "\n".join(lines) + "\n")
        for spdx in used:
            parts.append("\n" + "-" * 78 + f"\n{spdx}\n" + "-" * 78 + "\n" + std[spdx] + "\n")
    os.makedirs(os.path.dirname(os.path.abspath(out)), exist_ok=True)
    with open(out, "w", encoding="utf-8") as f:
        f.write("".join(parts))
    print(f"{out}: {len(pkgs)} packages, {len(texts)} licence texts, {len(bare)} without a file of their own")


if __name__ == "__main__":
    main()
