# Installer

`services-install` installs, updates and uninstalls Services DevTools for the
current user, without `sudo`, Node.js, Rust or the source repository. The root
`install.sh` and the published `install.sh` forward to it.

## Install

The installer finds the matching archive and checksum beside itself, whatever
the current working directory. If none is there (or the script is piped to
Bash), it downloads the checksum and archive from its own GitHub release
(`devtool-vX.Y.Z`, baked in at build time) and checks SHA-256. `--archive`
overrides the archive location.

Releases in a private repository require GitHub access: download the archive,
checksum and installer through your authorized browser into one directory and
run `bash install.sh` there. Piping a private release URL to Bash is not
supported.

Updates replace the installed copy; no release history is kept. Variables and
browser-stored settings are not replaced; do not remove the extension in
Chrome. After an update, reopen panels.
When upgrading an older installation whose `extension` folder was a symlink,
select that folder once more through **Load unpacked**, without removing the
existing extension; later updates need only **Reload**.

## Native-host registration

The installer registers `com.chrome_extentions.host` in Chrome's per-user
`NativeMessagingHosts` directory. Ordinary Chrome profiles and incognito
windows need nothing else. Running Chrome, Chrome for Testing and Chromium
instances with an explicit absolute `--user-data-dir` inside your home
directory are registered too, without admin rights; keep such a browser open
while installing, and run the installer again after creating a new custom
directory.

Existing registrations pointing to this installation in Chrome-family
directories under `~/Library/Application Support/Google/` and in Chromium's
standard directory are adopted, including earlier manual registrations.
Registered custom directories are remembered across updates and cleaned up on
uninstall, even if their browsers are closed. A pre-existing registration
replaced by the installer is restored on uninstall; registrations changed by
another installation are left untouched. User data is retained on uninstall.

## Gatekeeper

The distribution is not Apple-notarized. Gatekeeper or corporate policy may
restrict installation or execution. No global macOS security settings are
changed. Dependency licenses accompany the distribution.

## In-app updates

The application is the first row of Settings → Modules. Any source offers it
like a module: GitHub releases `devtool-vX.Y.Z` with their `version.json`, or
`distribution/` of a folder (`version.json`, `release-notes.txt` and the
archive). A newer version from the source the application came from turns the
Settings indicator green. Checks never install automatically: **Update** saves
the workspace, has the native runtime download the archive (or take it from the
folder), verifies the SHA-256 and package identity, installs transactionally
and reloads the extension. A private repository uses the token saved on its
source in Settings → Sources, else `gh auth token`; browser credentials are not copied
into the native process.

Downloads use private temporary files, a five-minute deadline and a 2 GiB
limit; redirects stay on approved HTTPS GitHub asset hosts. The downloader
adds no quarantine marker; an existing one is retained through extraction and
staging. After package validation the updater locally ad-hoc signs the staged
`services-devtools` runtime and `services-install` updater, removes only their
`com.apple.quarantine` attributes, verifies both signatures and the absence of
quarantine, and checks runtime health before activation. It does not clear
attributes recursively, touch the archive or other files, or provide Developer
ID signing or notarization. Diagnostics are saved under
`~/Library/Application Support/ServicesDevTools/data/` as
`update-local-signing.json` and `update-installer-signing.json`.

The updater rejects downgrades, unsafe archives and new Chrome permissions; releases needing more permissions must be installed
manually. Stop active operations and close other Services DevTools windows
before updating.

## Tests

`cargo run -p xtask -- test install` (`installer/tests`) installs,
updates, rolls back and uninstalls in a temporary HOME, launching the installed
binaries with a system-only PATH. After `cargo run -p xtask -- build`,
`cargo test -p services-installer --test packaged -- --ignored` checks the
built archive the same way. Neither touches your installed Chrome
registration.
