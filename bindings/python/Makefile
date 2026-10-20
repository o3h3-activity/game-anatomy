TARGET ?= x86_64-unknown-linux-gnu
CARGO_TARGET_DIR ?= ../../target
OUT_DIR ?= out

.PHONY: build
build:
	mkdir -p $(OUT_DIR)
	CARGO_TARGET_DIR=$(abspath $(CARGO_TARGET_DIR)) PYO3_BUILD_EXTENSION_MODULE=1 PYO3_NO_PYTHON=1 cargo build --locked --release --target $(TARGET)
	cp $(CARGO_TARGET_DIR)/$(TARGET)/release/libgame_anatomy.so $(OUT_DIR)/game_anatomy.abi3.so
