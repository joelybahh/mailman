# Changelog

## v0.5.0 - 11/03/2026

### New

- **Persistent request tabs** — opening a request now focuses or creates its own tab, and the open tab set is restored on startup.
- **Draft-based request editing** — request changes now stay as per-tab drafts until explicitly saved, and dirty drafts are retained across app close / relaunch.

### Improved

- **Request workspace balance** — the request tab strip and sidebar request header now share a thinner, more continuous header treatment.

## v0.4.0 - 08/03/2026

### New

- **Scripts tab** — define post-response rules that extract JSON path values (e.g. `data.access_token`) into environment variables after any 2xx reply. Supports dot-notation and array indexing (`items.0.id`).
- **Persistent sessions** — "Keep me signed in" dropdown on the unlock screen caches the derived key in the OS keychain (macOS Keychain / Windows Credential Manager) for 1, 7, 14, 30 days, forever, or always-ask. Raw password is never stored.
- **Lock button** — padlock icon in the toolbar immediately wipes the key from memory, clears the keychain, and returns to the unlock screen.
- **App version in toolbar** — current semver is shown in the toolbar for quick reference.

### Improved

- **Non-blocking unlock** — Argon2id KDF now runs on a background thread. The password field, session picker, and button are disabled with a "Verifying…" spinner while in-progress — no more frozen window on login.
- **Sidebar auto-expands** — on launch the sidebar opens the collection/folder containing the previously selected endpoint automatically.
- **Scripts badge** — a pill in the response header shows how many script rules ran successfully on the last response.

---

## v0.3.0 - 08/03/2026

### New

- **macOS native title bar** — content extends behind the traffic-light buttons (`fullsize_content_view`); window title is hidden for a native Mac feel.
- **"Mailman" wordmark** — app name rendered in brand accent orange in the toolbar.
- **Status code pill badge** — HTTP status codes shown as a coloured translucent pill next to "Response".
- **Themed JSON syntax highlighting** — strings green, numbers amber, booleans blue, nulls muted grey in the Pretty view.
- **Inline previews on collapsed JSON nodes** — folded objects/arrays show a compact inline preview (truncated at 80 chars) without needing to expand.

### Improved

- **Single hero row** — method, URL, and Send button collapsed into one compact action bar.
- **Editable request name heading** — request name is a large (18pt) frameless inline field; rename is a single click.
- **Collection/folder breadcrumb** — shown as a compact muted line beneath the heading instead of a full row.
- **Pretty view is now the default** — responses open on the Pretty tab instead of Raw.
- **Consistent pointer cursor** — every interactive element shows the hand cursor on hover via a chainable `.cursor_hand()` trait.
- **Sidebar refresh** — new-request button is accent orange; delete is a quiet muted "−".
- **Spacing passes** — breathing room improvements across the request editor, toolbar, and env panel.

### Fixed

- **Double-click on Raw response was slow** — long single-line responses (e.g. minified JSON) caused egui's word-boundary scan to freeze for multiple seconds. Response body is now pre-chunked into ~400-char segments; double-click is instant regardless of size.
- **Raw view frame rate** — virtual scrolling (`show_rows`) means only visible chunks are laid out; large responses no longer impact frame time.
- **Per-frame char count freeze** — `attach_text_context_menu` called `chars().count()` on the full response body ~60× per second. Fixed with `usize::MAX` + egui clamping.
- **Broken env panel scroll area** — a stray drop-shadow frame was collapsing the scroll area.

---

## v0.2.0 - 06/03/2026

### New

- Right-click context menu support
- Pretty JSON renderer in the response panel

### Fixed

- Response UI now clears when switching between requests

---

## v0.1.0 - 05/03/2026

- First release — basic HTTP request functionality
