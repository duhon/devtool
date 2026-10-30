# Release notes

## 3.0.0

- The Console is the only interface. The per-service forms, service picker and O button are
  removed; tabs saved by the old interface are not carried over.
- Removed the standalone HTTP interface (`services-devtools web`); the application runs only
  as the Chrome extension.
- Services are modules: REST, MySQL, GraphQL, gRPC, Splunk and Terminal ship as packages in
  `~/Library/Application Support/ServicesDevTools/data/modules` and load through the same
  module API as modules written by anyone else. An update replaces only the modules it ships.
- Settings → Modules lists the installed modules, installs a module from its package file
  (`.module.tgz`) and removes one. The console of a removed module is kept.
- Module SDK: `rust/module-api` and `rust/module-ui`, the example module and guide in
  `rust/examples/echo`, and `cargo run -p xtask -- package` to build package files.

## 2.6.7

- Removed the Refresh models button from Agent settings.

## 2.6.6

- Choose model and reasoning for new Agent chats in Settings; Copilot Fusion by default.

## 2.6.5

- Send with Enter; remove agent hints and author labels.

## 2.6.4

- Add full DOM inspection and per-page write approval.

## 2.6.3

- Unify Settings styling, status and update diagnostics.

## 2.6.2

- Improve agent page reading, search, context and interactions.

## 2.6.1

- Fix the macOS in-app updater using the signing and quarantine handling personally verified with local 2.5.26.
- After validating an approved archive, locally ad-hoc sign both staged native executables (`services-devtools` and `services-install`), remove only their `com.apple.quarantine`, verify their signatures and absence of quarantine, then check runtime health before activation. Preparing the next installer avoids leaving it quarantined for a subsequent update.
- Keep other files' quarantine metadata, archive/source/version/permission checks, user data and transactional rollback. No global macOS security settings are changed.
- Preserve private diagnostics at `data/update-local-signing.json` and `data/update-installer-signing.json`; signing, quarantine-removal and health failures stop installation instead of activating broken executables.
- Existing 2.6.0 and older installations still use their old installer. Install 2.6.1 manually once and reload the extension, or install the local 2.5.27 bootstrap first and update through Settings, to enable the corrected updater for both executables.
- Releases remain ad-hoc signed, not Developer ID signed or notarized. The updater intentionally removes quarantine from the two verified staged executables; managed-device policies can still restrict execution.

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
