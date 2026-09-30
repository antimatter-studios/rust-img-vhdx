# `info` and `get` on images the oracle made: every canonical key, with the
# right type, and the values `qemu-img info` reports for the same file --
# virtual size, block size (qemu's cluster size), backing file, dirty flag.
# And `info` is only a look: the file is byte-identical afterwards.
source "$(dirname "$0")/lib.sh"
source "$(dirname "$0")/images.sh"

cd "$SANDBOX" || exit 1
make_images

for img in $IMAGES; do
    cp "$img" "$img.before"
    img.vhdx "$img" info >"$img.json"
    check "info on $img exits 0" test $? -eq 0
    same "info left $img as it was" "$img" "$img.before"
    qemu-img info -f vhdx --output=json "$img" >"$img.qemu.json"

    jq_check "$img: the canonical keys, in order" \
        '[keys_unsorted[]] == ["format","virtual_size","block_size","backing","dirty","vhdx"]' "$img.json"
    jq_check "$img: format is vhdx" '.format == "vhdx"' "$img.json"
    jq_check "$img: the sizes are numbers" \
        '[.virtual_size, .block_size, .vhdx.logical_sector_size, .vhdx.header_sequence, .vhdx.log_size] | all(type == "number")' "$img.json"
    jq_check "$img: physical_sector_size is a number or null" \
        '.vhdx.physical_sector_size | type == "number" or . == null' "$img.json"
    jq_check "$img: dirty is a boolean" '.dirty | type == "boolean"' "$img.json"
    jq_check "$img: backing is null (no differencing image opens)" '.backing == null' "$img.json"
    jq_check "$img: the logical sector is qemu-img's 512" '.vhdx.logical_sector_size == 512' "$img.json"
    jq_check "$img: the log is a whole number of megabytes" \
        '.vhdx.log_size > 0 and .vhdx.log_size % 1048576 == 0' "$img.json"

    # The oracle's view of the same file.
    jq_check "$img: virtual_size is qemu-img's" \
        --slurpfile q "$img.qemu.json" '.virtual_size == $q[0]."virtual-size"' "$img.json"
    jq_check "$img: block_size is qemu-img's cluster size" \
        --slurpfile q "$img.qemu.json" '.block_size == $q[0]."cluster-size"' "$img.json"
    jq_check "$img: backing is qemu-img's backing-filename" \
        --slurpfile q "$img.qemu.json" '.backing == $q[0]."backing-filename"' "$img.json"
    jq_check "$img: dirty is qemu-img's dirty-flag" \
        --slurpfile q "$img.qemu.json" '.dirty == $q[0]."dirty-flag"' "$img.json"
done

jq_check "small.vhdx has 1 MiB blocks" '.block_size == 1048576' small.vhdx.json
jq_check "big.vhdx has 32 MiB blocks" '.block_size == 33554432' big.vhdx.json
jq_check "big.vhdx is 72 MiB" ".virtual_size == $BIG" big.vhdx.json

# get KEY answers one key, as an object; --text answers the bare value.
got="$(img.vhdx dynamic.vhdx get virtual_size --text)"
check "get virtual_size --text is $SIZE (got '$got')" test "$got" = "$SIZE"
img.vhdx small.vhdx get vhdx.logical_sector_size >key.json
jq_check "get vhdx.logical_sector_size is one key" \
    'keys == ["vhdx.logical_sector_size"] and .["vhdx.logical_sector_size"] == 512' key.json

# info and get are the same verb.
img.vhdx dynamic.vhdx get >get.json
img.vhdx dynamic.vhdx info >info.json
same "get and info report the same thing" get.json info.json

# --text is key: value lines, nested keys dotted.
img.vhdx dynamic.vhdx info --text >info.txt
check "info --text carries format: vhdx" grep -qx 'format: vhdx' info.txt
check "info --text dots the nested keys" grep -qx 'vhdx.logical_sector_size: 512' info.txt

expect_error "get of an unknown key" 2 img.vhdx dynamic.vhdx get no_such_key

finish
