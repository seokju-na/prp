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
