# tests/cli/images.sh -- the images the suite reads, every one made by the
# oracle: this library has no creator, and an image of our own making would
# check our reader against our writer's reading of the format.
#
# make_images builds, in the working directory, and lists in $IMAGES:
#
#   dynamic.vhdx     dynamic, qemu-img's own block size for the size, with
#                    patterns in the first block, a zeroed range
#                    (`write -z`), ranges never written, and the last sector
#   small.vhdx       dynamic with 1 MiB blocks, the smallest the format
#                    allows, so the same writes cross block boundaries and
#                    leave whole blocks never written
#   big.vhdx         dynamic with 32 MiB blocks over a 72 MiB disk, whose
#                    last block is only partly inside the disk
#   fixed.vhdx       `subformat=fixed`: every block present from the start
#   converted.vhdx   a raw image converted by qemu-img, which leaves the
#                    all-zero blocks unallocated
#
# qemu-img makes only 512-byte logical sectors and has no differencing
# VHDX, so neither is here: the 4096-byte case is covered by the crate's
# own tests, and a differencing image is refused by the library at open.
#
# Sourced by the test files that need them; lib.sh has already been.

MiB=1048576
SIZE=$((8 * MiB))
BIG=$((72 * MiB))

# fill SIZE IMAGE: the shared pattern of writes, the last one at the end of
# a disk of SIZE bytes.
fill() {
    local size="$1" img="$2"
    qemu-io -f vhdx \
        -c "write -P 0x5a 4096 8192" \
        -c "write -P 0xc3 $((1 * MiB - 1000)) 3000" \
        -c "write -P 0x3c $((3 * MiB)) $((256 * 1024))" \
        -c "write -z $((3 * MiB + 64 * 1024)) $((64 * 1024))" \
        -c "write -P 0x7e $((size - 512)) 512" \
        "$img" >/dev/null
}

make_images() {
    qemu-img create -q -f vhdx dynamic.vhdx "$SIZE"
    fill "$SIZE" dynamic.vhdx
    qemu-img create -q -f vhdx -o block_size=1M small.vhdx "$SIZE"
    fill "$SIZE" small.vhdx
    qemu-img create -q -f vhdx -o block_size=32M big.vhdx "$BIG"
    fill "$BIG" big.vhdx
    qemu-io -f vhdx -c "write -P 0x11 $((40 * MiB - 100)) 200" big.vhdx >/dev/null
    qemu-img create -q -f vhdx -o subformat=fixed fixed.vhdx "$SIZE"
    fill "$SIZE" fixed.vhdx
    qemu-img convert -f vhdx -O raw small.vhdx small.raw
    qemu-img convert -f raw -O vhdx small.raw converted.vhdx
    rm -f small.raw
    IMAGES="dynamic.vhdx small.vhdx big.vhdx fixed.vhdx converted.vhdx"
}

# size_of IMAGE: its virtual size, as made above.
size_of() {
    case "$1" in
        big.vhdx) echo "$BIG" ;;
        *) echo "$SIZE" ;;
    esac
}
