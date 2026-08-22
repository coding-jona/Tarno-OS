.POSIX:

.PHONY: build test lint devuan-iso devuan-run clean

build:
	go build ./...

test:
	go test ./...

lint:
	golangci-lint run ./...

devuan-iso:
	./scripts/build-devuan-live.sh

devuan-run:
	./scripts/run-devuan-live.sh

clean:
	cd tarno-devuan-live && (command -v sudo >/dev/null 2>&1 && sudo lb clean || doas lb clean)
