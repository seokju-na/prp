# Webview Bundle release fixture

This small workspace models the release units in Webview Bundle: Cargo core,
private native bindings, nested npm platform packages, a JSR wrapper, a mixed
Cargo/npm Tauri package, an asset-only FFI package, and dependent JavaScript packages.

Publish commands are disabled in the checked-in configuration. The E2E harness
copies the fixture into a temporary Git repository, connects local services, and
runs the compiled PRP CLI. No package manager publishes and no public service is used.

See [the fixture guide](../README.md) for test commands and coverage.
