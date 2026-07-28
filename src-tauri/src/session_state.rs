use std::{
    fs,
    io::ErrorKind,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Debug)]
pub struct SessionState {
    marker_path: PathBuf,
    previous_session_unclean: bool,
    exit_started: AtomicBool,
}

impl SessionState {
    pub fn begin(marker_path: PathBuf) -> Result<Self, String> {
        let previous_session_unclean = marker_path.exists();
        let parent = marker_path
            .parent()
            .ok_or_else(|| "运行状态路径缺少父目录".to_string())?;
        fs::create_dir_all(parent).map_err(|error| format!("创建运行状态目录失败: {error}"))?;
        fs::write(&marker_path, b"GugleFS session in progress\n")
            .map_err(|error| format!("写入运行状态失败: {error}"))?;
        Ok(Self {
            marker_path,
            previous_session_unclean,
            exit_started: AtomicBool::new(false),
        })
    }

    pub const fn previous_session_unclean(&self) -> bool {
        self.previous_session_unclean
    }

    pub fn begin_exit(&self) -> bool {
        !self.exit_started.swap(true, Ordering::AcqRel)
    }

    pub fn mark_clean(&self) -> Result<(), String> {
        match fs::remove_file(&self.marker_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("清理运行状态失败: {error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_path() -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "guglefs-session-{}-{unique}.marker",
            std::process::id()
        ))
    }

    #[test]
    fn detects_an_existing_marker_and_clears_it_after_a_clean_exit() {
        let path = test_path();
        let first = SessionState::begin(path.clone()).unwrap();
        assert!(!first.previous_session_unclean());

        let recovered = SessionState::begin(path.clone()).unwrap();
        assert!(recovered.previous_session_unclean());
        assert!(recovered.begin_exit());
        assert!(!recovered.begin_exit());
        recovered.mark_clean().unwrap();

        let clean = SessionState::begin(path.clone()).unwrap();
        assert!(!clean.previous_session_unclean());
        clean.mark_clean().unwrap();
    }
}
