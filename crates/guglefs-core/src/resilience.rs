use std::{future::Future, sync::Arc, time::Duration};

use async_trait::async_trait;
use tokio::{sync::Semaphore, time};

use crate::{
    DirectoryEntry, DirectoryPage, EngineError, EngineResult, FileMetadata, FileSystemSpace,
    FileTimes, FsErrorCode, RemoteFileSystem,
};

const DEFAULT_MAX_CONCURRENT_OPERATIONS: usize = 8;
const DEFAULT_CONTROL_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_TRANSFER_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_RETRY_DELAY: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Copy)]
pub struct RemoteOperationPolicy {
    pub max_concurrent_operations: usize,
    pub control_timeout: Duration,
    pub transfer_timeout: Duration,
    pub retry_delay: Duration,
}

impl Default for RemoteOperationPolicy {
    fn default() -> Self {
        Self {
            max_concurrent_operations: DEFAULT_MAX_CONCURRENT_OPERATIONS,
            control_timeout: DEFAULT_CONTROL_TIMEOUT,
            transfer_timeout: DEFAULT_TRANSFER_TIMEOUT,
            retry_delay: DEFAULT_RETRY_DELAY,
        }
    }
}

pub struct ResilientRemoteFileSystem<R: RemoteFileSystem + ?Sized> {
    inner: Arc<R>,
    limiter: Semaphore,
    policy: RemoteOperationPolicy,
}

impl<R: RemoteFileSystem + ?Sized> ResilientRemoteFileSystem<R> {
    pub fn new(inner: Arc<R>) -> Self {
        Self::with_policy(inner, RemoteOperationPolicy::default())
    }

    pub fn with_policy(inner: Arc<R>, mut policy: RemoteOperationPolicy) -> Self {
        policy.max_concurrent_operations = policy.max_concurrent_operations.max(1);
        Self {
            inner,
            limiter: Semaphore::new(policy.max_concurrent_operations),
            policy,
        }
    }

    async fn execute<T, F, Fut>(
        &self,
        operation_name: &'static str,
        timeout: Duration,
        retryable: bool,
        mut operation: F,
    ) -> EngineResult<T>
    where
        F: FnMut(Arc<R>) -> Fut,
        Fut: Future<Output = EngineResult<T>>,
    {
        let _permit = self
            .limiter
            .acquire()
            .await
            .map_err(|_| EngineError::Internal("remote operation limiter is closed".into()))?;
        let attempts = if retryable { 2 } else { 1 };
        for attempt in 0..attempts {
            let result = time::timeout(timeout, operation(Arc::clone(&self.inner))).await;
            match result {
                Ok(Ok(value)) => return Ok(value),
                Ok(Err(error)) => {
                    if attempt + 1 == attempts || !is_transient(&error) {
                        return Err(error);
                    }
                }
                Err(_) if attempt + 1 == attempts => {
                    return Err(timeout_error(operation_name, timeout));
                }
                Err(_) => {}
            }
            time::sleep(self.policy.retry_delay).await;
        }
        unreachable!("remote operations always execute at least once")
    }
}

#[async_trait]
impl<R: RemoteFileSystem + ?Sized + 'static> RemoteFileSystem for ResilientRemoteFileSystem<R> {
    async fn connect(&self) -> EngineResult<()> {
        self.execute(
            "connect",
            self.policy.control_timeout,
            true,
            |remote| async move { remote.connect().await },
        )
        .await
    }

    async fn disconnect(&self) -> EngineResult<()> {
        self.execute(
            "disconnect",
            self.policy.control_timeout,
            false,
            |remote| async move { remote.disconnect().await },
        )
        .await
    }

    async fn metadata(&self, path: &str) -> EngineResult<FileMetadata> {
        let path = path.to_string();
        self.execute(
            "read metadata",
            self.policy.control_timeout,
            true,
            move |remote| {
                let path = path.clone();
                async move { remote.metadata(&path).await }
            },
        )
        .await
    }

    async fn filesystem_space(&self, path: &str) -> EngineResult<Option<FileSystemSpace>> {
        let path = path.to_string();
        self.execute(
            "read filesystem space",
            self.policy.control_timeout,
            true,
            move |remote| {
                let path = path.clone();
                async move { remote.filesystem_space(&path).await }
            },
        )
        .await
    }

    async fn read_dir(&self, path: &str) -> EngineResult<Vec<DirectoryEntry>> {
        let path = path.to_string();
        self.execute(
            "read directory",
            self.policy.control_timeout,
            true,
            move |remote| {
                let path = path.clone();
                async move { remote.read_dir(&path).await }
            },
        )
        .await
    }

    async fn read_dir_page(
        &self,
        path: &str,
        cursor: Option<&str>,
        max_entries: usize,
    ) -> EngineResult<DirectoryPage> {
        let path = path.to_string();
        let cursor = cursor.map(str::to_owned);
        let retryable = cursor.is_none();
        self.execute(
            "read directory page",
            self.policy.control_timeout,
            retryable,
            move |remote| {
                let path = path.clone();
                let cursor = cursor.clone();
                async move {
                    remote
                        .read_dir_page(&path, cursor.as_deref(), max_entries)
                        .await
                }
            },
        )
        .await
    }

    async fn close_dir_cursor(&self, cursor: &str) -> EngineResult<()> {
        let cursor = cursor.to_string();
        self.execute(
            "close directory cursor",
            self.policy.control_timeout,
            false,
            move |remote| {
                let cursor = cursor.clone();
                async move { remote.close_dir_cursor(&cursor).await }
            },
        )
        .await
    }

    async fn read_range(&self, path: &str, offset: u64, length: u64) -> EngineResult<Vec<u8>> {
        let path = path.to_string();
        self.execute(
            "read file",
            self.policy.transfer_timeout,
            true,
            move |remote| {
                let path = path.clone();
                async move { remote.read_range(&path, offset, length).await }
            },
        )
        .await
    }

    async fn write(&self, path: &str, offset: u64, data: Vec<u8>) -> EngineResult<u64> {
        let path = path.to_string();
        self.execute(
            "write file",
            self.policy.transfer_timeout,
            true,
            move |remote| {
                let path = path.clone();
                let data = data.clone();
                async move { remote.write(&path, offset, data).await }
            },
        )
        .await
    }

    async fn create_file(&self, path: &str) -> EngineResult<()> {
        let path = path.to_string();
        self.execute(
            "create file",
            self.policy.control_timeout,
            false,
            move |remote| {
                let path = path.clone();
                async move { remote.create_file(&path).await }
            },
        )
        .await
    }

    async fn create_dir(&self, path: &str) -> EngineResult<()> {
        let path = path.to_string();
        self.execute(
            "create directory",
            self.policy.control_timeout,
            false,
            move |remote| {
                let path = path.clone();
                async move { remote.create_dir(&path).await }
            },
        )
        .await
    }

    async fn remove(&self, path: &str, directory: bool) -> EngineResult<()> {
        let path = path.to_string();
        self.execute(
            "remove",
            self.policy.control_timeout,
            false,
            move |remote| {
                let path = path.clone();
                async move { remote.remove(&path, directory).await }
            },
        )
        .await
    }

    async fn rename(&self, from: &str, to: &str) -> EngineResult<()> {
        let from = from.to_string();
        let to = to.to_string();
        self.execute(
            "rename",
            self.policy.control_timeout,
            false,
            move |remote| {
                let from = from.clone();
                let to = to.clone();
                async move { remote.rename(&from, &to).await }
            },
        )
        .await
    }

    async fn truncate(&self, path: &str, size: u64) -> EngineResult<()> {
        let path = path.to_string();
        self.execute(
            "truncate",
            self.policy.transfer_timeout,
            true,
            move |remote| {
                let path = path.clone();
                async move { remote.truncate(&path, size).await }
            },
        )
        .await
    }

    async fn set_times(&self, path: &str, times: FileTimes) -> EngineResult<()> {
        let path = path.to_string();
        self.execute(
            "set timestamps",
            self.policy.control_timeout,
            true,
            move |remote| {
                let path = path.clone();
                let times = times.clone();
                async move { remote.set_times(&path, times).await }
            },
        )
        .await
    }

    async fn flush(&self, path: &str) -> EngineResult<()> {
        let path = path.to_string();
        self.execute("flush", self.policy.transfer_timeout, true, move |remote| {
            let path = path.clone();
            async move { remote.flush(&path).await }
        })
        .await
    }
}

fn is_transient(error: &EngineError) -> bool {
    error.code() == FsErrorCode::RemoteIo
}

fn timeout_error(operation: &str, timeout: Duration) -> EngineError {
    EngineError::filesystem(
        FsErrorCode::RemoteIo,
        format!(
            "remote {operation} timed out after {} seconds",
            timeout.as_secs_f32()
        ),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct TestRemote {
        reads: AtomicUsize,
        creates: AtomicUsize,
        delay: Duration,
    }

    #[async_trait]
    impl RemoteFileSystem for TestRemote {
        async fn connect(&self) -> EngineResult<()> {
            Ok(())
        }

        async fn disconnect(&self) -> EngineResult<()> {
            Ok(())
        }

        async fn read_range(
            &self,
            _path: &str,
            _offset: u64,
            _length: u64,
        ) -> EngineResult<Vec<u8>> {
            if self.reads.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(EngineError::Remote("connection reset".into()));
            }
            Ok(b"ok".to_vec())
        }

        async fn create_file(&self, _path: &str) -> EngineResult<()> {
            self.creates.fetch_add(1, Ordering::SeqCst);
            Err(EngineError::Remote("response lost".into()))
        }

        async fn metadata(&self, _path: &str) -> EngineResult<FileMetadata> {
            time::sleep(self.delay).await;
            Ok(FileMetadata {
                kind: crate::EntryKind::File,
                size: 0,
                created: None,
                accessed: None,
                modified: None,
            })
        }
    }

    fn policy(timeout: Duration) -> RemoteOperationPolicy {
        RemoteOperationPolicy {
            max_concurrent_operations: 2,
            control_timeout: timeout,
            transfer_timeout: timeout,
            retry_delay: Duration::ZERO,
        }
    }

    #[tokio::test]
    async fn retries_transient_idempotent_operations_once() {
        let remote = Arc::new(TestRemote {
            reads: AtomicUsize::new(0),
            creates: AtomicUsize::new(0),
            delay: Duration::ZERO,
        });
        let managed = ResilientRemoteFileSystem::with_policy(
            Arc::clone(&remote),
            policy(Duration::from_secs(1)),
        );

        assert_eq!(managed.read_range("/file", 0, 2).await.unwrap(), b"ok");
        assert_eq!(remote.reads.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn does_not_replay_non_idempotent_operations() {
        let remote = Arc::new(TestRemote {
            reads: AtomicUsize::new(0),
            creates: AtomicUsize::new(0),
            delay: Duration::ZERO,
        });
        let managed = ResilientRemoteFileSystem::with_policy(
            Arc::clone(&remote),
            policy(Duration::from_secs(1)),
        );

        assert!(managed.create_file("/file").await.is_err());
        assert_eq!(remote.creates.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn returns_a_remote_io_error_after_the_timeout_retry() {
        let remote = Arc::new(TestRemote {
            reads: AtomicUsize::new(0),
            creates: AtomicUsize::new(0),
            delay: Duration::from_millis(25),
        });
        let managed =
            ResilientRemoteFileSystem::with_policy(remote, policy(Duration::from_millis(1)));

        let error = managed.metadata("/file").await.unwrap_err();
        assert_eq!(error.code(), FsErrorCode::RemoteIo);
        assert!(error.to_string().contains("timed out"));
    }
}
