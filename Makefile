TARGET := wasm32-unknown-unknown
WASM   := target/$(TARGET)/release/grithub.wasm

.PHONY: all app test check clean

all: app

app: app.wasm

app.wasm: src/*.rs Cargo.toml
	cargo build --release --target $(TARGET)
	cp $(WASM) app.wasm
	python3 tools/package.py

test:
	cargo test

check:
	cargo check --all-targets
	cargo check --all-targets --target $(TARGET)

clean:
	cargo clean
	rm -f app.wasm
