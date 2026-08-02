# Changelog

All notable changes to GugleFS are documented here. Release descriptions use the section matching the application version.

## [Unreleased]

## [0.16.0] - 2026-08-02

- Large SFTP directories now use cursor-based linear pagination. WinFsp and FUSE return the first available page without waiting for the complete directory, reuse stable per-handle snapshots, and clean up interrupted cursors; mount-time root prefetch no longer competes with foreground browsing. Longer bounded metadata caches reduce repeated property and thumbnail requests.
- Explorer metadata probes use the active directory name view instead of issuing serial SFTP metadata requests. Repeated enumeration restarts on one WinFsp handle reuse the existing snapshot and remote cursor while the directory is unchanged, and successful local create, remove, or rename operations refresh that snapshot. Directory paging no longer holds the main SFTP connection lock while waiting for the dedicated `READDIR` channel, so foreground metadata and file operations can continue concurrently.
- WinFsp volume information now returns a cached or compatibility capacity immediately and refreshes SFTP `statvfs` or WebDAV quota data asynchronously; Unix mounts retain cached remote capacity reporting.

## [0.15.1] - 2026-07-29

- FTP/FTPS temporary uploads now use non-hidden names for compatibility with servers such as Pure-FTPd that reject leading-dot file names.

## [0.15.0] - 2026-07-29

- Fixed WinFsp constrained-write and allocation-size semantics when overwriting existing files. SFTP now reads back every acknowledged write range and verifies the remote length after truncation; verification or transfer failures keep the file handle failed instead of silently producing a corrupt replacement.
- FTP/FTPS writes now upload to a same-directory temporary file, verify its remote length, and rename it over the target so a failed upload preserves the original file.

## [0.14.0] - 2026-07-29

- Prefetched the root directory asynchronously after mounting, coalesced concurrent reads of the same directory, and reused stable sorted snapshots across WinFsp pages; FTP now validates mapping roots with `CWD` instead of a full listing for root metadata.

## [0.13.0] - 2026-07-29

- Cached filesystem-space probes briefly and kept WebDAV quota `PROPFIND` requests separate from ordinary metadata requests to avoid slowing directory browsing.

## [0.12.0] - 2026-07-29

- Added optional startup and manual update checks with a GitHub Releases download link and a proxy fallback when the GitHub API is unavailable.
- Added platform-native WinFsp and FUSE callback consistency tests backed by the same in-memory remote filesystem scenario.
- Projected available remote creation, access, and modification timestamps into WinFsp and FUSE metadata instead of substituting the current time.
- Added real SFTP `statvfs` and WebDAV quota capacity reporting, while retaining a compatibility fallback only for protocols that do not expose filesystem space.
- Added FTP MLST/MLSD timestamp parsing and normalized WebDAV HTTP/RFC 3339 dates to Unix time without weakening conditional writes.

## [0.11.0] - 2026-07-29

- Added complete English and Simplified Chinese interface resources, including the native tray menu, with system-language detection and a persisted language switch.
- Added real FTP and SFTP container integration coverage for connection, directory, create, range read/write, rename, truncate, flush, and removal behavior.
- Added support for Pure-FTPd MLST/MLSD responses with four-digit UNIX mode facts.
- Documented the synchronous write-through contract and the implementation gates for chunking, staged replacement, bounded spill, and crash recovery.

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
