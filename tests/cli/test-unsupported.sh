# A verb the library cannot do still exists and says so: exit status 3, a
# structured error beginning `not implemented`, and the image untouched.
source "$(dirname "$0")/lib.sh"

cd "$SANDBOX" || exit 1
qemu-img create -q -f vhdx disk.vhdx 8M
cp disk.vhdx before.vhdx

# not_implemented DESCRIPTION COMMAND...
not_implemented() {
    local what="$1"
    shift
    expect_error "$what" 3 "$@"
    jq_check "$what: the error says not implemented" \
        '.error | startswith("not implemented")' "$SANDBOX/error.json"
}

not_implemented "resize" img.vhdx disk.vhdx resize 16M
not_implemented "set" img.vhdx disk.vhdx set virtual_size 16M
not_implemented "write" img.vhdx disk.vhdx write --offset 0 </dev/null
not_implemented "create" img.vhdx new.vhdx create 8M
same "no refused verb changed the image" disk.vhdx before.vhdx
check "the refused create made no file" test ! -e new.vhdx

# --text turns the error into a line for a person, with the same status.
img.vhdx disk.vhdx resize 16M --text 2>resize.txt
check "resize --text exits 3" test $? -eq 3
check "resize --text says not implemented" grep -q 'img.vhdx: not implemented' resize.txt

finish
