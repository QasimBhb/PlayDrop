#!/usr/bin/env bash
# Build the deterministic throughput datasets (docs/release/throughput.md).
#
# Usage: bench/make-dataset.sh [--force] [ROOT]
#
# ROOT defaults to /tmp/pd-bench and must be on a local Linux filesystem (ext4 here),
# never inside the repo. The same seed gives byte-identical files on every machine, so a
# manifest from one run checks another.
#
# Layout under ROOT:
#   big-1g/        one 1 GiB file
#   small-1000/    1,000 files of exactly 100 KiB
#   mixed-5000/    5,000 files of 4 to 64 KiB (seeded sizes)
#   medium-100/    100 files of 1.0 to 2.3 MiB (seeded), about the same total bytes as mixed-5000,
#                  so 100 vs 5,000 files compares per-file cost, not volume
#   tree/          a folder tree: 4 levels, 3 branches each, 1 KiB to 2 MiB files, plus
#                  a few empty folders
#   manifests/<dataset>.sha256   "sum  <dataset>/<relative path>", sorted by path
#   manifests/<dataset>.stats    "files=<n> bytes=<n>"
#
# An existing dataset whose manifest still matches is kept; --force rebuilds it.

set -euo pipefail

force=0
if [[ "${1:-}" == "--force" ]]; then
    force=1
    shift
fi
root="${1:-/tmp/pd-bench}"

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
mkdir -p "$root"
root="$(cd "$root" && pwd)"
case "$root/" in
    "$repo"/*)
        echo "refusing to write datasets inside the repo ($root)" >&2
        exit 1
        ;;
esac

fstype="$(stat -f -c %T "$root")"
if [[ "$fstype" != "ext2/ext3" ]]; then
    echo "warning: $root is on $fstype, not ext4; numbers may not match the notebook" >&2
fi

datasets=(big-1g small-1000 mixed-5000 medium-100 tree)
mkdir -p "$root/manifests"

manifest_ok() {
    local name="$1"
    [[ -f "$root/manifests/$name.sha256" && -d "$root/$name" ]] || return 1
    (cd "$root" && sha256sum --quiet --strict -c "manifests/$name.sha256") >/dev/null 2>&1 || return 1
    local listed actual
    listed="$(wc -l <"$root/manifests/$name.sha256")"
    actual="$(find "$root/$name" -type f | wc -l)"
    [[ "$listed" == "$actual" ]]
}

for name in "${datasets[@]}"; do
    if [[ "$force" == 0 ]] && manifest_ok "$name"; then
        echo "$name: up to date"
        continue
    fi
    rm -rf "${root:?}/$name"
    echo "$name: generating"
    # Python's Random(seed).randbytes is stable across CPython 3.9+, so the bytes are
    # reproducible. -I keeps the interpreter from importing anything from the cwd.
    python3 -I - "$root" "$name" <<'PY'
import os
import random
import sys

root, name = sys.argv[1], sys.argv[2]
KIB = 1024
MIB = 1024 * KIB
base = os.path.join(root, name)
os.makedirs(base)


def write(path, size, rng):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        left = size
        while left:
            n = min(left, 4 * MIB)
            f.write(rng.randbytes(n))
            left -= n


if name == "big-1g":
    write(os.path.join(base, "big-1GiB.bin"), 1024 * MIB, random.Random(1))
elif name == "small-1000":
    rng = random.Random(2)
    for i in range(1000):
        write(os.path.join(base, f"s{i:04d}.bin"), 100 * KIB, rng)
elif name == "mixed-5000":
    sizes = random.Random(3)
    data = random.Random(4)
    for i in range(5000):
        write(os.path.join(base, f"m{i:04d}.bin"), sizes.randint(4 * KIB, 64 * KIB), data)
elif name == "medium-100":
    sizes = random.Random(7)
    data = random.Random(8)
    for i in range(100):
        write(os.path.join(base, f"x{i:03d}.bin"), sizes.randint(1024 * KIB, 2360 * KIB), data)
elif name == "tree":
    sizes = random.Random(5)
    data = random.Random(6)

    def grow(path, depth):
        for f in range(4):
            # Mostly small with a long tail, like a source or photo folder.
            size = int(min(2 * MIB, max(KIB, sizes.lognormvariate(10.5, 1.4))))
            write(os.path.join(path, f"f{depth}{f}.dat"), size, data)
        if depth == 4:
            return
        for b in range(3):
            grow(os.path.join(path, f"d{depth}{b}"), depth + 1)

    grow(base, 1)
    for e in range(3):
        os.makedirs(os.path.join(base, f"empty{e}"))
else:
    sys.exit(f"unknown dataset {name}")
PY
    (cd "$root" && find "$name" -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum) \
        >"$root/manifests/$name.sha256"
done

printf '\n| Dataset | Files | Bytes | MiB | Manifest sha256 (first 16) |\n|---|---|---|---|---|\n'
for name in "${datasets[@]}"; do
    files="$(find "$root/$name" -type f | wc -l)"
    bytes="$(find "$root/$name" -type f -printf '%s\n' | awk '{s += $1} END {printf "%d", s}')"
    printf 'files=%s bytes=%s\n' "$files" "$bytes" >"$root/manifests/$name.stats"
    msum="$(sha256sum "$root/manifests/$name.sha256" | cut -c1-16)"
    awk -v n="$name" -v f="$files" -v b="$bytes" -v m="$msum" \
        'BEGIN {printf "| %s | %d | %d | %.1f | %s |\n", n, f, b, b / 1048576, m}'
done
echo
echo "Root: $root (manifests in $root/manifests)"
