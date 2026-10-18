# CLI fixtures

`monorepo/` is a checked-in workspace with real manifests and minimal source files.
It has no `.git` directory or generated lockfiles. Tests copy the complete tree,
including dotfiles, into a fresh temporary directory and create their own commits,
tags, and merge history. The checked-in fixture is never used as the CLI's working
directory.

## Workspace layout

| Directory | Manifests | Dependencies / behavior |
| --- | --- | --- |
| `packages/core` | npm | Shared JavaScript package |
| `packages/app` | npm | Workspace dependencies on core and bridge |
| `packages/bridge` | npm, Cargo, JSR | Depends on npm core, Cargo core, and JSR utils |
| `packages/bridge/npm/linux-x64` | npm | Nested platform package, exact optional dependency of bridge |
| `packages/bridge/internal` | npm (private) | Versioned with bridge; excluded from publishing and registry links |
| `crates/core` | Cargo | Shared native crate, renamed to `native_core` in workspace dependencies |
| `crates/consumer` | Cargo | Inherits workspace version and dependency alias |
| `deno/utils` | JSR | Shared Deno package |

Root `package.json`, `Cargo.toml`, and `deno.json` declare their workspaces.
`.prp/config.toml` leaves `packages.include` empty to exercise automatic discovery.
Bridge's manifests are also discovered automatically, including its nested packages.
Its `display_name`, artifact collection/restore paths, and release assets are configured.

`.prp/changes/initial.md` belongs to the initial test commit. Tests create subsequent
notes through `prp change` and verify selection and consumption independently.

## Scenarios

Run from the PRP repository root:

```sh
cargo test --test fixture_cli
```

The integration suite runs the compiled `prp` binary and checks:

- npm/Cargo/JSR workspace discovery, transitive dependency candidates, and read-only dry runs.
- `change` → `prepare-release --skip-pr` → merge, selected notes, nested version bumps,
  dependency ranges, root/package changelogs, registry links, and retained notes.
- Cargo workspace dependency aliases, inherited versions, and preserved comments.
- A merge with two parents → `release`, dependency ordering, artifact collection and restore,
  hooks, registry existence checks, private package exclusion, and tags on the merge commit.
- Failed dependency publication stopping before downstream publishes, hooks, and tags.

`tests/support/mod.rs` provides shared temporary repository and CLI helpers for both
the fixture suite and the smaller cases in `tests/cli.rs`.

The publication scenarios require Unix `sh` and are guarded by `cfg(unix)`.
They replace the disabled publication commands with `tools/publish.sh`, a recorder
that never runs a package manager. `tools/before-publish.sh` checks the restored
artifact. All registry requests go to a local mock HTTP server, and GitHub/push are
disabled. Other scenarios require only Git and the compiled CLI.

## Stateful release fixture

`webview-bundle/` models a mixed native workspace. It uses fixture package names
and contains no native build toolchain dependencies. Its checked-in config has
disabled publishers and loopback-only endpoints. Do not run it in place; the
test harness copies it into a temporary Git repository.

```sh
cargo test --locked --test release_e2e
```

The fixture has 10 release units and 12 public manifests: 9 npm packages, 2 Cargo
crates, and 1 JSR package. It includes:

- Core Cargo crate and Tauri dependent using a workspace dependency alias.
- Node wrapper, private Cargo manifest, two nested npm platform packages, and
  an Electron dependent.
- Deno JSR package with a private Cargo manifest, dynamic libraries, and checksum
  output.
- FFI package published only as a tag and GitHub Release with three archives.
- Config and remote packages with a transitive dependency chain.
- A create CLI whose nested app templates have independent versions, duplicate
  names, and a Cargo manifest. An explicit `manifests = ["package.json"]` keeps
  those template files out of publication.

`tests/support/release_world.rs` owns the local services and failure controls.
`tests/release_e2e.rs` supplies an independent expected inventory and invokes the
compiled CLI through `std::process::Command`. Every scenario owns a temporary
working repository, a **real bare Git remote**, and a stateful loopback HTTP
server. Git user configuration, hooks, and environment overrides are isolated.

The publisher is a subprocess of the integration test binary, launched through
`tools/mock-publish.sh`. The ignored `mock_publish` test is its entry point; it is
not an omitted scenario. It reads actual manifests and package files, validates
dependency availability, and writes atomic registry receipts with the version,
source commit, working directory, arguments, and file bytes. HTTP registry
lookups read those receipts. GitHub mocks retain releases and uploaded bytes and
reject duplicate creation/upload attempts. No package manager, external registry,
external Git remote, or real GitHub account is used.

`World::build` simulates separate platform jobs by collecting each package's
partial output with `prp artifacts --package`, then removing the original files.
Release must restore them and distribute the Node binaries before publication.
The test checks that published and uploaded binary bytes match the collected
input. These files model artifact transport; they are not usable native binaries
or valid ZIP archives.

The suite verifies:

- Initial publication, exact-version registry selection, dry-run/check side effects,
  dependency order, private/template exclusion, and all tags at the source commit.
- `prepare` → squash merge → detached checkout → release, including rejection of
  a later checkout with `--expected-head`.
- Partial package publication and retry, staged npm duplicate rejection, and
  downstream publication stopping after failure.
- Registry authentication/server errors and malformed Cargo index data before
  any publication.
- Conflicting tags, remote-only annotated tags, and recovery after a real Git
  pre-receive hook rejects tag pushes.
- GitHub creation/upload failures, reuse of existing releases, and uploading only
  missing assets.
- Required missing assets, uncommitted source, and artifacts/hooks that overwrite
  committed inputs.

Extend `World` for new service behavior and add scenarios that assert observable
registry receipts, remote refs, asset bytes, and repository state. Keep failure
injection local to each world. Do not replace CLI calls with internal function
calls: this suite is the black-box release contract. The suite currently runs on
Unix because its fixture adapters use `sh`; Windows execution remains unverified.
