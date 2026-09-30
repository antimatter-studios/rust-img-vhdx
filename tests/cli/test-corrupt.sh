# Damaged images fail with a structured error and nothing on stdout -- both
# header slots bad, both region tables failing their checksum, a BAT entry
# pointing past the end of the file, a truncated file -- and the oracle
# agrees each one is damaged: `qemu-img check` reports it, or refuses to
# open it. No damaged image is changed by being looked at.
source "$(dirname "$0")/lib.sh"

cd "$SANDBOX" || exit 1

KiB=1024
# poke FILE OFFSET BYTE: overwrite one byte.
poke() {
    printf "\\$(printf '%03o' "$3")" | dd of="$1" bs=1 seek="$2" conv=notrunc 2>/dev/null
}
# le FILE OFFSET COUNT: COUNT bytes at OFFSET as one little-endian number.
le() {
    local hex
    hex="$(od -An -tx1 -j "$2" -N "$3" "$1" | tr -d ' \n' | sed 's/../& /g' | awk '{for (i = NF; i > 0; i--) printf "%s", $i}')"
    echo $((16#$hex))
}
# bat_offset FILE: where the BAT region starts, from the first region
# table's entry for the BAT's GUID.
bat_offset() {
    local table=$((192 * KiB)) count i entry
    count="$(le "$1" $((table + 8)) 4)"
    for ((i = 0; i < count; i++)); do
        entry=$((table + 16 + 32 * i))
        if [ "$(od -An -tx1 -j "$entry" -N 16 "$1" | tr -d ' \n')" = "6677c22d23f600429d64115e9bfd4a08" ]; then
            le "$1" $((entry + 16)) 8
            return
        fi
    done
    echo "no BAT region in $1" >&2
    echo 0
}

# qemu_rejects DESCRIPTION IMAGE: `qemu-img check` exits non-zero on it.
qemu_rejects() {
    if qemu-img check -f vhdx "$2" >"$SANDBOX/check.out" 2>&1; then
        fail "$1: qemu-img check calls it clean: $(head -c 300 "$SANDBOX/check.out")"
    else
        ok
    fi
}

# damaged DESCRIPTION IMAGE VERB...: the verb fails as a structured error,
# qemu-img rejects the image, and the image is as it was.
damaged() {
    local what="$1" img="$2"
    shift 2
    cp "$img" "$img.before"
    expect_error "$what" 1 img.vhdx "$img" "$@"
    same "$what: the image is as it was" "$img" "$img.before"
}

qemu-img create -q -f vhdx good.vhdx 8M
qemu-io -f vhdx -c "write -P 0x42 0 65536" good.vhdx >/dev/null
check "qemu-img check calls the undamaged image clean" qemu-img check -q -f vhdx good.vhdx

# Both header slots: the signature is not `head`.
cp good.vhdx bad-headers.vhdx
poke bad-headers.vhdx $((64 * KiB)) 0
poke bad-headers.vhdx $((128 * KiB)) 0
damaged "both headers bad" bad-headers.vhdx info
damaged "both headers bad, read" bad-headers.vhdx read
qemu_rejects "both headers bad" bad-headers.vhdx

# One header bad is not damage: the other slot is the header.
cp good.vhdx one-header.vhdx
poke one-header.vhdx $((64 * KiB)) 0
img.vhdx one-header.vhdx read --length 65536 >one-header.out
check "one bad header slot still reads" test $? -eq 0
qemu-img convert -f vhdx -O raw good.vhdx good.raw
same "one bad header slot reads the right bytes" one-header.out <(head -c 65536 good.raw)

# Both region tables: a byte inside each, so each fails its checksum.
cp good.vhdx bad-regions.vhdx
poke bad-regions.vhdx $((192 * KiB + 100)) 0x55
poke bad-regions.vhdx $((256 * KiB + 100)) 0x55
damaged "both region tables fail their checksum" bad-regions.vhdx info
qemu_rejects "both region tables fail their checksum" bad-regions.vhdx

# The first BAT entry: fully present (state 6), at an offset far past the
# end of the file.
bat="$(bat_offset good.vhdx)"
check "the BAT region is found (at $bat)" test "$bat" -gt 0
cp good.vhdx bat-past-end.vhdx
printf '\006\000\000\000\000\000\020\000' | dd of=bat-past-end.vhdx bs=1 seek="$bat" conv=notrunc 2>/dev/null
damaged "a BAT entry past the end of the file" bat-past-end.vhdx read --length 512
qemu_rejects "a BAT entry past the end of the file" bat-past-end.vhdx

# Cut short: the headers survive, the BAT and data do not.
head -c $((300 * KiB)) good.vhdx >truncated.vhdx
damaged "a truncated image" truncated.vhdx read
qemu_rejects "a truncated image" truncated.vhdx

# Not an image at all, and an empty file.
head -c 65536 /dev/urandom >noise.bin
expect_error "a file of noise" 1 img.vhdx noise.bin info
: >empty.vhdx
expect_error "an empty file" 1 img.vhdx empty.vhdx info
expect_error "a file that is not there" 1 img.vhdx missing.vhdx info

finish
