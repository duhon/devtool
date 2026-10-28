# Services DevTools

A Chrome DevTools **Services** panel and standalone extension page for
MySQL, REST, GraphQL, gRPC, Terminal and Splunk. Requests run through a
native-messaging host that Chrome starts automatically. No local web server
or manually started background service is required.

Console 2.0 adds an embedded **Agent** panel with Copilot chat-session tabs.
It works against the live editor, preserves Undo, and requires one-time approval
before executing a check. Chat is optional: install and authenticate GitHub
Copilot CLI separately to use it; the service clients do not depend on Copilot.

The right **History** panel follows the highlighted check and keeps immutable
execution snapshots with resolved requests and masked request credentials.
Response bodies are kept verbatim, without extra transport/debug wrappers.
Check identities survive edits and named-session switches; copied checks get
new identities. Embedded SQLite needs no separate installation and shares
history between the extension and HTTP interface. Its global payload quota is
100 MiB; reaching the quota removes oldest executions toward 50 MiB. SQLite
metadata and temporary transaction journals take additional disk space.

## Install: macOS 13.5+ Apple Silicon

The Rust distribution contains native executables and a Rust/WebAssembly
interface. Recipients do not need Node.js, npm, Rust, the source repository,
or `sudo`. JavaScript is limited to generated WASM/browser bootstrapping
and vendored editor dependencies.

The Console's right-hand **Settings** panel shows the current application
version and available releases, with an explicit Update action.

The publisher hosts three generated files in the root of a public GitHub
branch: `install.sh`, `checksums.txt`, and `services-macos-arm64.tar.gz`.
After that branch has been published, the installation command is:

```sh
# Replace OWNER/REPO and the branch with the actual published location.
curl -fsSL https://raw.githubusercontent.com/OWNER/REPO/distribution/install.sh | bash
```

Review the script before executing it if preferred. No repository/remote is
configured by default; the example above is not a live download URL.

The installer downloads the checksum and archive from one immutable commit,
checks SHA-256, and installs per-user into:

```text
~/Library/Application Support/ServicesDevTools/
  extension/    # one installed copy of the browser UI and native/ executables
  data/         # Variables, hints, templates and history; retained across updates/uninstall
  uninstall
  dev           # installed CLI, without Cargo or Node.js
```

It also registers `com.chrome_extentions.host` in Chrome's per-user
`NativeMessagingHosts` directory. Running Chrome, Chrome for Testing and
Chromium instances with an explicit absolute `--user-data-dir` inside your home
directory are registered automatically too, without admin rights or extra
commands. Keep such a browser open while installing. If you create a new
custom directory later, run the same installer again with that browser open.
Ordinary Chrome profiles and standard incognito windows need no separate
native-host setup.

**One manual Chrome step:** open `chrome://extensions`, enable **Developer
mode**, choose **Load unpacked**, and select
`~/Library/Application Support/ServicesDevTools/extension`. Keep this folder in place.

Open DevTools → **Services**, or open this URL in a regular Chrome tab:

```text
chrome-extension://ckicpgdfkgcmgnclbfbcbkillcpaffnb/panel.html
```

For updates, run the installer again, reload the extension in
`chrome://extensions`, and reopen its panels. Variables and browser-stored
settings are not replaced; do not remove the extension in Chrome.
When upgrading an older installation whose `extension` folder was a symlink,
select that folder once more through **Load unpacked**, without removing
the existing extension. This switches Chrome from the old release path to the
permanent directory; subsequent updates only need **Reload**.
Updates replace the installed copy; no release history is kept.
After an update, reopen panels and restart any previously running CLI/web processes.
To uninstall the host/runtime:

```sh
"$HOME/Library/Application Support/ServicesDevTools/uninstall"
```

Then remove the extension in Chrome. User data is retained. A pre-existing
native-host registration replaced by the installer is restored on uninstall.
Automatically registered custom directories are remembered across updates
and cleaned up on uninstall, even if their browsers are closed. Registrations
changed by another installation are left untouched.
Existing registrations pointing to this installation in Chrome-family
directories under `~/Library/Application Support/Google/` and in Chromium's
standard directory are adopted too, including earlier manual registrations.

The application distribution is not Apple-notarized. Gatekeeper or corporate
policy may restrict installation/execution. The installer does not disable
macOS security checks. Dependency licenses accompany the distribution.

## Build and publish

On a macOS Apple Silicon development machine with Rust/Cargo:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.108 --locked
cargo run -p xtask -- build --repository OWNER/REPO --branch distribution
```

This produces the three publishable files in `dist/distribution/`. For manual
branch-based hosting, copy their contents to the **root** of a separate
distribution branch and commit/push them together. Never publish private keys,
Variables or other user data.
The generated archive contains neither IWA prototypes nor private keys.

### Automatic releases from `main`

`.github/workflows/devtool-release.yml` runs on pushes to `main`.
The generated package and update metadata use the current repository and
`main` as their source. The updater also accepts the legacy `devtool` channel,
but never switches repositories or channels automatically.
`RELEASE_NOTES.md` controls releases: add a new `## X.Y.Z` section at the top
with its release notes, followed by older versions in descending order.
The workflow prepares the Cargo and Chrome versions from that section without
committing generated version changes back to the branch.

An already published version is skipped, so ordinary code pushes do not
rebuild or overwrite a release. To ship another build, add a higher version.
The workflow uses a macOS ARM64 runner, runs Rust and packaged installation
checks, then publishes `devtool-vX.Y.Z` as a GitHub Release.
Run the headless UI checks locally before pushing a new release:
`cargo run -p xtask -- test-browser`. CI does not download Chrome or run browser
tests, to reduce runner-minute usage.
The release includes `services-macos-arm64.tar.gz`, `checksums.txt`,
`install.sh` and `version.json`; its description comes from the matching notes.
Assets are uploaded to a draft before publication. Failed builds do not
publish an update, and published assets are normally never replaced.
The 2.6.0 personal-testing preview is an explicit exception: it was rebuilt
to remove the downloader-added quarantine marker.

Public releases in `duhon/devtool` do not require GitHub authentication.
Releases in a private repository require GitHub access. For those, download
the archive, checksum and installer through your authorized browser into the
same directory, then run `bash install.sh` there. The installer's existing
remote fallback still uses branch-based public downloads; piping a private
Release URL to Bash is not supported.

### In-app updater preview

Version 2.6.0 is a preview for personal verification. Install the local 2.5.24
bootstrap containing the native downloader first, then use Settings to test
the update to 2.6.0 after the GitHub release is published.
The bootstrap is in `dist/native-bootstrap-2.5.24/`; its installer must be run
manually, followed by reloading the installed extension in Chrome.
Earlier local bootstraps add a quarantine marker themselves and must be
replaced before retrying this scenario.
CI derives published versions from `RELEASE_NOTES.md`.

The preview is not Developer ID signed or notarized. macOS Gatekeeper blocked
the new runtime in the isolated Chrome update scenario. A data-only diagnostic
also confirmed that Chrome-origin file creation receives quarantine metadata,
unlike the terminal-origin baseline. Personal acceptance of the full update is
pending; system protections are not disabled or bypassed.

Install the update-capable version manually once and reload the unpacked
extension in Chrome. It accesses GitHub release metadata but requires neither
the downloads permission nor cookie access. The native downloader supports
public release assets without GitHub sign-in, `gh`, Rust or an API token.
Private assets must be installed manually; browser credentials are not copied
into the native process.

The extension checks for a newer release at startup. An unseen update turns
the Settings indicator green; Settings shows the current and available
versions and a **Check for updates** action. Checks never install automatically:
click **Update** to save the workspace, download through the native runtime,
verify the SHA-256 and package identity, install transactionally, and reload
the extension. Downloads use private temporary files, a five-minute deadline
and a 2 GiB limit; redirects stay on approved HTTPS GitHub asset hosts.
The native downloader does not add a quarantine marker. Any already-existing
quarantine is retained through extraction and staging, never removed.
Failed downloads, access errors, and installation
failures are shown explicitly. A published release must exist before checks
can succeed.

Stop active operations and close other Services DevTools windows before updating.
The installed updater rejects downgrades, unsafe archives, different update
sources, and new Chrome permissions. Releases requiring additional permissions
need manual installation. The standalone HTTP interface cannot use Chrome's
native updater.

To build and verify locally without a GitHub location:

```sh
cargo run -p xtask -- build
cargo test -p services-installer --test packaged -- --ignored
cargo run -p xtask -- test-browser
./install.sh
```

The root `install.sh` forwards to the built installer; no archive path is needed.
The installer automatically finds the matching archive and checksum beside
itself, regardless of the current working directory. If no matching archive
is present beside a file-based installer (or the script is piped to Bash),
it downloads the package from the configured GitHub branch. `--archive`
remains an optional override for custom archive locations.

The Rust clean-install tests use a temporary HOME and launch the installed
binaries with a system-only PATH. They exercise the archived host, update
and uninstall without modifying your installed Chrome registration.
Browser acceptance uses Chrome DevTools Protocol directly from Rust, without
Node.js, npm or Playwright. Install Chrome for Testing or Chromium and set
`CHROME_BIN` to its executable if it is not installed in `/Applications`.
An existing cached Chromium executable is also supported; no browser is
downloaded automatically.

For extension-only development see [the extension guide](adapters/extension/README.md).
The published public key fixes the extension ID across clean builds; no
private key is required for an unpacked distribution.

## Other development adapters

The CLI and HTTP adapter remain available for development; neither is needed
by the installed Chrome application:

```sh
./dev rest --method=GET --endpoint=https://httpbin.org/get
cargo run -p services-native -- web
```

After installing, the CLI can also run without Cargo:

```sh
"$HOME/Library/Application Support/ServicesDevTools/dev" rest --method=GET --endpoint=https://httpbin.org/get
```

See `adapters/cli/README.md` for the full flag reference per service.

## Test

    cargo test --workspace

Rust service, UI model, native-host, CLI, installer and bootstrap tests live
with their respective crates. Captured JSON parser/formatter fixtures are
retained; the old implementation is not needed to run them.

After `cargo run -p xtask -- build`, run the two acceptance commands above.
`test-browser` opens the archived Rust interface in an isolated Chrome profile
and exercises all nine views, real Native Messaging/HTTP requests, Terminal,
Variables, contextual completions and clipboard restrictions. Browser-only
JavaScript fixtures run inside Chrome; the test driver and fixture server
are Rust. The clean-install browser check also verifies registration of an
already-running custom Chrome without a browser restart. All test installations,
environment files and Variables are disposable.

## Rust architecture

The dependency direction remains **adapters → registry/core → service
clients**. Concrete libraries are assembled by the runtime rather than
being wired into the browser or each transport.

- `rust/contracts/` — shared service/parameter metadata and protocol constants.
- `rust/native/` — the core, six service clients, composition root and
  native-messaging/CLI/HTTP adapters.
- `rust/ui/` — browser UI, state, parsing and rendering compiled to WebAssembly.
- `rust/ui/assets/` — shared styles and vendored CodeMirror files.
- `rust/installer/` — install/update/rollback/uninstall, without a script runtime.
- `rust/xtask/` — extension and macOS distribution builds.
- `adapters/extension/src/` — Chrome manifest, HTML and minimal browser bootstrap.
- `install.sh`, `dev` — root shell entry points.
