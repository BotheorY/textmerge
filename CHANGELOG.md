# Changelog

## 1.0.1 — 2026-10-16

- Fix controls being erased or covered after maximizing and restoring the window.
- Keep group-box frames behind the fields and buttons they surround, with
  sibling painting clipped correctly.
- Apply child positions together and invalidate all child clients and borders
  after resizing; discard stale pixel copies and avoid unnecessary font resets.
- Skip layout while minimized and retain the final layout after DPI changes.
- Add a native, off-screen regression test covering repeated resize/restore,
  DPI changes, repaint invalidation, and preservation of values and progress.

## 1.0.0 — 2026-10-16

- Initial MIT-licensed portable Windows release with disk-backed exact
  deduplication, four merge orders, Unicode case folding, UTF-8 output,
  pause/cancel, registry settings, and error logging.
