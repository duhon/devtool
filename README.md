# Services DevTools

A Chrome DevTools panel for developers who work with backend services. Send
REST, GraphQL and gRPC requests, run MySQL queries, search Splunk, look into
ActiveMQ Artemis queues and messages, and run terminal commands right next
to the page you are debugging, with shared variables, saved checks, templates
and request history in one place.
macOS 13.5+ on Apple Silicon.

<!-- Demo GIF goes here. -->

## Install, update, uninstall

Install the latest release:

```sh
curl -fsSL https://github.com/duhon/devtool/releases/latest/download/install.sh | bash
```

Then follow the installer's instructions to add the extension to Chrome once,
and open DevTools → **Services**.

The installer installs the application only. Add the services you need in
**Settings** → **Modules**: the standard ones come from this repository's
releases.

Updates are installed from inside the application: **Settings** →
**Modules** lists the application (its first row) and every module with their
sources, versions and newer versions; **Update** installs one.

Uninstall, then remove the extension in Chrome (your data is kept):

```sh
"$HOME/Library/Application Support/ServicesDevTools/uninstall"
```

## Extensions

Every service in the panel is a module, and you can write your own. Modules
are installed from sources only: put the package file in `modules/` of a folder
source and install it in **Settings** → **Modules**. How to write and package a
module: [rust/examples/echo/README.md](rust/examples/echo/README.md).

A team can share modules and settings through **Settings** → **Sources**: add a
GitHub repository or a local folder, then install its modules and apply its
Dictionary profiles, templates, hints and Agent skills. A source is laid out as

```text
dictionary.json          {"profiles": {"<profile>": {"<VAR>": "value"}}}
templates.json           {"<module>": {"<section>": "<templates text>"}}
hints/<module>.md        a module's hints
skills/<name>/SKILL.md   Agent skills
modules/*.module.tgz     module packages (a GitHub repository publishes them as
                         releases tagged module-<id>-v<version>)
```

Your own changes stay on top of a source's and survive its updates; **Reset**
returns to the source's.

---

[Development](DEVELOPMENT.md) · [Installer details](rust/installer/README.md)
