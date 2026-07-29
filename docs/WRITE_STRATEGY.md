# Write, Commit, and Recovery Strategy

This document defines the durability contract for future chunked and deferred writes. It is a design specification, not a claim that write-back caching is enabled.

## Invariants

1. A successful filesystem write must mean that the corresponding bytes have been accepted by the remote service under the active concurrency precondition.
2. GugleFS must never acknowledge data that exists only in volatile memory.
3. Automatic recovery must not overwrite a remote object when the original version can no longer be proven.
4. Journals, cache metadata, diagnostics, and logs must not contain credentials, endpoint names, usernames, or plaintext remote paths.
5. Memory and local staging use must be bounded per mapping and globally.

## Current Contract

The current VFS is write-through. It serializes writes for each open handle and waits for `RemoteFileSystem::write` before returning success to WinFsp or FUSE. A failed handle rejects later writes and keeps returning the original error class from `flush` and `release`. SFTP writes the requested range directly, closes the remote write handle, reads the range back for a byte-for-byte comparison, and verifies the remote length after truncation. FTP performs read-modify-write through a verified same-directory temporary upload and rename replacement, while WebDAV still writes its conditionally updated representation directly. WebDAV applies an ETag or Last-Modified precondition when the server provides one. There is no durable local write-back journal.

This behavior remains the default until every implementation gate below is met. In particular, WinFsp close handling cannot reliably surface a late background-upload error after a prior write was reported as successful.

## Planned Chunk Model

- Use fixed 4 MiB logical chunks, with the final chunk allowed to be shorter.
- Track dirty byte ranges inside each chunk so a small write does not imply that untouched bytes are zeroed.
- Apply per-handle ordering in the VFS and a per-file commit lock across handles.
- Bound dirty data to 64 MiB per mapping and 256 MiB process-wide. Writers receive backpressure when either limit is reached.
- Keep memory staging as the fast path. Any disk spill must be opt-in, encrypted with a random key stored by the platform secure store, created with user-only permissions, and removed after commit or explicit abandonment.

## Remote Commit

The preferred transaction writes a same-directory temporary object, verifies its length and digest when the protocol permits, and replaces the destination using the strongest available rename or move primitive.

| Backend | Upload path | Concurrency guard | Commit caveat |
| --- | --- | --- | --- |
| SFTP | Sequential writes to a sibling temporary file | Original metadata plus optional server extensions | POSIX rename and server-side `fsync` extensions are not universally available |
| WebDAV | `PUT` to a sibling temporary resource | Strong ETag, otherwise Last-Modified | `MOVE` preconditions vary by server and require compatibility tests |
| FTP/FTPS | Upload a sibling temporary file | Size/modified-time comparison where available | Rename replacement and atomicity are server-specific |

If a backend cannot prove that the destination is still the version originally opened, GugleFS must stop and return a conflict. It must not silently use last-writer-wins during recovery.

## Journal and Recovery

The durable journal is a versioned state machine:

`prepared -> uploading -> ready_to_commit -> committed -> cleanup`

Each record contains only opaque mapping/file transaction IDs, chunk indexes, lengths, digests, the remote version validator, timestamps, and the encrypted staging-file reference. It never contains the mapping ID used by configuration, a remote path, or secret material.

Recovery starts only after the app is unlocked and the mapping credential is available:

- `prepared` or `uploading`: resume only when uploaded chunk identity and the original remote validator can be proven; otherwise abandon the temporary object and report a conflict.
- `ready_to_commit`: re-check the destination validator, then perform the remote replace once.
- `committed`: verify the destination identity before deleting local staging.
- `cleanup`: retry removal of local and remote temporary data without changing the destination.

Journal transitions are written with create-new temporary files, flush, and atomic local rename. A corrupt or unknown-version record is quarantined and never replayed automatically.

## Implementation Gates

Write-back may be enabled only after all of the following are true:

- Remote backends expose explicit capabilities for range writes, conditional replace, atomic rename, durable flush, and object identity.
- FTP, SFTP, and WebDAV integration suites exercise interrupted upload, ambiguous commit, stale validator, reconnect, and cleanup behavior.
- WinFsp and FUSE consistency tests verify write, flush, close, process crash, unmount, and remount outcomes on their native platforms.
- The mount adapters can surface a final commit failure before the operating system treats data as durable, or the UI clearly opts into weaker asynchronous semantics.
- Staging encryption, quota enforcement, journal migration, and hostile/corrupt journal tests pass.

Until then, GugleFS keeps write-through semantics and treats protocol-specific interrupted-write behavior as a documented residual risk.
