# Changelog

All notable changes to GugleFS are documented here. Release descriptions use the section matching the application version.

## [Unreleased]

- Added complete English and Simplified Chinese interface resources with system-language detection and a persisted language switch.

## [0.10.0] - 2026-07-29

- Added backend-driven mount lifecycle events, including an explicit unmounting state used by startup recovery and app locking.
- Added per-mapping remote I/O limits, control/transfer timeouts, safe one-shot retries, and FTP session recovery.
- Improved keyboard operation, dialog focus, contextual screen-reader labels, and live mount/error announcements.
- Added non-sensitive unclean-exit detection with post-unlock mapping recovery guidance.

## [0.9.0] - 2026-07-29

- Added an authenticated remote directory browser for choosing FTP, FTPS, SFTP, and WebDAV mapping roots.
- Added sanitized JSONL operation logs with rotation and user-exportable JSON diagnostic reports.
- Added cross-platform SSH Agent authentication and verified OpenSSH `known_hosts` import for SFTP mappings.
- Added conditional WebDAV writes using ETag/Last-Modified validators, conflict detection, safe offset-zero writes, and nonzero truncation.
- Added WebDAV Digest, Bearer Token, anonymous, and local PEM client-certificate authentication.
- Added case-insensitive, case-preserving WinFsp lookup with collision detection, Windows filename validation, and basic attribute projection.

## [0.8.0] - 2026-07-29

- Added portable JSON mapping import and export; exported files contain no credential or private-key references.
- Added bilingual Release notes generation with a platform, architecture, format, and file download table.
- Added a repository threat model, security invariants, and contributor/release instructions.
- Fixed macOS FUSE-T CI linking and added a local compatibility pkg-config shim for `libfuse-t`.

## [0.7.0] - 2026-07-29

- Replaced the macOS macFUSE backend with the user-space FUSE-T 1.2.7 runtime.
- Bundled the verified official FUSE-T installer, license, and attribution files in macOS packages.
- Added runtime detection and an in-app installer action when FUSE-T is unavailable.
- Fixed macOS CI to link `fuser` directly against `libfuse-t` through repository-owned pkg-config metadata.

## [0.6.0] - 2026-07-28

- Added an optional system startup mode that launches GugleFS silently in the tray.
- Detects SFTP MFA requirements during setup and prevents unsupported automatic mounting for MFA mappings.
- Detects occupied Windows drive letters and selects the next available letter for new mappings.
- Adjusted the desktop window and mapping form for a denser 4:3 workspace.

## [0.5.0] - 2026-07-28

- Rebuilt the configuration interface with Vue 3 and a focused desktop control layout.
- Added mapping status cards, mount dialogs, persisted-auth shortcuts, and responsive empty/error states.
- Rewrote the English and Chinese README files with architecture, security, usage, and screenshot documentation.

## [0.4.0] - 2026-07-28

- Added manual SFTP MFA authentication with transient six-digit TOTP codes.
- Added SSH transport keepalives, silent SFTP session recreation, and safe reconnect behavior for non-MFA sessions.
- Improved mapping cards, application branding, and mount status presentation.

## [0.3.0] - 2026-07-28

- Added system proxy discovery and per-mapping proxy bypass across FTP, FTPS, SFTP, and WebDAV.
- Added local and pasted SSH private-key support with platform secure-store persistence.
- Fixed Windows directory mount conflicts and completed the first cross-platform release configuration.
- Introduced the sidebar-based mapping workspace and application icon.

## [0.2.0] - 2026-07-28

- Added Linux FUSE3 and macOS FUSE mounting alongside Windows WinFsp.
- Added macOS Keychain and Linux Secret Service credential storage.
- Added system tray operation, single-instance protection, safe exit unmounting, and failed-mapping recovery.
- Added bounded metadata/directory caches and per-handle sequential read-ahead.
- Added Windows, Linux, and macOS release packaging with unsigned macOS fallback.

## [0.1.0] - 2026-07-28

- Established the Tauri, Vue/TypeScript, and Rust workspace architecture.
- Implemented WebDAV, FTP/FTPS, and SFTP remote filesystem operations.
- Added WinFsp drive and directory mounts with Unicode WebDAV path handling.
- Added startup TOTP 2FA, secure credential persistence, and mount-state restoration.
- Added automatic prereleases and a Windows installer with the WinFsp runtime.
