"""Write docs/printers.md from profiles/printers.json: the public table of which printers only3d
supports and how far each is tested.

    python3 tools/printers_table.py          # rewrite docs/printers.md
    python3 tools/printers_table.py --check  # fail if docs/printers.md is out of date (CI)
"""

import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MARK = {"tested": "✅ tested", "checked": "🔍 checked", "untested": "⚪ untested", "no": "— no"}


def render(reg):
    out = [
        "# Printers",
        "",
        "Which printers only3d slices, exports and prints for, and how far each is tested. This page",
        "is generated from [profiles/printers.json](../profiles/printers.json); the app reads the same",
        "file and warns before a first print on a printer nobody has printed on with only3d yet.",
        "",
    ]
    for key, text in reg["statuses"].items():
        out.append(f"- **{MARK[key]}**: {text}.")
    printers = reg["printers"]
    count = lambda step, st: sum(1 for p in printers if p[step]["status"] == st)
    makers = sorted({p["maker"] for p in printers})
    out += [
        "",
        f"{len(printers)} printers from {len(makers)} makers. Slicing: {count('slice', 'checked')} checked against the maker's "
        f"slicer, {count('slice', 'untested')} untested, {count('slice', 'no')} not yet. Printing: {count('print', 'tested')} tested.",
        "",
        "Bambu Lab's printers come with Bambu Studio's profiles; the other makers' with OrcaSlicer's",
        "(see [profiles/orca/README.md](../profiles/orca/README.md)).",
    ]

    def table(rows, maker_column):
        head = "| Maker | Printer | Slice | Export | Print | Notes |" if maker_column else "| Printer | Slice | Export | Print | Notes |"
        lines = [head, "|---" * head.count("|") + "|"][:2]
        lines[1] = "|" + "---|" * (head.count("|") - 1)
        for p in rows:
            notes = []
            for step in ("slice", "export", "print"):
                s = p[step]
                if s.get("note"):
                    notes.append(f"{step.capitalize()}: {s['note']}" + (f" ({s['date']})" if s.get("date") else ""))
            cells = [p["maker"]] if maker_column else []
            cells += [p["name"], MARK[p["slice"]["status"]], MARK[p["export"]["status"]], MARK[p["print"]["status"]], "<br>".join(notes)]
            lines.append("| " + " | ".join(c.replace("|", "\\|") for c in cells) + " |")
        return lines

    out += ["", "## Bambu Lab", ""] + table([p for p in printers if p["maker"] == "Bambu Lab"], False)
    out += ["", "## Other makers", ""] + table([p for p in printers if p["maker"] != "Bambu Lab"], True)
    out += [
        "",
        "## Help test a printer",
        "",
        "If you print with only3d on a printer marked untested or checked, please open an issue: the",
        "printer, its firmware, what you printed and how it came out. A good first print lifts it to",
        "tested.",
        "",
    ]
    return "\n".join(out)


def main():
    with open(os.path.join(ROOT, "profiles", "printers.json")) as f:
        reg = json.load(f)
    text = render(reg)
    path = os.path.join(ROOT, "docs", "printers.md")
    if "--check" in sys.argv:
        current = open(path).read() if os.path.exists(path) else ""
        if current != text:
            sys.exit("docs/printers.md is out of date: run python3 tools/printers_table.py")
        print("docs/printers.md is up to date")
        return
    with open(path, "w") as f:
        f.write(text)
    print(f"wrote {os.path.relpath(path, ROOT)} ({len(reg['printers'])} printers)")


if __name__ == "__main__":
    main()
