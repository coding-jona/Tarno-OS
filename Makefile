.POSIX:

.PHONY: build iso run run-gui toolchain clean \
        go-build go-test go-lint devuan-iso devuan-run

# ---- THOS kernel (primary) ----

build:
	cargo xtask build

iso:
	cargo xtask iso

run:
	cargo xtask run

run-gui:
	cargo xtask run --gui

toolchain:
	rustup target add x86_64-unknown-none
	rustup component add rust-src llvm-tools

# ---- frozen Devuan distribution (see FROZEN.md) ----

go-build:
	go build ./...

go-test:
	go test ./...

go-lint:
	golangci-lint run ./...

devuan-iso:
	./scripts/build-devuan-live.sh

devuan-run:
	./scripts/run-devuan-live.sh

clean:
	cargo clean || true
	rm -rf target/iso_root target/thos.iso boot.log
