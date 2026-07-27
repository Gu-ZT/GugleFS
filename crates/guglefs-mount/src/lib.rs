#[cfg(unix)]
mod fuse;
#[cfg(windows)]
mod winfsp;

#[cfg(unix)]
pub use fuse::SystemMountDriver;
#[cfg(windows)]
pub use winfsp::SystemMountDriver;

#[cfg(not(any(unix, windows)))]
compile_error!("GugleFS currently supports Windows, Linux, and macOS only");
