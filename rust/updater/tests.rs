//! Synthetic release metadata and local sockets only; no real update is run.
use super::*;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::io::Cursor;
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};

fn release(version: &str) -> Value {
    json!({
        "tag_name": format!("v{version}"), "draft": false, "prerelease": false,
        "assets": [{
            "name": format!("SightOCR-Setup-{version}.exe"), "state": "uploaded", "size": 128,
            "browser_download_url": format!("https://github.com/FueTsui/SightOCR/releases/download/v{version}/SightOCR-Setup-{version}.exe"),
            "digest": format!("sha256:{}", "01".repeat(32))
        }]
    })
}

fn parsed(value: &Value) -> Result<Option<AvailableUpdate>> {
    parse_release(&serde_json::to_vec(value)?, "2.0.2")
}

fn checksum_asset(version: &str) -> Value {
    json!({
        "name": "SHA256SUMS.txt", "state": "uploaded", "size": 512,
        "browser_download_url": format!("https://github.com/FueTsui/SightOCR/releases/download/v{version}/SHA256SUMS.txt"),
        "digest": null
    })
}

#[test]
fn versions_compare_numbers_and_skip_older_assets_before_selecting_an_installer() -> Result<()> {
    assert!(numeric_version("v2.0.10")? > numeric_version("2.0.9")?);
    assert!(numeric_version("10.0.0")? > numeric_version("9.99.99")?);
    assert_eq!(numeric_version("v2.0.2")?, numeric_version("2.0.2")?);
    assert!(parsed(&release("2.0.2"))?.is_none());
    assert_eq!(parsed(&release("2.0.10"))?.unwrap().version, "2.0.10");
    let mut legacy = release("1.8.4");
    legacy["assets"][0]["name"] = json!("SightOCR_v1.8.4_Setup.exe");
    legacy["assets"][0]["digest"] = Value::Null;
    assert!(parsed(&legacy)?.is_none());
    for invalid in [
        "",
        "v",
        "v2.0",
        "2.0.3.4",
        "2.0.3-beta",
        "2.0.3+meta",
        "2.00.3",
        "2.0.-1",
        "2.0.３",
        " 2.0.3",
        "2.0.18446744073709551616",
    ] {
        assert!(
            numeric_version(invalid).is_err(),
            "accepted invalid version: {invalid}"
        );
    }
    Ok(())
}

#[test]
fn exact_setup_name_stable_release_and_unique_asset_are_required() {
    for name in [
        "SightOCR.exe",
        "SightOCR-Setup-2.0.2.exe",
        "different-Setup-2.0.3.exe",
        "SightOCR-Setup-2.0.3.exe.zip",
    ] {
        let mut value = release("2.0.3");
        value["assets"][0]["name"] = json!(name);
        assert!(parsed(&value).is_err());
    }
    for key in ["draft", "prerelease"] {
        let mut value = release("2.0.3");
        value[key] = json!(true);
        assert!(parsed(&value).is_err());
    }
    let mut duplicate = release("2.0.3");
    let asset = duplicate["assets"][0].clone();
    duplicate["assets"].as_array_mut().unwrap().push(asset);
    assert!(parsed(&duplicate).is_err());
}

#[test]
fn official_repository_tag_filename_and_bounded_uploaded_size_are_enforced() {
    for url in [
        "http://github.com/FueTsui/SightOCR/releases/download/v2.0.3/SightOCR-Setup-2.0.3.exe",
        "https://github.com/attacker/SightOCR/releases/download/v2.0.3/SightOCR-Setup-2.0.3.exe",
        "https://github.com/FueTsui/SightOCR/releases/download/v2.0.4/SightOCR-Setup-2.0.3.exe",
        "https://github.com/FueTsui/SightOCR/releases/download/v2.0.3/SightOCR.exe",
        "https://github.com/FueTsui/SightOCR/releases/download/v2.0.3/SightOCR-Setup-2.0.3.exe?redirect=1",
    ] {
        let mut value = release("2.0.3");
        value["assets"][0]["browser_download_url"] = json!(url);
        assert!(parsed(&value).is_err());
    }
    for size in [0, MAX_INSTALLER_BYTES + 1] {
        let mut value = release("2.0.3");
        value["assets"][0]["size"] = json!(size);
        assert!(parsed(&value).is_err());
    }
    let mut value = release("2.0.3");
    value["assets"][0]["state"] = json!("new");
    assert!(parsed(&value).is_err());
}

#[test]
fn a_checksum_is_mandatory_and_invalid_digest_cannot_fall_back() -> Result<()> {
    let mut value = release("2.0.3");
    value["assets"][0]["digest"] = Value::Null;
    value["body"] = json!(format!("SHA256: {}", "ab".repeat(32)));
    assert!(
        parsed(&value).is_err(),
        "release notes must not authorize installation"
    );
    value["assets"]
        .as_array_mut()
        .unwrap()
        .push(checksum_asset("2.0.3"));
    assert!(parsed(&value)?.unwrap().digest.is_none());
    for hash in [
        "sha256:bad",
        "md5:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "sha256:",
    ] {
        value["assets"][0]["digest"] = json!(hash);
        assert!(parsed(&value).is_err());
    }
    value["assets"][0]["digest"] = Value::Null;
    value["assets"][1]["browser_download_url"] = json!("https://attacker.invalid/SHA256SUMS.txt");
    assert!(parsed(&value).is_err());
    Ok(())
}

#[test]
fn checksum_manifest_requires_one_exact_filename() -> Result<()> {
    let filename = "SightOCR-Setup-2.0.3.exe";
    let hash = "AB".repeat(32);
    for line in [
        format!("{hash}  {filename}\r\n"),
        format!("\u{feff}{hash} *{filename}\n"),
    ] {
        assert_eq!(checksum_for(line.as_bytes(), filename)?, [0xab; 32]);
    }
    for invalid in [
        format!("{hash}  other.exe"),
        format!("{hash}  ./{filename}"),
        format!("{hash} {filename} trailing"),
        format!("{hash} {filename}\n{hash} {filename}"),
        format!("invalid {filename}"),
    ] {
        assert!(checksum_for(invalid.as_bytes(), filename).is_err());
    }
    Ok(())
}

#[test]
fn redirects_cannot_escape_github_release_infrastructure_or_downgrade_tls() {
    for url in [
        RELEASE_URL,
        "https://github.com/FueTsui/SightOCR/releases/download/v2.0.3/SightOCR-Setup-2.0.3.exe",
        "https://release-assets.githubusercontent.com/github-production-release-asset/1/2?token=sample",
        "https://objects.githubusercontent.com/github-production-release-asset/1/2",
    ] {
        assert!(allowed_redirect(&Url::parse(url).unwrap()), "rejected {url}");
    }
    for url in [
        "http://github.com/FueTsui/SightOCR/releases/download/installer.exe",
        "https://github.com/attacker/repo/releases/download/installer.exe",
        "https://api.github.com/repos/attacker/repo/releases/latest",
        "https://release-assets.githubusercontent.com.attacker.invalid/installer.exe",
        "https://github.com@attacker.invalid/installer.exe",
        "https://user:password@github.com/FueTsui/SightOCR/releases/download/installer.exe",
        "https://release-assets.githubusercontent.com:444/installer.exe",
        "file:///C:/installer.exe",
    ] {
        assert!(
            !allowed_redirect(&Url::parse(url).unwrap()),
            "accepted {url}"
        );
    }
}

fn executable_bytes() -> Vec<u8> {
    let mut bytes = vec![0; 128];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
    bytes[64..68].copy_from_slice(b"PE\0\0");
    bytes
}

#[test]
fn download_checks_lengths_and_reports_complete_progress() -> Result<()> {
    let bytes = executable_bytes();
    let events = RefCell::new(Vec::new());
    let mut result = Vec::new();
    stream_download(Cursor::new(&bytes), &mut result, 128, "2.0.3", &|event| {
        events.borrow_mut().push(event)
    })?;
    assert_eq!(result, bytes);
    assert!(matches!(
        events.borrow().first(),
        Some(UpdateProgress::Downloading {
            downloaded: 0,
            total: 128,
            ..
        })
    ));
    assert!(matches!(
        events.borrow().last(),
        Some(UpdateProgress::Downloading {
            downloaded: 128,
            total: 128,
            ..
        })
    ));
    let short =
        stream_download(Cursor::new(&bytes[..50]), Vec::new(), 128, "2.0.3", &|_| {}).unwrap_err();
    assert!(retryable(&short));
    let oversized =
        stream_download(Cursor::new(&bytes), Vec::new(), 127, "2.0.3", &|_| {}).unwrap_err();
    assert!(!retryable(&oversized));
    Ok(())
}

#[test]
fn verified_download_rejects_corruption_truncation_and_non_executables() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("SightOCR-Setup-2.0.3.exe");
    let bytes = executable_bytes();
    let hash: [u8; 32] = Sha256::digest(&bytes).into();
    std::fs::write(&path, &bytes)?;
    drop(open_verified(&path, 128, &hash)?);
    assert!(open_verified(&path, 129, &hash).is_err());
    let mut corrupted = bytes.clone();
    corrupted[100] = 1;
    std::fs::write(&path, &corrupted)?;
    assert!(open_verified(&path, 128, &hash).is_err());
    let html = vec![b'x'; 128];
    std::fs::write(&path, &html)?;
    assert!(open_verified(&path, 128, &Sha256::digest(&html).into()).is_err());
    let mut bad_pe = bytes;
    bad_pe[60..64].copy_from_slice(&9999u32.to_le_bytes());
    std::fs::write(&path, &bad_pe)?;
    assert!(open_verified(&path, 128, &Sha256::digest(&bad_pe).into()).is_err());
    Ok(())
}

#[test]
fn discarding_a_verified_update_removes_the_installer_and_directory() -> Result<()> {
    let directory = tempfile::Builder::new()
        .prefix("SightOCR-update-test-")
        .tempdir()?;
    let temp_path = directory.path().to_owned();
    let installer = temp_path.join("SightOCR-Setup-2.0.3.exe");
    let bytes = executable_bytes();
    std::fs::write(&installer, &bytes)?;
    let update = PreparedUpdate {
        _verified_file: open_verified(&installer, 128, &Sha256::digest(&bytes).into())?,
        directory,
        version: "2.0.3".into(),
        expected_size: 128,
        expected_hash: Sha256::digest(&bytes).into(),
    };
    assert_eq!(update.version(), "2.0.3");
    #[cfg(windows)]
    assert!(
        OpenOptions::new().write(true).open(&installer).is_err(),
        "verified installer must stay read locked"
    );
    // Invalid destination returns before spawning any process and must clean up.
    assert!(update
        .launch(Path::new("relative-invalid-install-directory"))
        .is_err());
    assert!(!temp_path.exists());
    Ok(())
}

#[test]
fn interrupted_or_corrupt_downloads_remove_their_real_staging_directory() -> Result<()> {
    let update = parsed(&release("2.0.3"))?.unwrap();
    let bytes = executable_bytes();
    for interrupted in [true, false] {
        let path = RefCell::new(PathBuf::new());
        let result = stage_download(&update, &[1; 32], &|_| {}, |installer| {
            *path.borrow_mut() = installer.parent().unwrap().to_owned();
            if interrupted {
                stream_download(
                    Cursor::new(&bytes[..50]),
                    File::create(installer)?,
                    128,
                    "2.0.3",
                    &|_| {},
                )
            } else {
                std::fs::write(installer, &bytes)?;
                Ok(())
            }
        });
        assert!(result.is_err());
        assert!(!path.borrow().as_os_str().is_empty());
        assert!(!path.borrow().exists(), "failed download left files behind");
    }
    Ok(())
}

fn server(responses: Vec<Vec<u8>>) -> Result<(String, JoinHandle<Vec<String>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let endpoint = format!("http://{}/test", listener.local_addr()?);
    let task = thread::spawn(move || {
        let mut requests = Vec::new();
        for response in responses {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("local update server timed out: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            requests.push(read_head(&mut stream));
            stream.write_all(&response).unwrap();
        }
        requests
    });
    Ok((endpoint, task))
}

fn read_head(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") && bytes.len() < 16_384 {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
    }
    String::from_utf8(bytes).unwrap()
}

fn local_client() -> Result<Client> {
    Ok(Client::builder()
        .no_proxy()
        .redirect(redirect_policy())
        .timeout(Duration::from_secs(5))
        .build()?)
}

#[test]
fn local_http_retry_recovers_and_metadata_is_size_bounded() -> Result<()> {
    let unavailable =
        b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_vec();
    let success = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".to_vec();
    let (endpoint, worker) = server(vec![unavailable, success])?;
    assert_eq!(
        fetch_bounded(&local_client()?, &endpoint, true, 128)?,
        b"{}"
    );
    let requests = worker.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0]
        .to_ascii_lowercase()
        .contains("x-github-api-version: 2026-03-10"));

    let oversized = b"HTTP/1.1 200 OK\r\nContent-Length: 129\r\nConnection: close\r\n\r\n".to_vec();
    let (endpoint, worker) = server(vec![oversized])?;
    assert!(fetch_bounded(&local_client()?, &endpoint, true, 128).is_err());
    worker.join().unwrap();

    let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\n12345\r\n0\r\n\r\n".to_vec();
    let (endpoint, worker) = server(vec![chunked])?;
    assert!(fetch_bounded(&local_client()?, &endpoint, true, 4).is_err());
    worker.join().unwrap();
    Ok(())
}

#[test]
fn retry_is_bounded_and_http_errors_do_not_expose_response_bodies() -> Result<()> {
    let attempts = Cell::new(0);
    let failure = retry::<()>(|| {
        attempts.set(attempts.get() + 1);
        Err(NetworkFailure::Connection.into())
    });
    assert!(failure.is_err());
    assert_eq!(attempts.get(), ATTEMPTS);
    let response = b"HTTP/1.1 403 Forbidden\r\nContent-Length: 16\r\nConnection: close\r\n\r\nsynthetic-secret".to_vec();
    let (endpoint, worker) = server(vec![response])?;
    let error = fetch_bounded(&local_client()?, &endpoint, true, 128)
        .unwrap_err()
        .to_string();
    assert!(!error.contains("synthetic") && !error.contains("127.0.0.1"));
    assert!(error.contains("稍后重试"));
    worker.join().unwrap();
    Ok(())
}

#[test]
fn insecure_redirect_is_blocked_before_the_target_receives_a_request() -> Result<()> {
    let response = b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/do-not-request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
    let (endpoint, worker) = server(vec![response])?;
    let error = request(&local_client()?, &endpoint, false)
        .unwrap_err()
        .to_string();
    assert!(!error.contains("127.0.0.1"));
    worker.join().unwrap();
    Ok(())
}

#[test]
fn configured_proxy_is_used_and_its_credentials_never_appear_in_errors() -> Result<()> {
    use base64::Engine;
    let denial = b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
    let (endpoint, worker) = server(vec![denial])?;
    let config = ProxyConfig {
        mode: crate::config::ProxyMode::Manual,
        url: endpoint.trim_end_matches("/test").into(),
        username: "synthetic-proxy-user".into(),
        password: "synthetic-proxy-password".into(),
    };
    let error = request(&build_client(&config)?, RELEASE_URL, true)
        .unwrap_err()
        .to_string();
    assert!(!error.contains("synthetic") && !error.contains("127.0.0.1"));
    let requests = worker.join().unwrap();
    assert!(requests[0].starts_with("CONNECT api.github.com:443 HTTP/1.1\r\n"));
    let auth = base64::engine::general_purpose::STANDARD
        .encode("synthetic-proxy-user:synthetic-proxy-password");
    assert!(requests[0]
        .to_ascii_lowercase()
        .contains(&format!("proxy-authorization: basic {auth}").to_ascii_lowercase()));
    Ok(())
}

fn helper_fixture() -> Result<(TempDir, PathBuf, PathBuf)> {
    let directory = tempfile::tempdir()?;
    let staged = directory.path().join("更新 文件");
    let installed = directory.path().join("安装 目录");
    std::fs::create_dir_all(&staged)?;
    std::fs::create_dir_all(&installed)?;
    let bytes = executable_bytes();
    std::fs::write(staged.join("SightOCR-Setup-2.0.3.exe"), &bytes)?;
    let manifest = UpdateManifest {
        version: "2.0.3".into(),
        install_dir: installed,
        size: bytes.len() as u64,
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        parent_pid: 1234,
    };
    let manifest_path = staged.join(MANIFEST_NAME);
    let helper = staged.join(HELPER_NAME);
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest)?)?;
    std::fs::write(&helper, "fixture: never execute")?;
    Ok((directory, manifest_path, helper))
}

#[test]
fn helper_waits_before_installing_and_only_installer_restarts_on_success() -> Result<()> {
    let (_directory, manifest, helper) = helper_fixture()?;
    let order = RefCell::new(Vec::new());
    apply_manifest(
        &manifest,
        &helper,
        |_| {
            order.borrow_mut().push("wait");
            Ok(())
        },
        |installer, destination| {
            order.borrow_mut().push("install");
            assert_eq!(installer.file_name().unwrap(), "SightOCR-Setup-2.0.3.exe");
            assert_eq!(destination.file_name().unwrap(), "安装 目录");
            #[cfg(windows)]
            assert!(OpenOptions::new().write(true).open(installer).is_err());
            Ok(0)
        },
        |_| panic!("successful installer must own the only restart"),
    )?;
    assert_eq!(*order.borrow(), ["wait", "install"]);
    Ok(())
}

#[test]
fn helper_restores_the_original_app_once_after_exit_code_or_spawn_failure() -> Result<()> {
    for code in [Some(5), None] {
        let (_directory, manifest, helper) = helper_fixture()?;
        let restores = Cell::new(0);
        let error = apply_manifest(
            &manifest,
            &helper,
            |_| Ok(()),
            |_, _| match code {
                Some(code) => Ok(code),
                None => Err(anyhow!("无法运行更新安装程序")),
            },
            |destination| {
                assert_eq!(destination.file_name().unwrap(), "安装 目录");
                restores.set(restores.get() + 1);
                Ok(())
            },
        )
        .unwrap_err()
        .to_string();
        assert_eq!(restores.get(), 1);
        assert!(error.contains("已重新启动 SightOCR"));
    }
    Ok(())
}

#[test]
fn helper_revalidates_download_and_recovers_without_executing_corrupt_bytes() -> Result<()> {
    let (_directory, manifest, helper) = helper_fixture()?;
    let restores = Cell::new(0);
    let mut bytes = executable_bytes();
    bytes[120] = 42;
    std::fs::write(
        helper.parent().unwrap().join("SightOCR-Setup-2.0.3.exe"),
        bytes,
    )?;
    let error = apply_manifest(
        &manifest,
        &helper,
        |_| Ok(()),
        |_, _| panic!("corrupted installer executed"),
        |_| {
            restores.set(restores.get() + 1);
            Ok(())
        },
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("SHA256 校验失败"));
    assert_eq!(restores.get(), 1);
    Ok(())
}

#[test]
fn helper_does_not_start_anything_if_parent_cannot_exit_and_reports_recovery_failure() -> Result<()>
{
    let (_directory, manifest, helper) = helper_fixture()?;
    assert!(apply_manifest(
        &manifest,
        &helper,
        |_| Err(anyhow!("parent has not exited")),
        |_, _| { panic!("installer launched while original app was still running") },
        |_| panic!("must not duplicate the still-running app")
    )
    .is_err());
    let error = apply_manifest(
        &manifest,
        &helper,
        |_| Ok(()),
        |_, _| Ok(5),
        |_| Err(anyhow!("restore failed")),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("无法重新启动 SightOCR"));
    Ok(())
}

#[test]
fn helper_rejects_manifests_outside_its_directory_and_malformed_metadata() -> Result<()> {
    let (directory, manifest, helper) = helper_fixture()?;
    let outside = directory.path().join(MANIFEST_NAME);
    std::fs::copy(&manifest, &outside)?;
    assert!(read_manifest(&outside, &helper).is_err());
    assert!(read_manifest(&manifest, &helper.with_file_name("SightOCR.exe")).is_err());
    let original: Value = serde_json::from_slice(&std::fs::read(&manifest)?)?;
    for (key, invalid) in [
        ("version", json!("../bad")),
        ("install_dir", json!("relative")),
        ("size", json!(0)),
        ("sha256", json!("invalid")),
        ("parent_pid", json!(0)),
    ] {
        let mut value = original.clone();
        value[key] = invalid;
        std::fs::write(&manifest, serde_json::to_vec(&value)?)?;
        assert!(read_manifest(&manifest, &helper).is_err());
    }
    std::fs::write(&manifest, vec![b' '; MAX_MANIFEST_BYTES as usize + 1])?;
    assert!(read_manifest(&manifest, &helper).is_err());
    Ok(())
}
