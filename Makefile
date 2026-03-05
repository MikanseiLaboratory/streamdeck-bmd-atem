#! Local build & pack for Stream Deck plugin. 
#! Requires: cargo, rustup target (x86_64-pc-windows-gnu), mingw-w64, `streamdeck` CLI (npm i -g @elgato/cli).

.PHONY: build build-windows build-mac copy-binary pack

PLUGIN_DIR := dev.miseinul.atem.sdPlugin
TARGET_MAC  := x86_64-apple-darwin
TARGET_WIN  := x86_64-pc-windows-gnu

build-mac:
	cargo build --release --target $(TARGET_MAC)

copy-binary-mac: build-mac
	cp target/$(TARGET_MAC)/release/streamdeck-atem $(PLUGIN_DIR)/

copy-binary-windows: build-windows
	cp target/$(TARGET_WIN)/release/streamdeck-atem.exe $(PLUGIN_DIR)/

build-windows:
	cargo build --release --target $(TARGET_WIN)

build: build-windows build-mac

copy-binary: copy-binary-windows

pack: copy-binary
	mkdir -p dist
	streamdeck pack $(PLUGIN_DIR) --output dist --no-update-check -f

