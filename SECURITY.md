# Security Policy And Threat Model

## Supported Versions

GugleFS is pre-release software. Security fixes are applied to the latest prerelease only.

Report vulnerabilities privately through GitHub Security Advisories for `Gu-ZT/GugleFS`. Do not include real passwords, private keys, TOTP seeds, server contents, or production hostnames in an issue or diagnostic attachment.

## Assets And Trust Boundaries

GugleFS handles remote-server credentials, pasted and local SSH private keys, the application TOTP seed, remote file contents, mount paths, proxy configuration, and remembered mount state. The trusted computing base includes the GugleFS process, the operating-system credential store, WinFsp/FUSE/FUSE-T, TLS and SSH implementations, and the user's operating-system account.

FTP servers, WebDAV servers, SFTP servers, proxies, directory listings, filenames, redirects, and filesystem callers are untrusted. A user who can read or modify the current operating-system account can control application configuration and should be considered inside the trust boundary.

## Primary Threats And Controls

| Threat | Current control | Residual risk |
| --- | --- | --- |
| Credential disclosure at rest | Passwords, pasted keys, private-key passphrases, and the application TOTP seed use the platform credential store; configuration stores references only | Local private-key mode stores its filesystem path; compromise of the user account or credential store exposes secrets |
| Credential disclosure over IPC or logs | Secret values are accepted only as command inputs and are not returned; fixed-field JSONL logs and diagnostic reports exclude identifying configuration and free-form errors | Crash dumps and third-party libraries remain platform concerns |
| WebDAV credential forwarding | HTTPS is required and authenticated redirects are restricted to the same origin | A trusted server or local TLS trust-store compromise can still expose credentials |
| SSH server impersonation | SFTP stores and checks a user-approved SHA-256 host-key fingerprint | First-use confirmation can be accepted incorrectly; `known_hosts` import is not implemented |
| Path traversal and encoded separators | VFS paths are normalized; WebDAV href parsing rejects encoded separators and cross-origin redirects | Protocol/server-specific filename behavior still needs integration testing |
| Mount-point takeover or collision | Duplicate mount points, occupied Windows drive letters, and non-empty Unix directories are rejected | Races with other local processes remain possible between validation and mount |
| Corruption during interrupted writes | VFS serializes writes per handle and remote backends use read-modify-write where required | There is no durable write-back journal; power loss or remote partial failure can still leave protocol-dependent results |
| Unauthorized use of an unlocked app | Startup and credential operations require TOTP unlock; locking unmounts active mappings | TOTP does not protect an already unlocked OS session from process injection or UI automation |
| Dependency or bundled-runtime tampering | CI uses a lockfile; bundled FUSE-T has a pinned checksum and attribution | Release signing/notarization is not fully configured on all platforms |

## Security Invariants

- Never persist or log plaintext passwords, private-key contents/passphrases, TOTP seeds/codes, or proxy credentials.
- Never place secret values in IPC responses, mount-state files, exported configurations, release logs, or diagnostic bundles.
- Treat remote paths and names as untrusted structured data; do not build protocol requests or local paths through unchecked string concatenation.
- Reject authentication-bearing cross-origin redirects.
- Preserve safe unmount behavior on lock and normal exit.
- Keep host-key changes explicit and user-confirmed.

## Known Gaps

Release signing and notarization, OpenSSH `known_hosts` import, SSH Agent support, durable write recovery, protocol integration environments, filesystem consistency tests, and hostile-server fuzzing remain open work. See `TODO.md`; these items require platform credentials, external services, or broader design work and must not be represented as completed without verification evidence.
