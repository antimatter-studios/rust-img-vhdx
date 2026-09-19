#!/usr/bin/env bash
# Rebuild fuzz/corpus from images qemu-img wrote.
#
# VHDX has more attacker-controlled indirection than any other image
# format in this family: the region table points at the metadata table,
# which declares the block size and virtual disk size, which the BAT is
# then indexed with. Each hop is an offset and a length read out of the
# image, and the log is worse again -- it is a structure the format
# expects to be PARTIALLY WRITTEN, so it is parsed with the corruption
# tolerance a fuzzer is built to abuse.
#
# Usage: scripts/make-fuzz-corpus.sh
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/vhdx-fuzz-corpus.XXXXXX")"
trap 'rm -rf "$work"' EXIT

command -v qemu-img >/dev/null || {
    echo "qemu-img not found; install qemu-utils" >&2
    exit 1
}

# A VHDX is 8 MB before it holds anything: a 1 MB log region, the
# metadata and BAT regions, and 1 MB alignment between them. That is the
# format's floor, not a choice -- `qemu-img create` at a 1 MB virtual
# size produces exactly the same 8 MB as at 4 MB.
#
# So the images are built in a scratch directory and only ONE is
# committed. The structures worth fuzzing on their own -- header, region
# table, metadata, log -- are cut out of all three and are 64 KiB each;
# the whole-image target gets a single 8 MB file, which git stores as
# about 10 KB because it is nearly all zeros.
build() {
    local name="$1"; shift
    local img="$work/$name.vhdx"
    # 2 MiB virtual at a 1 MiB block size: block 0 holds both runs and
    # block 1 is left unallocated, which is what makes a hole worth
    # reading. The result is a 9 MiB file -- under the 10 MiB ceiling
    # github-guard enforces, which a 4 MiB virtual size is not.
    qemu-img create -f vhdx -o "$@" "$img" 2M >/dev/null 2>&1 || {
        echo "qemu-img could not create the '$name' image" >&2
        exit 1
    }
    # Two runs either side of a hole, so the BAT has both allocated and
    # unallocated entries rather than being uniformly one or the other.
    qemu-io -c "write -P 0x41 0 8k" -c "write -P 0x42 512k 8k" "$img" >/dev/null 2>&1 || {
        echo "qemu-io could not write into the '$name' image" >&2
        exit 1
    }
}

rm -rf "$here/fuzz/corpus"
mkdir -p "$here/fuzz/corpus"/{image,header,region_table,metadata,log}

build dynamic subformat=dynamic,block_size=1048576
build fixed   subformat=fixed
# The block size is what the BAT is indexed with, so a non-default one
# changes the arithmetic rather than just the layout.
build block2m subformat=dynamic,block_size=2097152

cp "$work/dynamic.vhdx" "$here/fuzz/corpus/image/dynamic.vhdx"

python3 - "$here/fuzz/corpus" "$work" <<'PY'
import os, struct, sys

root = sys.argv[1]
SIGNATURE = b'vhdxfile'
HEADER_1 = 64 * 1024        # the two header copies live at 64K and 128K
HEADER_2 = 128 * 1024
REGION_1 = 192 * 1024       # and the two region tables at 192K and 256K
REGION_2 = 256 * 1024
SECTION = 64 * 1024

def cut(kind, name, data):
    with open(os.path.join(root, kind, name), 'wb') as f:
        f.write(data)

work = sys.argv[2]
for img_name in sorted(os.listdir(work)):
    if not img_name.endswith('.vhdx'):
        continue
    stem = img_name[:-len('.vhdx')]
    img = open(os.path.join(work, img_name), 'rb').read()
    assert img[:8] == SIGNATURE, f"{img_name}: not a VHDX file"

    # Both header copies, because which one is believed when they
    # disagree is a decision a crafted image gets to influence -- the
    # format picks by sequence number.
    for label, at in (('head1', HEADER_1), ('head2', HEADER_2)):
        block = img[at:at + 4096]
        assert block[:4] == b'head', f"{img_name}: no head signature at {at}"
        cut('header', f'{stem}-{label}.bin', block)

    for label, at in (('region1', REGION_1), ('region2', REGION_2)):
        block = img[at:at + SECTION]
        assert block[:4] == b'regi', f"{img_name}: no regi signature at {at}"
        cut('region_table', f'{stem}-{label}.bin', block)

    # The metadata region's own location comes out of the region table,
    # so it is found by its signature rather than assumed: a 'metadata'
    # signature on a 64 KiB boundary is the region itself.
    for at in range(0, len(img) - SECTION + 1, SECTION):
        if img[at:at + 8] == b'metadata':
            cut('metadata', f'{stem}.bin', img[at:at + SECTION])
            break
    else:
        raise AssertionError(f"{img_name}: no metadata region found")

    # The log region, likewise. An empty log is the ordinary case and
    # still worth seeding: it is the shape the replay path starts from.
    for at in range(0, len(img) - SECTION + 1, SECTION):
        if img[at:at + 4] == b'loge':
            cut('log', f'{stem}.bin', img[at:at + SECTION])
            break
PY

# An empty log is what a cleanly-closed image has, so seed the replay
# path with something it can actually walk as well: a region of zeros
# is a legitimate "nothing to replay" and the shortest path through
# collect_replay_chain.
if [ -z "$(ls -A "$here/fuzz/corpus/log" 2>/dev/null)" ]; then
    head -c 65536 /dev/zero > "$here/fuzz/corpus/log/empty.bin"
fi

echo "corpus rebuilt under fuzz/corpus:"
find "$here/fuzz/corpus" -type f | sort | sed "s#$here/##"
echo "total: $(find "$here/fuzz/corpus" -type f | wc -l) seeds, $(du -sh "$here/fuzz/corpus" | cut -f1)"
