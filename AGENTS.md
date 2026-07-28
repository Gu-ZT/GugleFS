# Repository Instructions

## Architecture

- Keep the dependency direction `src -> Tauri IPC -> guglefs-core <- guglefs-remote / guglefs-mount`.
- Put protocol behavior in `crates/guglefs-remote`, shared filesystem semantics in `crates/guglefs-core`, and OS adapters in `crates/guglefs-mount`.
- Never expose passwords, private-key contents/passphrases, TOTP secrets/codes, or proxy credentials through persisted configuration, logs, errors, or IPC responses.
- Locking and safe application exit must unmount every filesystem created by the process. Do not weaken this invariant for UI responsiveness.
- Route mount lifecycle changes through `MappingManager` and emit the fixed `mapping-runtime` event after every transition. Keep mount tasks attached to the Tauri application lifecycle so Exit can still unmount them.

## Required Checks

Run these before committing:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
node --test scripts/update-release-notes.test.mjs
pnpm run web:build
git diff --check
```

On Windows, use the bundled WinFsp preparation script when the SDK is unavailable. On macOS, install FUSE-T and export `PKG_CONFIG_PATH="$PWD/scripts/pkgconfig/fuse-t${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"` before Rust builds.

## Documentation And Releases

- Before every commit, update `TODO.md` and both README files for behavior changed by that commit.
- Add user-visible changes to `CHANGES.md` and `CHANGES.zh_CN.md`. A release version must have a `## [x.y.z]` section in both files before it is pushed.
- Keep the version synchronized in the workspace `Cargo.toml`, `Cargo.lock`, `package.json`, and `src-tauri/tauri.conf.json`.
- Use the existing Conventional Commit style with concise Chinese subjects.
- Do not mark signing, notarization, protocol compatibility, performance, or mount acceptance tasks complete without evidence from the required platform or service.

## Platform Constraints

- WinFsp and FUSE behavior cannot be considered verified by unit tests on another operating system.
- FUSE-T is under its own license. Keep its installer checksum, license, and attribution files synchronized when upgrading it.
- Changes to credential identifiers require migration and cleanup logic for the Windows Credential Manager, macOS Keychain, and Linux Secret Service.
- Configuration exports must not contain credential IDs, pasted-key IDs, private-key paths, automatic-mount state that depends on credentials, or secret material.
