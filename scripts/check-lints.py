#!/usr/bin/env python3
"""Catch lints that newer clippy versions reject but the MSRV toolchain misses.

Clippy gains lints with every release. A workspace can therefore be clean on the
MSRV (1.75) and still fail on current stable, which is where CI runs. This
script checks, using nothing but Python, the specific lints that have actually
broken this build.

It is a stopgap, not a substitute: it cannot know about lints it was not taught.
Run `cargo clippy` on stable before opening a pull request.

Run: python3 scripts/check-lints.py
"""

import pathlib
import re
import sys

# `clippy::empty_line_after_doc_comments` — a doc comment separated from the
# item it documents by a blank line usually ends up documenting the wrong thing.
ORPHANED_DOC = re.compile(
    r"^[ \t]*///.*\n[ \t]*\n[ \t]*(#\[|pub |fn |struct |enum |impl |const |type )",
    re.MULTILINE,
)

# `clippy::byte_char_slices` — an array of byte-character literals is more
# clearly written as a byte string: [b'a', b'b'] is *b"ab".
BYTE_CHAR_ARRAY = re.compile(r"\[\s*b'[^']*'\s*(?:,\s*b'[^']*'\s*)+,?\s*\]")

# `clippy::derivable_impls` — a manual `Default` for an enum returning a plain
# variant is derivable with `#[derive(Default)]` and `#[default]`.
MANUAL_ENUM_DEFAULT = re.compile(
    r"impl Default for (\w+)\s*\{\s*fn default\(\)\s*->\s*Self\s*\{\s*Self::(\w+)\s*,?\s*\}",
    re.MULTILINE,
)

CHECKS = [
    (ORPHANED_DOC, "blank line after doc comment", "remove the blank line"),
    (BYTE_CHAR_ARRAY, "byte-char array", 'write it as a byte string, e.g. *b"abc"'),
]


def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent
    failures = []

    for path in sorted(root.glob("crates/**/*.rs")):
        text = path.read_text(encoding="utf-8")
        relative = path.relative_to(root)

        for pattern, label, hint in CHECKS:
            for match in pattern.finditer(text):
                line = text[: match.start()].count("\n") + 1
                failures.append(f"{relative}:{line}: {label} — {hint}")

        # A manual enum Default is derivable only if the target is an enum and
        # the returned path is one of its *variants*. Returning an associated
        # constant (`Self::DEFAULT`) cannot be derived, so clippy leaves it be
        # and so must we.
        for match in MANUAL_ENUM_DEFAULT.finditer(text):
            type_name, returned = match.group(1), match.group(2)

            enum_body = re.search(
                rf"^\s*pub enum {type_name}\b[^{{]*\{{(.*?)^\}}",
                text,
                re.MULTILINE | re.DOTALL,
            )
            if not enum_body:
                continue

            is_variant = re.search(
                rf"^\s*{returned}\s*(=|,|\()", enum_body.group(1), re.MULTILINE
            )
            if not is_variant:
                continue

            line = text[: match.start()].count("\n") + 1
            failures.append(
                f"{relative}:{line}: manual Default for enum {type_name} — "
                "use #[derive(Default)] with a #[default] variant"
            )

    if failures:
        print("lint problems found:\n")
        for failure in failures:
            print(f"  {failure}")
        print(f"\n{len(failures)} problem(s).")
        return 1

    print(f"lint checks OK ({len(list(root.glob('crates/**/*.rs')))} files)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
