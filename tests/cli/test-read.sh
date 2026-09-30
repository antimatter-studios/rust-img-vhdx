# `read` on images the oracle made: the whole disk is byte-identical to
# `qemu-img convert -O raw` for every image, and every range is the same
# slice of that raw image: straddling a block boundary, inside a zeroed
# range, inside blocks never written, the last byte. No read changes the
# image file.
source "$(dirname "$0")/lib.sh"
source "$(dirname "$0")/images.sh"

cd "$SANDBOX" || exit 1
make_images

# slice FILE OFFSET LENGTH: those bytes of FILE, on stdout.
slice() {
    tail -c +$(($2 + 1)) "$1" | head -c "$3"
}

for img in $IMAGES; do
    size="$(size_of "$img")"
    cp "$img" "$img.before"
    qemu-img convert -f vhdx -O raw "$img" "$img.qemu.raw"
    img.vhdx "$img" read >"$img.ours.raw"
    check "read of the whole $img exits 0" test $? -eq 0
    same "the whole of $img, read, is qemu-img's raw image" "$img.ours.raw" "$img.qemu.raw"

    for range in "4096 8192" "$((1 * MiB - 1000)) 3000" "$((3 * MiB + 60000)) 10000" \
        "$((5 * MiB)) 65536" "$((1 * MiB - 4000)) $((2 * MiB))" \
        "$((size - 1)) 1" "$((size - 512)) 512"; do
        set -- $range
        img.vhdx "$img" read --offset "$1" --length "$2" >"$img.range"
        slice "$img.qemu.raw" "$1" "$2" >"$img.want"
        same "$img: read --offset $1 --length $2" "$img.range" "$img.want"
    done
    same "no read changed $img" "$img" "$img.before"
done

# Across big.vhdx's 32 MiB block boundary, and into its partly-inside last
# block.
for range in "$((32 * MiB - 700)) 1400" "$((40 * MiB - 100)) 200" "$((64 * MiB - 10)) $((8 * MiB + 10))"; do
    set -- $range
    img.vhdx big.vhdx read --offset "$1" --length "$2" >big.range
    slice big.vhdx.qemu.raw "$1" "$2" >big.want
    same "big.vhdx: read --offset $1 --length $2" big.range big.want
done

img.vhdx fixed.vhdx read -o fixed.o.raw
same "read -o writes the same bytes as read to stdout" fixed.o.raw fixed.vhdx.qemu.raw
check "read -o leaves no .partial file" test ! -e fixed.o.raw.partial

# --offset alone reads to the end; --length alone reads from 0.
img.vhdx dynamic.vhdx read --offset $((SIZE - 512)) >tail.bin
slice dynamic.vhdx.qemu.raw $((SIZE - 512)) 512 >want.bin
same "read --offset alone reads to the end" tail.bin want.bin
img.vhdx dynamic.vhdx read --length 16K >head.bin
slice dynamic.vhdx.qemu.raw 0 16384 >want.bin
same "read --length alone reads from 0, and takes a suffix" head.bin want.bin

# A range past the end is refused whole, before a byte is written.
expect_error "a range past the end" 1 img.vhdx dynamic.vhdx read --offset $((SIZE - 512)) --length 513
expect_error "an offset past the end" 1 img.vhdx dynamic.vhdx read --offset $((SIZE + 1))

# An image file nobody may write is read all the same: the read-only verbs
# never open the file for writing.
cp dynamic.vhdx locked.vhdx
chmod 0444 locked.vhdx
img.vhdx locked.vhdx read >locked.raw
check "a read-only image file reads" test $? -eq 0
same "a read-only image file reads the same bytes" locked.raw dynamic.vhdx.qemu.raw
chmod 0644 locked.vhdx

# A closed pipe is the reader's choice, not a failure.
img.vhdx dynamic.vhdx read 2>pipe.err | head -c 100 >/dev/null
check "read into a pipe closed early exits 0" test "${PIPESTATUS[0]}" -eq 0
check "read into a pipe closed early says nothing on stderr" test ! -s pipe.err

finish
