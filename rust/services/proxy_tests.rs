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
