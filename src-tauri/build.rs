fn main() {
    tauri_build::build();
    #[cfg(windows)]
    winfsp_build::build();
}
