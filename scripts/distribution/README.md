# Releases

## Automatic releases from `main`

`.github/workflows/devtool-release.yml` runs on pushes to `main`; these
scripts are its steps (`release.py`, tested by `test_release.py`).
`RELEASE_NOTES.md` controls releases: add a new `## X.Y.Z` section at the top,
followed by older versions in descending order. The workflow sets the Cargo
and Chrome versions from that section without committing them back.

An already published version is skipped, so ordinary pushes do not rebuild or
overwrite a release; to ship another build, add a higher version. The workflow
uses a macOS ARM64 runner, runs the Rust and packaged-installation checks and
the browser scenarios (`cargo run -p xtask -- test browser --jobs 2`), then
publishes `devtool-vX.Y.Z` as a GitHub Release with
`services-macos-arm64.tar.gz`, `checksums.txt`, `install.sh` and
`version.json`; its description is the matching notes. A failing scenario
blocks the release; run the browser scenarios locally before pushing. Assets
are uploaded to a draft before publication and are never replaced.

The package and update metadata use the current repository and `main` as their
source. The updater also accepts the legacy `devtool` channel, but never
switches repositories or channels automatically. Public releases in
`duhon/devtool` need no GitHub authentication.

The build bakes the repository and version into `install.sh`, so piping
`releases/latest/download/install.sh` to Bash downloads the checksum and
archive of that same release:

    curl -fsSL https://github.com/OWNER/REPO/releases/latest/download/install.sh | bash

`release.py metadata` refuses to publish an installer bound to another release.
The generated archive contains no private keys, Variables or other user data.
