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
	# ── Pre-flight cleanup ────────────────────────────────────────────────
	# Force-detach any lingering /Volumes/Mailman from a prior failed build.
	-diskutil unmount force /Volumes/Mailman 2>/dev/null || true
	-hdiutil detach "/Volumes/Mailman" --force 2>/dev/null || true
	# Remove leftover rw.* intermediates that Finder silently re-mounts.
	-rm -f dist/packager/rw.Mailman*.dmg
	# ── Spotlight exclusion ───────────────────────────────────────────────
	# Prevent mds from grabbing the freshly-mounted rw DMG and blocking the
	# unmount step inside create-dmg.  These marker files tell Spotlight and
	# Time Machine to leave the directory alone.
	@mkdir -p dist/packager
	@touch dist/packager/.metadata_never_index
	@touch dist/packager/.com.apple.timemachine.donotpresent
	# Kick off a background job that touches the Spotlight marker inside the
	# mounted volume the moment it appears, before mds can open any files.
	@bash -c 'while ! test -d /Volumes/Mailman; do sleep 0.2; done; \
	  touch /Volumes/Mailman/.metadata_never_index 2>/dev/null; \
	  sudo mdutil -i off /Volumes/Mailman 2>/dev/null || true' & \
	disown
	# ── Build ─────────────────────────────────────────────────────────────
	cargo packager --release --formats app,dmg

bundle-linux: check-packager
	cargo packager --release --formats deb

bundle-win: check-packager
	cargo packager --release --formats wix,nsis

bundle-all: check-packager
	cargo packager --release
