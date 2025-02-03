#!/bin/zsh

cargo skyline build --release || {
    echo "Failed to build ult_logger"
    exit 1
}

cp -v \
    "${HOME}/Developer/ult_logger/target/aarch64-skyline-switch/release/libult_logger.nro" \
    "${HOME}/Library/Application Support/Ryujinx/sdcard/atmosphere/contents/01006a800016e000/romfs/skyline/plugins/libult_logger.nro"

/Applications/RyuJinx.app/Contents/MacOS/Ryujinx \
    "${HOME}/Games/Smash Ultimate/Super Smash Bros. Ultimate v0 (01006A800016E000) (BASE).nsp"