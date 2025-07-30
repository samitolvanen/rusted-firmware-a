#!/bin/bash

# Copyright The Rusted Firmware-A Contributors.
#
# SPDX-License-Identifier: BSD-3-Clause


case "$PLAT" in
  qemu)
    make -C $TFA PLAT=qemu FVP_TRUSTED_SRAM_SIZE=512 CC=clang NEED_BL32=yes NEED_BL31=no DEBUG=1 \
        bl1 bl2
    make PLAT=qemu DEBUG=1 qemu
    ;;
  fvp)
    if [[ "${RME:-}" == 1 ]]; then
        make PLAT=fvp FEATURES=sel2,rme DEBUG=1 all
        make -C $TFA PLAT=fvp FVP_TRUSTED_SRAM_SIZE=512 ENABLE_RME=1 NEED_BL31=no DEBUG=1 \
            BL31="$(pwd)/target/bl31.bin" BL32="$(pwd)/target/bl32.bin" \
            BL33="$(pwd)/target/bl33.bin" all fip
        make PLAT=fvp FEATURES=sel2,rme DEBUG=1 fvp
    else
        make PLAT=fvp DEBUG=1 all
        make -C $TFA PLAT=fvp FVP_TRUSTED_SRAM_SIZE=512 SPD=spmd SPMD_SPM_AT_SEL2=0 DEBUG=1 \
            BL31="$(pwd)/target/bl31.bin" BL32="$(pwd)/target/bl32.bin" \
            BL33="$(pwd)/target/bl33.bin" all fip
        make PLAT=fvp DEBUG=1 fvp
    fi
    ;;
  *)
    echo "PLAT ${PLAT} is not supported by this script."
    exit 1
    ;;
esac
