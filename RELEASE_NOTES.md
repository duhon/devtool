# Release notes

## 2.6.0

- Download approved public GitHub release archives directly through the native Rust runtime; Chrome no longer downloads the archive.
- Verify SHA-256 during download and again before installation, with bounded size, HTTPS redirects and download time.
- Keep downloaded files private and temporary without adding a quarantine marker. Preserve any already-existing macOS quarantine metadata through extraction and installation.
- Remove the extension's downloads permission. Save the workspace before updating, retain rollback checks and reload only after confirmed installation.
- Test the transition using the corrected local 2.5.24 bootstrap configured for duhon/devtool on main.
- This remains an updater preview for personal verification, not a confirmed end-to-end installation. Releases are not Developer ID signed or notarized; Gatekeeper can still block execution and is not bypassed.

## 2.5.23

- Preview for personal verification of the in-app updater; use the local 2.5.22 package as the initial installation.
- Check private GitHub Releases through the current Chrome session and show new versions in Settings.
- Notify about unseen updates with a green Settings indicator.
- Save the workspace, download and verify the approved archive, then install and reload after choosing Update.
- Preserve the existing installation if validation or the new runtime health check fails.
- This preview is not Developer ID signed or notarized. macOS Gatekeeper may block the automatic installation; no system protections are disabled.

## 2.5.21

- Add the application Settings panel with the installed version.
- Keep the selected History execution visible and clearly highlighted.
- Load large History snapshots automatically without a manual load-more button.
- Preserve session-specific panel sizes and text-only selection highlighting.
