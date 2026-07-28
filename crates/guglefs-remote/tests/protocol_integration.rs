use std::time::{SystemTime, UNIX_EPOCH};

use guglefs_core::{
    AuthMethod, ConnectionSecrets, MappingConfig, Protocol, RemoteFileSystem, WebDavAuthMethod,
};
use guglefs_remote::{inspect_host_key, FtpFileSystem, SftpFileSystem};

const USERNAME: &str = "guglefs";
const PASSWORD: &str = "guglefs-test";

#[tokio::test]
#[ignore = "requires the repository FTP container"]
async fn ftp_container_exercises_remote_file_operations() {
    let host = required_env("GUGLEFS_TEST_FTP_HOST");
    let port = required_port("GUGLEFS_TEST_FTP_PORT");
    let config = mapping(Protocol::Ftp, host, port, "/", None);
    let remote = FtpFileSystem::from_config(&config, Some(PASSWORD.into())).unwrap();

    exercise_remote(&remote, "ftp").await;
}

#[tokio::test]
#[ignore = "requires the repository SFTP container"]
async fn sftp_container_exercises_remote_file_operations() {
    let host = required_env("GUGLEFS_TEST_SFTP_HOST");
    let port = required_port("GUGLEFS_TEST_SFTP_PORT");
    let fingerprint = inspect_host_key(&host, port, true).await.unwrap();
    let config = mapping(Protocol::Sftp, host, port, "/upload", Some(fingerprint));
    let remote = SftpFileSystem::from_config(
        &config,
        ConnectionSecrets {
            credential: Some(PASSWORD.into()),
            ..ConnectionSecrets::default()
        },
    )
    .unwrap();

    exercise_remote(&remote, "sftp").await;
}

async fn exercise_remote(remote: &dyn RemoteFileSystem, protocol: &str) {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = format!("/guglefs-integration-{protocol}-{suffix}");
    let original = format!("{directory}/original.txt");
    let renamed = format!("{directory}/renamed.txt");

    remote.connect().await.unwrap();
    remote.create_dir(&directory).await.unwrap();
    remote.create_file(&original).await.unwrap();
    assert_eq!(
        remote.write(&original, 0, b"hello".to_vec()).await.unwrap(),
        5
    );
    assert_eq!(
        remote
            .write(&original, 5, b" remote".to_vec())
            .await
            .unwrap(),
        7
    );
    assert_eq!(
        remote.read_range(&original, 0, 64).await.unwrap(),
        b"hello remote"
    );
    assert_eq!(remote.metadata(&original).await.unwrap().size, 12);
    assert!(remote
        .read_dir(&directory)
        .await
        .unwrap()
        .iter()
        .any(|entry| entry.name == "original.txt"));

    remote.rename(&original, &renamed).await.unwrap();
    remote.truncate(&renamed, 5).await.unwrap();
    assert_eq!(remote.read_range(&renamed, 0, 64).await.unwrap(), b"hello");
    remote.flush(&renamed).await.unwrap();
    remote.remove(&renamed, false).await.unwrap();
    remote.remove(&directory, true).await.unwrap();
    remote.disconnect().await.unwrap();
}

fn mapping(
    protocol: Protocol,
    host: String,
    port: u16,
    remote_path: &str,
    host_key_fingerprint: Option<String>,
) -> MappingConfig {
    MappingConfig {
        id: format!("{protocol:?}-integration"),
        name: format!("{protocol:?} integration"),
        protocol,
        host,
        port,
        username: Some(USERNAME.into()),
        auth: AuthMethod::Password {
            credential_id: None,
        },
        remote_path: remote_path.into(),
        mount_point: "/integration".into(),
        ftp_tls: false,
        host_key_fingerprint,
        sftp_totp_required: false,
        ignore_system_proxy: true,
        webdav_auth: WebDavAuthMethod::Basic,
        webdav_client_certificate_path: None,
        auto_mount: false,
    }
}

fn required_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required"))
}

fn required_port(name: &str) -> u16 {
    required_env(name)
        .parse()
        .unwrap_or_else(|_| panic!("{name} must be a valid port"))
}
