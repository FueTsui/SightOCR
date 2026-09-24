//! Local socket tests exercise the real connector without TLS bypasses or cloud calls.

use super::*;
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    process::Command,
    thread::{self, JoinHandle},
};

fn listener<T: Send + 'static>(
    handler: impl FnOnce(TcpStream) -> T + Send + 'static,
) -> (SocketAddr, JoinHandle<T>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(error) => panic!("local mock did not receive a connection: {error}"),
            }
        };
        // Winsock can inherit the listener's nonblocking mode on accept.
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        handler(stream)
    });
    (address, handle)
}

fn read_head(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") && bytes.len() < 16_384 {
        let mut byte = [0u8];
        stream.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
    }
    assert!(bytes.ends_with(b"\r\n\r\n"));
    String::from_utf8(bytes).unwrap()
}

fn rejecting_proxy() -> (SocketAddr, JoinHandle<String>) {
    listener(|mut stream| {
        let request = read_head(&mut stream);
        stream.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=local-test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        request
    })
}

fn manual(address: SocketAddr) -> ProxyConfig {
    ProxyConfig {
        mode: ProxyMode::Manual,
        url: format!("http://{address}"),
        ..ProxyConfig::default()
    }
}

#[test]
fn manual_http_proxy_connect_uses_separate_auth_without_sending_api_payload() -> Result<()> {
    let (address, handle) = rejecting_proxy();
    let proxy = ProxyConfig {
        username: "proxy-user".into(),
        password: "synthetic p@ss:%".into(),
        ..manual(address)
    };
    let client = build_client(&proxy)?;
    let error = send(
        client
            .post("https://selected-provider.invalid/v1/ocr")
            .bearer_auth("synthetic-api-key")
            .body("private-source-text"),
    )
    .unwrap_err()
    .to_string();
    assert!(
        !error.contains("synthetic")
            && !error.contains("private-source-text")
            && !error.contains("127.0.0.1")
    );
    let request = handle.join().unwrap();
    assert!(request.starts_with("CONNECT selected-provider.invalid:443 HTTP/1.1\r\n"));
    let encoded = base64::engine::general_purpose::STANDARD.encode("proxy-user:synthetic p@ss:%");
    assert!(request.to_ascii_lowercase().contains(&format!(
        "proxy-authorization: basic {}",
        encoded.to_ascii_lowercase()
    )));
    assert!(!request.contains("synthetic-api-key") && !request.contains("private-source-text"));
    Ok(())
}

#[test]
fn every_public_cloud_entry_uses_the_changed_proxy() -> Result<()> {
    let mut services = Services::new()?;
    let mut config = Config {
        api_key: "synthetic".into(),
        secret_key: "synthetic".into(),
        baidu_trans_appid: "synthetic".into(),
        baidu_trans_appkey: "synthetic".into(),
        tencent_secret_id: "synthetic".into(),
        tencent_secret_key: "synthetic".into(),
        tencent_trans_secret_id: "synthetic".into(),
        tencent_trans_secret_key: "synthetic".into(),
        mistral_api_key: "synthetic".into(),
        openai_api_key: "synthetic".into(),
        nvidia_api_key: "synthetic".into(),
        ..Config::default()
    };
    for (selection, host) in [
        ("Baidu_auto", "aip.baidubce.com"),
        ("Tencent_auto", "ocr.tencentcloudapi.com"),
        ("Mistral_auto", "api.mistral.ai"),
        ("OpenAI_auto", "api.openai.com"),
        ("Nvidia_auto", "integrate.api.nvidia.com"),
    ] {
        let (address, handle) = rejecting_proxy();
        config.proxy = manual(address);
        assert!(services
            .ocr(b"synthetic-image", selection, &config)
            .is_err());
        assert!(handle
            .join()
            .unwrap()
            .starts_with(&format!("CONNECT {host}:443 ")));
    }
    for (selection, host) in [
        ("Baidu", "api.fanyi.baidu.com"),
        ("Tencent", "tmt.tencentcloudapi.com"),
        ("默认", "www.bing.com"),
        ("OpenAI", "api.openai.com"),
        ("Nvidia", "integrate.api.nvidia.com"),
    ] {
        let (address, handle) = rejecting_proxy();
        config.proxy = manual(address);
        config.last_translate_selection = selection.into();
        assert!(services.translate("synthetic text", &config).is_err());
        assert!(handle
            .join()
            .unwrap()
            .starts_with(&format!("CONNECT {host}:443 ")));
    }
    Ok(())
}

#[test]
fn direct_clears_preexisting_proxies_and_ignores_inactive_manual_drafts() -> Result<()> {
    let proxy_listener = TcpListener::bind("127.0.0.1:0")?;
    proxy_listener.set_nonblocking(true)?;
    let (origin, handle) = listener(|mut stream| {
        let mut record = [0; 3];
        stream.read_exact(&mut record).unwrap();
        record
    });
    let direct = ProxyConfig {
        mode: ProxyMode::Direct,
        url: "incomplete invalid draft".into(),
        username: "not-used".into(),
        password: "private-password".into(),
    };
    let seeded = Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(3))
        .proxy(reqwest::Proxy::all(format!(
            "http://{}",
            proxy_listener.local_addr()?
        ))?);
    let client = apply_proxy(seeded, &direct)?.build()?;
    // The mock origin closes after ClientHello: success is an actual TLS attempt,
    // not a plaintext request or a disabled certificate check.
    assert!(send(client.get(format!("https://{origin}/"))).is_err());
    let hello = handle.join().unwrap();
    assert_eq!(hello[0], 0x16);
    assert_eq!(hello[1], 0x03);
    assert!(
        matches!(proxy_listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    Ok(())
}

#[test]
fn switching_proxies_invalidates_sessions_but_invalid_manual_settings_do_not_commit() -> Result<()>
{
    let mut services = Services::new()?;
    let direct = ProxyConfig {
        mode: ProxyMode::Direct,
        ..ProxyConfig::default()
    };
    services.configure_proxy(&direct)?;
    services.baidu_token = Some(Token {
        credentials: [1; 32],
        value: "synthetic-token".into(),
        expires: Instant::now() + Duration::from_secs(60),
    });
    services.bing_session = Some(BingSession {
        key: "synthetic-key".into(),
        token: "synthetic-token".into(),
        ig: "synthetic".into(),
        iid: "synthetic".into(),
        cookies: reqwest::cookie::Jar::default(),
        page_url: reqwest::Url::parse(bing::TRANSLATOR_URL)?,
        expires: Instant::now() + Duration::from_secs(60),
        requests: 0,
    });
    services.configure_proxy(&direct)?;
    assert!(services.baidu_token.is_some() && services.bing_session.is_some());
    let invalid = ProxyConfig {
        mode: ProxyMode::Manual,
        url: "https://user:private-password@host/".into(),
        ..ProxyConfig::default()
    };
    assert!(services.configure_proxy(&invalid).is_err());
    assert!(services.proxy == direct);
    assert!(services.baidu_token.is_some() && services.bing_session.is_some());
    services.configure_proxy(&ProxyConfig::default())?;
    assert!(services.baidu_token.is_none() && services.bing_session.is_none());
    assert_eq!(services.proxy.mode, ProxyMode::System);
    Ok(())
}

#[test]
fn cancelled_entry_does_not_apply_a_pending_proxy_change() -> Result<()> {
    let mut services = Services::new()?;
    let config = Config {
        proxy: ProxyConfig {
            mode: ProxyMode::Manual,
            url: "incomplete".into(),
            ..ProxyConfig::default()
        },
        ..Config::default()
    };
    assert!(services
        .ocr_cancellable(b"image", "OpenAI_auto", &config, &|| true)
        .unwrap_err()
        .to_string()
        .contains("取消"));
    assert!(services
        .translate_cancellable("text", &config, &|| true)
        .unwrap_err()
        .to_string()
        .contains("取消"));
    assert_eq!(services.proxy.mode, ProxyMode::System);
    Ok(())
}

#[test]
fn socks5h_uses_proxy_dns_and_rfc1929_authentication() -> Result<()> {
    let (address, handle) = listener(|mut stream| {
        let mut greeting = [0; 2];
        stream.read_exact(&mut greeting).unwrap();
        assert_eq!(greeting[0], 5);
        let mut methods = vec![0; greeting[1] as usize];
        stream.read_exact(&mut methods).unwrap();
        assert!(methods.contains(&2));
        stream.write_all(&[5, 2]).unwrap();
        let mut auth = [0; 2];
        stream.read_exact(&mut auth).unwrap();
        assert_eq!(auth[0], 1);
        let mut username = vec![0; auth[1] as usize];
        stream.read_exact(&mut username).unwrap();
        let mut plen = [0];
        stream.read_exact(&mut plen).unwrap();
        let mut password = vec![0; plen[0] as usize];
        stream.read_exact(&mut password).unwrap();
        stream.write_all(&[1, 0]).unwrap();
        let mut connect = [0; 4];
        stream.read_exact(&mut connect).unwrap();
        assert_eq!(connect, [5, 1, 0, 3]);
        let mut length = [0];
        stream.read_exact(&mut length).unwrap();
        let mut domain = vec![0; length[0] as usize];
        stream.read_exact(&mut domain).unwrap();
        let mut port = [0; 2];
        stream.read_exact(&mut port).unwrap();
        stream.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).unwrap();
        (username, password, domain, port)
    });
    let proxy = ProxyConfig {
        mode: ProxyMode::Manual,
        url: format!("socks5h://{address}"),
        username: "user@name".into(),
        password: "p:ass%word".into(),
    };
    let client = build_client(&proxy)?;
    assert!(send(client.get("https://proxy-resolves-this.invalid/")).is_err());
    let (username, password, domain, port) = handle.join().unwrap();
    assert_eq!(username, b"user@name");
    assert_eq!(password, b"p:ass%word");
    assert_eq!(domain, b"proxy-resolves-this.invalid");
    assert_eq!(u16::from_be_bytes(port), 443);
    Ok(())
}

#[test]
fn socks5_sends_an_ip_address_for_local_dns_mode() -> Result<()> {
    let (address, handle) = listener(|mut stream| {
        let mut greeting = [0; 2];
        stream.read_exact(&mut greeting).unwrap();
        let mut methods = vec![0; greeting[1] as usize];
        stream.read_exact(&mut methods).unwrap();
        stream.write_all(&[5, 0]).unwrap();
        let mut connect = [0; 10];
        stream.read_exact(&mut connect).unwrap();
        stream.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).unwrap();
        connect
    });
    let proxy = ProxyConfig {
        mode: ProxyMode::Manual,
        url: format!("socks5://{address}"),
        ..ProxyConfig::default()
    };
    assert!(send(build_client(&proxy)?.get("https://127.0.0.1:4443/")).is_err());
    let connect = handle.join().unwrap();
    assert_eq!(&connect[..8], &[5, 1, 0, 1, 127, 0, 0, 1]);
    assert_eq!(u16::from_be_bytes([connect[8], connect[9]]), 4443);
    Ok(())
}

#[test]
fn system_mode_obeys_environment_proxy_in_an_isolated_process() -> Result<()> {
    let (address, handle) = rejecting_proxy();
    let url = format!("http://{address}");
    let mut child = Command::new(std::env::current_exe()?);
    child
        .args([
            "--exact",
            "services::proxy_tests::system_environment_child",
            "--ignored",
            "--test-threads=1",
        ])
        .env("SIGHTOCR_PROXY_CHILD_TEST", "1");
    for name in [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        child.env(name, &url);
    }
    for name in ["NO_PROXY", "no_proxy"] {
        child.env(name, "never-bypass.invalid");
    }
    let output = child.output()?;
    assert!(
        output.status.success(),
        "isolated proxy test failed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(handle
        .join()
        .unwrap()
        .starts_with("CONNECT system-proxy-target.invalid:443 "));
    Ok(())
}

#[test]
#[ignore = "invoked by system_mode_obeys_environment_proxy_in_an_isolated_process"]
fn system_environment_child() -> Result<()> {
    if std::env::var("SIGHTOCR_PROXY_CHILD_TEST").as_deref() != Ok("1") {
        return Ok(());
    }
    let config = Config {
        proxy: ProxyConfig {
            url: "inactive invalid draft".into(),
            ..ProxyConfig::default()
        },
        mistral_base_url: "https://system-proxy-target.invalid/v1".into(),
        mistral_api_key: "synthetic".into(),
        ..Config::default()
    };
    assert!(Services::new()?
        .ocr(b"image", "Mistral_auto", &config)
        .is_err());
    Ok(())
}

#[test]
fn system_proxy_changes_are_followed_without_restarting_services() -> Result<()> {
    // RegOverridePredefKey and environment edits are process-wide, so run this
    // test alone in a child with its own HKCU sandbox, never the user's settings.
    let mut child = Command::new(std::env::current_exe()?);
    child
        .args([
            "--exact",
            "services::proxy_tests::system_changes_child",
            "--ignored",
            "--test-threads=1",
        ])
        .env("SIGHTOCR_SYSTEM_CHANGES_TEST", "1");
    for name in [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
        "REQUEST_METHOD",
    ] {
        child.env_remove(name);
    }
    let output = child.output()?;
    assert!(
        output.status.success(),
        "system changes child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

struct IsolatedInternetSettings {
    root: windows_sys::Win32::System::Registry::HKEY,
    settings: windows_sys::Win32::System::Registry::HKEY,
}

impl IsolatedInternetSettings {
    fn new() -> Result<Self> {
        use windows_sys::Win32::System::Registry::*;
        let path: Vec<u16> = format!("Software\\SightOCR-Proxy-Test-{}", std::process::id())
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut root = std::ptr::null_mut();
        // SAFETY: Valid terminated path and output handle. Volatile test key
        // is separate from the real Internet Settings and opened in this child.
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_VOLATILE,
                KEY_ALL_ACCESS,
                std::ptr::null(),
                &mut root,
                std::ptr::null_mut(),
            )
        };
        anyhow::ensure!(status == 0, "create test registry root: {status}");
        let mut sandbox = Self {
            root,
            settings: std::ptr::null_mut(),
        };
        // SAFETY: Redirect only this isolated test process's predefined HKCU.
        let status = unsafe { RegOverridePredefKey(HKEY_CURRENT_USER, root) };
        anyhow::ensure!(status == 0, "override test HKCU: {status}");
        let path: Vec<u16> = system_proxy::SETTINGS_KEY
            .encode_utf16()
            .chain(Some(0))
            .collect();
        // SAFETY: All pointers are valid and HKCU now refers to our sandbox.
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_VOLATILE,
                KEY_ALL_ACCESS,
                std::ptr::null(),
                &mut sandbox.settings,
                std::ptr::null_mut(),
            )
        };
        anyhow::ensure!(status == 0, "create test Internet Settings: {status}");
        Ok(sandbox)
    }

    fn set(&self, name: &str, kind: u32, data: &[u8]) {
        use windows_sys::Win32::System::Registry::RegSetValueExW;
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        // SAFETY: This key belongs to the sandbox; name and byte buffer live
        // through the synchronous call and data length matches the buffer.
        let status = unsafe {
            RegSetValueExW(
                self.settings,
                name.as_ptr(),
                0,
                kind,
                data.as_ptr(),
                data.len() as u32,
            )
        };
        assert_eq!(status, 0);
    }

    fn enabled(&self, enabled: bool) {
        self.set(
            "ProxyEnable",
            windows_sys::Win32::System::Registry::REG_DWORD,
            &u32::from(enabled).to_le_bytes(),
        );
    }

    fn text(&self, name: &str, value: &str) {
        let bytes: Vec<u8> = value
            .encode_utf16()
            .chain(Some(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        self.set(name, windows_sys::Win32::System::Registry::REG_SZ, &bytes);
    }
}

impl Drop for IsolatedInternetSettings {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Registry::*;
        let path: Vec<u16> = format!("Software\\SightOCR-Proxy-Test-{}", std::process::id())
            .encode_utf16()
            .chain(Some(0))
            .collect();
        // SAFETY: Restore HKCU before deleting only the unique test subtree.
        // These handles are owned here and no services are using them on drop.
        unsafe {
            RegOverridePredefKey(HKEY_CURRENT_USER, std::ptr::null_mut());
            if !self.settings.is_null() {
                RegCloseKey(self.settings);
            }
            RegDeleteTreeW(self.root, std::ptr::null());
            RegCloseKey(self.root);
            RegDeleteKeyW(HKEY_CURRENT_USER, path.as_ptr());
        }
    }
}

fn expect_direct(services: &mut Services, config: &Config) -> Result<()> {
    let (origin, handle) = listener(|mut stream| {
        let mut record = [0; 3];
        stream.read_exact(&mut record).unwrap();
        record
    });
    let mut config = config.clone();
    config.mistral_base_url = format!("https://{origin}/v1");
    assert!(services.ocr(b"synthetic", "Mistral_auto", &config).is_err());
    let hello = handle.join().unwrap();
    assert_eq!(&hello[..2], &[0x16, 0x03]);
    Ok(())
}

#[test]
#[ignore = "invoked in isolation by system_proxy_changes_are_followed_without_restarting_services"]
fn system_changes_child() -> Result<()> {
    if std::env::var("SIGHTOCR_SYSTEM_CHANGES_TEST").as_deref() != Ok("1") {
        return Ok(());
    }
    let registry = IsolatedInternetSettings::new()?;
    let (first, first_request) = rejecting_proxy();
    registry.text("ProxyServer", &format!("http://{first}"));
    registry.enabled(true);
    let mut services = Services::new()?;
    let config = Config {
        mistral_base_url: "https://system-change.invalid/v1".into(),
        mistral_api_key: "synthetic".into(),
        ..Config::default()
    };
    assert!(services.ocr(b"synthetic", "Mistral_auto", &config).is_err());
    assert!(first_request
        .join()
        .unwrap()
        .starts_with("CONNECT system-change.invalid:443 "));

    // Stable configuration must preserve cached sessions.
    services.baidu_token = Some(Token {
        credentials: [1; 32],
        value: "synthetic".into(),
        expires: Instant::now() + Duration::from_secs(60),
    });
    services.configure_proxy(&config.proxy)?;
    assert!(services.baidu_token.is_some());

    // The old proxy has closed: disabling must reach the origin directly.
    registry.enabled(false);
    assert!(
        system_proxy::Snapshot::capture() != services.system_proxy,
        "disable changes snapshot"
    );
    expect_direct(&mut services, &config)?;
    assert!(services.baidu_token.is_none());

    // Re-enable at another port, exercising the translation entry too.
    let (second, second_request) = rejecting_proxy();
    registry.text("ProxyServer", &format!("http://{second}"));
    registry.enabled(true);
    assert!(services.translate("synthetic", &config).is_err());
    assert!(second_request
        .join()
        .unwrap()
        .starts_with("CONNECT www.bing.com:443 "));

    // Changing only ProxyServer while still enabled also discards the client.
    let (third, third_request) = rejecting_proxy();
    registry.text("ProxyServer", &format!("http://{third}"));
    assert!(services.ocr(b"synthetic", "Mistral_auto", &config).is_err());
    assert!(third_request
        .join()
        .unwrap()
        .starts_with("CONNECT system-change.invalid:443 "));

    // Changing only the bypass list must apply to the very next task.
    registry.text("ProxyOverride", "127.0.0.1");
    expect_direct(&mut services, &config)?;

    // Environment proxy changes retain the documented precedence and refresh.
    registry.text("ProxyOverride", "");
    let (environment, environment_request) = rejecting_proxy();
    std::env::set_var("HTTPS_PROXY", format!("http://{environment}"));
    assert!(services.ocr(b"synthetic", "Mistral_auto", &config).is_err());
    assert!(environment_request
        .join()
        .unwrap()
        .starts_with("CONNECT system-change.invalid:443 "));
    std::env::set_var("NO_PROXY", "127.0.0.1");
    expect_direct(&mut services, &config)?;
    std::env::remove_var("HTTPS_PROXY");
    std::env::remove_var("NO_PROXY");

    // Explicit Direct/Manual must not respond to external system changes.
    for proxy in [
        ProxyConfig {
            mode: ProxyMode::Direct,
            ..ProxyConfig::default()
        },
        manual(first),
    ] {
        services.configure_proxy(&proxy)?;
        services.baidu_token = Some(Token {
            credentials: [2; 32],
            value: "synthetic".into(),
            expires: Instant::now() + Duration::from_secs(60),
        });
        registry.enabled(false);
        services.configure_proxy(&proxy)?;
        assert!(services.baidu_token.is_some());
        registry.enabled(true);
    }
    Ok(())
}
