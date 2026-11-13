#!/usr/bin/env python3
# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0
"""Add the copyright header to source files, or check that every file has it.

    scripts/license-headers.py           # add it where it is missing
    scripts/license-headers.py --check   # list files without it; exit 1 if any

Covers the Rust sources under crates/ and fuzz/ and the scripts under
scripts/. A file that already names a copyright holder in its first lines
is left alone.
"""
import pathlib
import subprocess
import sys

LINES = [
    "Copyright 2026 Carver Automation Corporation.",
    "",
    'Licensed under the Apache License, Version 2.0 (the "License").',
    "SPDX-License-Identifier: Apache-2.0",
]
COMMENT = {".rs": "//", ".sh": "#", ".py": "#"}


def header(suffix: str) -> str:
    mark = COMMENT[suffix]
    return "".join(f"{mark} {line}".rstrip() + "\n" for line in LINES)


def tracked() -> list[pathlib.Path]:
    out = subprocess.run(
        ["git", "ls-files", "crates", "fuzz", "scripts"],
        check=True, capture_output=True, text=True,
    ).stdout
    return [p for p in map(pathlib.Path, out.split()) if p.suffix in COMMENT]


def main() -> int:
    check = "--check" in sys.argv[1:]
    missing = []
    for path in tracked():
        text = path.read_text()
        if "Copyright" in "\n".join(text.split("\n")[:8]):
            continue
        missing.append(path)
        if check:
            continue
        # A shebang stays on the first line.
        first, rest = "", text
        if text.startswith("#!/"):
            first, _, rest = text.partition("\n")
            first += "\n"
        path.write_text(first + header(path.suffix) + "\n" + rest.lstrip("\n"))
    if check:
        for path in missing:
            print(path)
        return 1 if missing else 0
    print(f"added the header to {len(missing)} file(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
