"""Cuts Material Symbols down to the icons the editor uses.

Run it after adding an icon to `crates/app/src/icons.rs`:

    python assets/fonts/subset.py            # uses the cached full font, or downloads it
    python assets/fonts/subset.py --keep     # and keeps the full font for next time

The full variable font is 15 MB; what ships is ~30 KB.
"""

import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).parent
FULL = HERE / "MaterialSymbolsRounded.full.ttf"
OUT = HERE / "MaterialSymbolsRounded.ttf"
SOURCE = (
    "https://raw.githubusercontent.com/google/material-design-icons/master/"
    "variablefont/MaterialSymbolsRounded%5BFILL%2CGRAD%2Copsz%2Cwght%5D.ttf"
)
ICONS_RS = HERE.parent.parent / "crates" / "app" / "src" / "icons.rs"


def codepoints() -> list[str]:
    text = ICONS_RS.read_text(encoding="utf-8")
    found = re.findall(r"'\\u\{([0-9a-fA-F]+)\}'", text)
    if not found:
        sys.exit(f"no icons found in {ICONS_RS}")
    return [f"U+{c.upper()}" for c in sorted(set(found))]


def main() -> None:
    if not FULL.exists():
        print(f"downloading {SOURCE}")
        subprocess.run(["curl", "-sL", "-o", str(FULL), SOURCE], check=True)
    unicodes = codepoints()
    print(f"{len(unicodes)} icons")
    from fontTools.subset import main as subset

    try:
        subset([
            str(FULL),
            "--unicodes=" + ",".join(unicodes),
            "--layout-features=",
            "--no-hinting",
            "--desubroutinize",
            f"--output-file={OUT}",
        ])
    except SystemExit as e:
        if e.code:
            raise
    print(f"{OUT.name}: {OUT.stat().st_size // 1024} KB")
    if "--keep" not in sys.argv:
        FULL.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
