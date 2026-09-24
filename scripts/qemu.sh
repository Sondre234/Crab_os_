#!/usr/bin/env bash
# Cargo runner: boot a CrabOS .efi image under QEMU with OVMF firmware.
# Test binaries (named <crate>-<16 hex digit hash>.efi) run headless and map the
# isa-debug-exit success code to 0; everything else opens a display.
set -euo pipefail

if [[ $# -lt 1 ]]; then
    printf 'Usage: %s <image.efi> [qemu args...]\n' "$0" >&2
    exit 2
fi

image="$(realpath "$1")"
shift
ovmf_code="${OVMF_CODE:-/usr/share/edk2/x64/OVMF_CODE.4m.fd}"
ovmf_vars="${OVMF_VARS:-/usr/share/edk2/x64/OVMF_VARS.4m.fd}"

work="$(mktemp -d "${TMPDIR:-/tmp}/crab_os.XXXXXX")"
trap 'rm -rf "$work"' EXIT
install -Dm0644 "$image" "$work/esp/EFI/BOOT/BOOTX64.EFI"
cp "$ovmf_vars" "$work/vars.fd"

qemu=(
    qemu-system-x86_64
    -machine q35
    -cpu max
    -m 256M
    -drive "if=pflash,format=raw,readonly=on,file=$ovmf_code"
    -drive "if=pflash,format=raw,file=$work/vars.fd"
    -drive "format=raw,file=fat:rw:$work/esp"
    -device isa-debug-exit,iobase=0xf4,iosize=0x04
    -serial stdio
)

if [[ $(basename "$image") =~ -[0-9a-f]{16}\.efi$ ]]; then
    status=0
    timeout 300 "${qemu[@]}" -display none -nic none -no-reboot "$@" || status=$?
    # isa-debug-exit reports (code << 1) | 1; QemuExitCode::Success is 0x10.
    if [[ $status -eq 33 ]]; then
        exit 0
    fi
    printf 'qemu exited with status %d\n' "$status" >&2
    exit 1
fi

"${qemu[@]}" -nic user,model=e1000 "$@"
