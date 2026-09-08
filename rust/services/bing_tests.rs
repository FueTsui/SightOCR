use super::*;
use std::io::Write;
use std::net::TcpListener;
use std::thread;

const PAGE: &str = r#"IG:"0123456789ABCDEF";
params_AbusePreventionHelper=[123456,"synthetic-token",600000];
<div data-iid="translator.5028"></div>"#;

// Execute each supplied HTTP fixture on a loopback socket. Only this test
// adapter rewrites the destination to HTTP; production URLs/policy are checked
// before the rewrite and production TLS settings remain unchanged.
fn reply(
    request: RequestBuilder,
    status: u16,
    headers: &[(&str, &str)],
    body: &str,
) -> Result<Response> {
    let mut request = request.build()?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    let mut response = format!(
        "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    response.push_str("\r\n");
    response.push_str(body);
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("Bing mock connection failed: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") && head.len() < 32768 {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        let head = String::from_utf8(head).unwrap();
        let content_length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        let mut body = vec![0; content_length];
        stream.read_exact(&mut body).unwrap();
        stream.write_all(response.as_bytes()).unwrap();
    });
    *request.url_mut() = Url::parse(&format!("http://{address}/fixture"))?;
    let result = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()?
        .execute(request);
    server.join().unwrap();
    Ok(result?)
}

fn client() -> Result<Client> {
    build_client(&ProxyConfig {
        mode: ProxyMode::Direct,
        ..ProxyConfig::default()
    })
}

#[test]
fn regional_302_keeps_session_origin_and_scopes_cookies_through_translation() -> Result<()> {
    let client = client()?;
    let mut calls = 0;
    let mut session = authorize_with(&client, &|| false, |request| {
        let actual = request.try_clone().unwrap().build()?;
        assert_eq!(actual.method(), reqwest::Method::GET);
        assert!(actual.body().is_none());
        let cookies = actual
            .headers()
            .get(COOKIE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        calls += 1;
        match calls {
            1 => {
                assert_eq!(actual.url().as_str(), TRANSLATOR_URL);
                assert!(cookies.is_empty());
                reply(
                    request,
                    302,
                    &[
                        ("Location", "https://cn.bing.com/translator"),
                        (
                            "Set-Cookie",
                            "shared=first; Domain=.bing.com; Path=/; Secure",
                        ),
                        ("Set-Cookie", "www_only=secret; Path=/; Secure"),
                    ],
                    "",
                )
            }
            2 => {
                assert_eq!(actual.url().as_str(), "https://cn.bing.com/translator");
                assert!(cookies.contains("shared=first"));
                assert!(!cookies.contains("www_only"));
                reply(
                    request,
                    301,
                    &[
                        ("Location", "/translator?regional=1"),
                        ("Set-Cookie", "cn_only=regional; Path=/; Secure"),
                        (
                            "Set-Cookie",
                            "shared=updated; Domain=.bing.com; Path=/; Secure",
                        ),
                    ],
                    "",
                )
            }
            3 => {
                assert_eq!(
                    actual.url().as_str(),
                    "https://cn.bing.com/translator?regional=1"
                );
                assert!(cookies.contains("cn_only=regional") && cookies.contains("shared=updated"));
                assert!(!cookies.contains("shared=first"));
                reply(
                    request,
                    200,
                    &[
                        (
                            "Set-Cookie",
                            "wrong_domain=private; Domain=example.com; Path=/",
                        ),
                        ("Set-Cookie", "page_only=private; Path=/translator; Secure"),
                        ("Set-Cookie", "expired=private; Max-Age=0; Path=/"),
                        ("Set-Cookie", "final=ready; Path=/; Secure"),
                    ],
                    PAGE,
                )
            }
            _ => panic!("unexpected authorization retry"),
        }
    })?;
    assert_eq!(calls, 3);
    let request = translation_request(&client, &mut session, "Hello & 世界", "en", "zh-Hans")?;
    let actual = request.try_clone().unwrap().build()?;
    assert_eq!(actual.method(), reqwest::Method::POST);
    assert_eq!(actual.url().host_str(), Some("cn.bing.com"));
    assert_eq!(actual.url().path(), "/ttranslatev3");
    assert_eq!(actual.headers()[ORIGIN], "https://cn.bing.com");
    assert_eq!(
        actual.headers()[REFERER],
        "https://cn.bing.com/translator?regional=1"
    );
    assert!(actual
        .url()
        .query_pairs()
        .any(|(key, value)| key == "IID" && value == "translator.5028.1"));
    let cookie = actual.headers()[COOKIE].to_str()?;
    assert!(
        cookie.contains("shared=updated")
            && cookie.contains("cn_only=regional")
            && cookie.contains("final=ready")
    );
    for excluded in [
        "www_only",
        "page_only",
        "wrong_domain",
        "expired",
        "shared=first",
    ] {
        assert!(!cookie.contains(excluded));
    }
    assert!(actual.headers()[COOKIE].is_sensitive());
    let body = std::str::from_utf8(actual.body().unwrap().as_bytes().unwrap())?;
    assert!(body.contains("text=Hello+%26+%E4%B8%96%E7%95%8C"));
    assert!(body.contains("token=synthetic-token") && body.contains("to=zh-Hans"));
    assert!(!actual.url().as_str().contains("synthetic-token"));
    let response = reply(
        request,
        200,
        &[],
        r#"[{"translations":[{"text":"你好，世界","to":"zh-Hans"}]}]"#,
    )?;
    assert_eq!(
        parse_edge_translation(&read_json(response, "默认翻译")?)?,
        "你好，世界"
    );
    Ok(())
}

#[test]
fn authorization_rejects_unsafe_or_missing_locations_before_another_request() -> Result<()> {
    let client = client()?;
    for location in [
        "http://cn.bing.com/translator",
        "https://bing.com.evil.invalid/translator?synthetic-token",
        "https://other.bing.com/translator",
        "https://cn.bing.com:8443/translator",
        "https://synthetic-token@cn.bing.com/translator",
        "https://cn.bing.com:password@evil.invalid/",
        "file:///C:/private",
        "https://127.0.0.1/translator",
        "",
    ] {
        let mut calls = 0;
        let result = authorize_with(&client, &|| false, |request| {
            calls += 1;
            let headers = if location.is_empty() {
                vec![]
            } else {
                vec![("Location", location)]
            };
            reply(request, 302, &headers, "private-body")
        });
        let error = result.err().expect("unsafe redirect must fail").to_string();
        assert!(error.contains("跳转地址不受支持"));
        assert!(!error.contains("synthetic-token") && !error.contains("private-body"));
        assert_eq!(calls, 1);
    }
    Ok(())
}

#[test]
fn authorization_rejects_loops_and_caps_unique_redirects() -> Result<()> {
    let client = client()?;
    let mut calls = 0;
    let looping = authorize_with(&client, &|| false, |request| {
        calls += 1;
        let location = if calls == 1 {
            "https://cn.bing.com/translator"
        } else {
            "https://www.bing.com/translator#fragment"
        };
        reply(request, 302, &[("Location", location)], "")
    });
    assert!(looping.err().unwrap().to_string().contains("循环跳转"));
    assert_eq!(calls, 2);
    calls = 0;
    let excessive = authorize_with(&client, &|| false, |request| {
        calls += 1;
        reply(
            request,
            307,
            &[("Location", &format!("/translator?hop={calls}"))],
            "",
        )
    });
    assert!(excessive.err().unwrap().to_string().contains("次数过多"));
    assert_eq!(calls, MAX_REDIRECTS + 1);
    Ok(())
}

#[test]
fn authorization_accepts_the_bounded_supported_redirect_statuses() -> Result<()> {
    let client = client()?;
    let mut calls = 0;
    let statuses = [301, 302, 303, 307, 308];
    let session = authorize_with(&client, &|| false, |request| {
        calls += 1;
        if calls <= statuses.len() {
            reply(
                request,
                statuses[calls - 1],
                &[("Location", &format!("/translator?hop={calls}"))],
                "",
            )
        } else {
            reply(request, 200, &[], PAGE)
        }
    })?;
    assert_eq!(calls, MAX_REDIRECTS + 1);
    assert_eq!(session.page_url.query(), Some("hop=5"));
    Ok(())
}

#[test]
fn cancelled_authorization_never_sends_the_next_hop() -> Result<()> {
    let client = client()?;
    let calls = std::cell::Cell::new(0);
    let result = authorize_with(&client, &|| calls.get() != 0, |request| {
        calls.set(calls.get() + 1);
        reply(
            request,
            302,
            &[("Location", "https://cn.bing.com/translator")],
            "",
        )
    });
    assert_eq!(result.err().unwrap().to_string(), "任务已取消");
    assert_eq!(calls.get(), 1);
    Ok(())
}

#[test]
fn authorization_still_reports_http_failures_and_limits_cookie_data() -> Result<()> {
    let client = client()?;
    let mut calls = 0;
    let failed = authorize_with(&client, &|| false, |request| {
        calls += 1;
        reply(request, 403, &[], "synthetic-private-error")
    });
    let error = failed.err().unwrap().to_string();
    assert!(error.contains("HTTP 403") && !error.contains("synthetic-private-error"));
    assert_eq!(calls, 1);
    let oversized = "x".repeat(MAX_COOKIE_BYTES + 1);
    let result = authorize_with(&client, &|| false, |request| {
        reply(request, 200, &[("Set-Cookie", &oversized)], PAGE)
    });
    assert!(result.err().unwrap().to_string().contains("Cookie 过长"));
    Ok(())
}

#[test]
fn translation_post_redirect_is_an_error_not_a_translation_result() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let client = client()?;
    let mut session = parse_bing_session(PAGE)?;
    let request = translation_request(&client, &mut session, "synthetic text", "en", "zh-Hans")?;
    let response = reply(
        request,
        302,
        &[(
            "Location",
            &format!("http://{}/unexpected", listener.local_addr()?),
        )],
        "",
    )?;
    assert_eq!(response.status().as_u16(), 302);
    assert!(read_json(response, "默认翻译")
        .unwrap_err()
        .to_string()
        .contains("HTTP 302"));
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    Ok(())
}

#[test]
#[ignore = "explicit live smoke: sends only synthetic Hello world to Microsoft"]
fn live_default_bing_translation_round_trip() -> Result<()> {
    let proxy_mode = match std::env::var("SIGHTOCR_BING_TEST_PROXY").as_deref() {
        Ok("direct") => ProxyMode::Direct,
        _ => ProxyMode::System,
    };
    let config = Config {
        source_lang: "en".into(),
        target_lang: "zh-Hans".into(),
        last_translate_selection: "默认".into(),
        proxy: ProxyConfig {
            mode: proxy_mode,
            ..ProxyConfig::default()
        },
        ..Config::default()
    };
    let mut services = Services::new()?;
    let translated = services.translate("Hello world", &config)?;
    assert!(!translated.trim().is_empty());
    assert_ne!(translated, "Hello world");
    let first_request_count = services.bing_session.as_ref().unwrap().requests;
    assert!(!services
        .translate("Hello world", &config)?
        .trim()
        .is_empty());
    let session = services.bing_session.as_ref().unwrap();
    assert!(session.requests > first_request_count);
    // Metadata only: never emit tokens, cookies, raw page HTML, or user config.
    eprintln!("Bing synthetic translation passed; proxy={proxy_mode:?}; final_host={}; session_reused=true; output_chars={}", session.page_url.host_str().unwrap(), translated.chars().count());
    Ok(())
}
