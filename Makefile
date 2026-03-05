.PHONY: help check-packager bundle-mac bundle-linux bundle-win bundle-all

help:
	@echo "Available targets:"
	@echo "  make check-packager # Validate Cargo metadata + cargo-packager availability"
	@echo "  make bundle-mac    # Build macOS .app and .dmg"
	@echo "  make bundle-linux  # Build Linux .deb"
	@echo "  make bundle-win    # Build Windows .msi (WiX) and .exe (NSIS)"
	@echo "  make bundle-all    # Build all configured formats"

check-packager:
	@cargo metadata --format-version 1 --no-deps >/dev/null
	@cargo packager --help >/dev/null

bundle-mac: check-packager
	cargo packager --release --formats app,dmg

bundle-linux: check-packager
	cargo packager --release --formats deb

bundle-win: check-packager
	cargo packager --release --formats wix,nsis

bundle-all: check-packager
	cargo packager --release
