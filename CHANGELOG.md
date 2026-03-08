# Changelog

## v0.3.0 - 08/03/2026

### UI Rebrand & Visual Polish

- **macOS native title bar** — content now extends into the macOS title bar area (`fullsize_content_view`). The toolbar sits flush beside the traffic-light buttons exactly like a native Mac app; window title is hidden.
- **"Mailman" wordmark** — the toolbar now renders the app name in the brand accent orange rather than a plain system title.
- **Accent-orange Send CTA** — the Send button uses the same warm orange as the logo/login screen, making the primary action immediately obvious.
- **Method + URL + Send as a single hero row** — collapsed from a multi-row layout into one compact, prominent action bar at the top of the request editor.
- **Request name as an editable heading** — the selected request's name is shown as a large (18pt) frameless inline text field at the top, so renaming is just a click away.
- **Collection/folder breadcrumb** — collection and folder context is shown as a compact muted breadcrumb beneath the heading instead of taking up a full row.
- **Sidebar refresh** — new-request button is accent orange; delete button is a quiet muted "−" so the destructive action doesn't compete visually.
- **Status code pill badge** — HTTP status codes are displayed as a translucent coloured pill badge (coloured text on a tinted fill) instead of plain coloured text next to the "Response" heading.
- **Consistent pointer cursor** — introduced a `HandCursor` trait with a chainable `.cursor_hand()` method; every interactive element (buttons, tabs, list items, selectable values) now correctly shows the pointer cursor on hover.
- **Themed JSON syntax highlighting** — JSON leaf values in the Pretty view are colour-coded: strings in green, numbers in amber, booleans in blue, nulls in muted grey — all drawn from a centralised theme palette.
- **Spacing & breathing room** — additional spacing passes across the request editor tabs, toolbar, and env panel for a less cramped feel.

### Response Panel

- **Pretty view is now the default** — opening a response lands on the Pretty tab instead of Raw.
- **Inline previews on collapsed JSON nodes** — folded objects and arrays show a compact inline preview (e.g. `{ "id": 1, "name": "Alice" }`) truncated at 80 characters, so you can see contents without expanding.
- **Clean clipboard copy for JSON strings** — double-clicking or using "Copy Value" on a JSON string leaf strips the surrounding quotes, so you get `Gold` on your clipboard rather than `"Gold"`.
- **Root-level count labels preserved** — top-level objects and arrays still show their item/key count (`{} 12`, `[] 5`) for at-a-glance size awareness.

### Performance

- **Double-click on Raw response is now instant** — the previous implementation rendered entire response lines as single egui labels. On a minified JSON response (one line, potentially 100 000+ characters), egui's word-boundary scan on double-click was effectively O(line_length), causing the UI to freeze for multiple seconds. The fix pre-processes the response body into ~400-character chunks at natural token boundaries (whitespace, commas, `{`, `}`, `[`, `]`, `:`, `&`, `=`) when the response arrives. Double-click word selection is now O(chunk_size) regardless of response size.
- **Virtual scrolling for Raw view** — `show_rows` ensures only the visible chunks are laid out each frame, so even a 10 000-line response costs nothing to keep open while scrolling.
- **Removed per-frame O(n) char count** — `attach_text_context_menu` was calling `chars().count()` on the full response body every frame (≈60× per second). "Select All" now uses `usize::MAX` and lets egui clamp to the actual end, eliminating the freeze entirely.

### Refactoring & Code Health

- **Monolith split into focused modules** — the single 1 946-line `src/app.rs` was decomposed into `src/app/mod.rs` (state & logic) and a `src/app/ui/` module tree: `auth`, `dialogs`, `env_panel`, `request`, `response`, `shared`, `sidebar`, `status`, `theme`, `toolbar`. Each file has a single clear responsibility.
- **Centralised theme constants** — all colours (`ACCENT`, `ERROR`, `SUCCESS`, `WARNING`, `REDIRECT`, `MUTED`, JSON palette) live in `theme.rs` and are referenced by name everywhere else.
- **`status_code_color` helper** — HTTP status-code-to-colour mapping is a single function rather than duplicated `match` arms scattered through the UI.
- **Fixed broken scroll area on the env panel** — a stray drop-shadow frame was collapsing the env panel scroll area; the auth layout was also cleaned up at the same time.

## v0.2.0 - 06/03/2026

- Added right click functionality
- Added pretty json renderer mode in responses
- Clear repsonse UI when swapping between requests

## v0.1.0 - 05/03/2026

- First basic release imitating simple request functionality
