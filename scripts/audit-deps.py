#!/usr/bin/env python3
"""Cross-reference Cargo.lock against the RustSec advisory database.

`cargo audit` is the proper tool and should be used in CI. This exists because
it is not always installable on the MSRV toolchain, and a security-relevant
dependency tree should not go unchecked for that reason.

Clone the database first:

    git clone --depth 1 https://github.com/rustsec/advisory-db /tmp/advdb

then run:

    python3 scripts/audit-deps.py

Exits non-zero if any dependency has an unpatched security advisory.
Informational advisories (unmaintained, unsound-with-no-fix) are reported
separately, because conflating them with exploitable vulnerabilities trains
people to ignore both.
"""

import pathlib
import re
import sys

def parse_lock(path):
    pkgs = []
    name = ver = None
    for line in pathlib.Path(path).read_text().splitlines():
        line = line.strip()
        if line == "[[package]]":
            name = ver = None
        elif line.startswith("name = "):
            name = line.split('"')[1]
        elif line.startswith("version = ") and name:
            ver = line.split('"')[1]
            pkgs.append((name, ver))
            name = ver = None
    return pkgs

def vt(v):
    parts = re.split(r"[.\-+]", v)
    out = []
    for p in parts[:3]:
        try: out.append(int(p))
        except ValueError: out.append(0)
    while len(out) < 3: out.append(0)
    return tuple(out)

def satisfies(ver, req):
    """Approximate semver range check for the forms used in advisory-db."""
    req = req.strip()
    v = vt(ver)
    for clause in [c.strip() for c in req.split(",")]:
        m = re.match(r"^(>=|<=|>|<|\^|=)?\s*([0-9][0-9.]*)", clause)
        if not m: continue
        op, target = m.group(1) or "=", vt(m.group(2))
        if op == ">=" and not v >= target: return False
        if op == ">"  and not v >  target: return False
        if op == "<=" and not v <= target: return False
        if op == "<"  and not v <  target: return False
        if op == "="  and not v == target: return False
        if op == "^":
            if v < target: return False
            if target[0] > 0 and v[0] != target[0]: return False
            if target[0] == 0 and (v[0] != 0 or v[1] != target[1]): return False
    return True

db = pathlib.Path("/tmp/advdb/crates")
if not db.is_dir():
    print("advisory database not found at /tmp/advdb", file=sys.stderr)
    print("clone it: git clone --depth 1 https://github.com/rustsec/advisory-db /tmp/advdb", file=sys.stderr)
    sys.exit(2)
findings = []
notices = []
checked = 0

for name, ver in parse_lock("Cargo.lock"):
    d = db / name
    if not d.is_dir(): continue
    checked += 1
    for f in sorted(d.glob("*.md")):
        text = f.read_text()
        patched = re.search(r'^patched\s*=\s*\[(.*?)\]', text, re.M | re.S)
        unaffected = re.search(r'^unaffected\s*=\s*\[(.*?)\]', text, re.M | re.S)
        informational = re.search(r'^informational\s*=\s*"(.*?)"', text, re.M)
        title = re.search(r'^title\s*=\s*"(.*?)"', text, re.M)
        withdrawn = "withdrawn" in text.lower() and re.search(r'^withdrawn', text, re.M)
        if withdrawn: continue

        def ranges(m):
            return re.findall(r'"([^"]+)"', m.group(1)) if m else []

        # Fixed versions first: an advisory we are patched against is simply
        # not applicable, informational or otherwise.
        if any(satisfies(ver, r) for r in ranges(patched)):
            continue
        if any(satisfies(ver, r) for r in ranges(unaffected)):
            continue

        label = title.group(1) if title else f.stem
        # Only now does the distinction matter: an unmaintained or unsound
        # notice with no fix is not the same as an exploitable vulnerability.
        if informational:
            notices.append((name, ver, f.stem, informational.group(1)))
        else:
            findings.append((name, ver, f.stem, label))

print(f"packages cross-referenced against advisory-db: {checked} of {len(parse_lock('Cargo.lock'))}")
if findings:
    print(f"\nVULNERABLE ({len(findings)}):")
    for n, v, adv, t in findings:
        print(f"  {n} {v} — {adv}: {t}")
    sys.exit(1)
if notices:
    print(f"\nadvisory notices, not vulnerabilities ({len(notices)}):")
    for n, v, adv, kind in notices:
        print(f"  {n} {v} — {adv}: {kind}")

print("\nno unpatched security advisories found")
