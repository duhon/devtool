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

Updates are installed from inside the application: **Settings** →
**Check for updates** → **Update**.

Uninstall, then remove the extension in Chrome (your data is kept):

```sh
"$HOME/Library/Application Support/ServicesDevTools/uninstall"
```

## Extensions

Every service in the panel is a module, and you can write your own. Install it
in **Settings** → **Modules** → **Install from file…**. How to write and package
a module: [rust/examples/echo/README.md](rust/examples/echo/README.md).

---

[Development](DEVELOPMENT.md) · [Installer details](rust/installer/README.md)
