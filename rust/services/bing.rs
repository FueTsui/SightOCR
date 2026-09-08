//! Bing's public translator page is region-routed. Only its authorization GET
//! may follow redirects; API requests never inherit this exception.

use super::*;
use reqwest::cookie::{CookieStore, Jar};
use reqwest::header::{
    HeaderMap, HeaderValue, CACHE_CONTROL, COOKIE, LOCATION, ORIGIN, REFERER, SET_COOKIE,
};
use reqwest::Url;

pub(super) const TRANSLATOR_URL: &str = "https://www.bing.com/translator";
const MAX_REDIRECTS: usize = 5;
const MAX_COOKIE_BYTES: usize = 16_384;
const AUTHORIZATION_TIMEOUT: Duration = Duration::from_secs(15);

pub(super) fn authorize(client: &Client, cancelled: &dyn Fn() -> bool) -> Result<BingSession> {
    authorize_with(client, cancelled, send)
}

fn authorize_with(
    client: &Client,
    cancelled: &dyn Fn() -> bool,
    mut request: impl FnMut(RequestBuilder) -> Result<Response>,
) -> Result<BingSession> {
    let cookies = Jar::default();
    let mut url = Url::parse(TRANSLATOR_URL).expect("the built-in translator URL is valid");
    let mut visited = vec![url.clone()];
    let deadline = Instant::now() + AUTHORIZATION_TIMEOUT;
    loop {
        anyhow::ensure!(!cancelled(), "任务已取消");
        let timeout = deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or(ServiceError::Network("默认翻译授权超时，请稍后重试"))?;
        let mut get = client
            .get(url.clone())
            .header(CACHE_CONTROL, "no-cache")
            .timeout(timeout);
        if let Some(cookie) = cookie_header(&cookies, &url)? {
            get = get.header(COOKIE, cookie);
        }
        let response = request(get)?;
        anyhow::ensure!(!cancelled(), "任务已取消");
        store_cookies(&cookies, response.headers(), &url)?;
        if matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
            let next = redirect_url(&url, response.headers())?;
            if visited.contains(&next) {
                return Err(ServiceError::Network(
                    "默认翻译授权发生循环跳转，请检查网络或代理后重试",
                )
                .into());
            }
            if visited.len() > MAX_REDIRECTS {
                return Err(ServiceError::Network(
                    "默认翻译授权跳转次数过多，请检查网络或代理后重试",
                )
                .into());
            }
            visited.push(next.clone());
            url = next;
            continue;
        }
        let bytes = read_response(response, "默认翻译授权", 2 * 1024 * 1024)?;
        let html = std::str::from_utf8(&bytes)
            .map_err(|_| ServiceError::InvalidResponse("翻译页面不是 UTF-8"))?;
        let mut session = parse_bing_session(html)?;
        session.cookies = cookies;
        session.page_url = url;
        return Ok(session);
    }
}

fn redirect_url(current: &Url, headers: &HeaderMap) -> Result<Url> {
    let invalid = || ServiceError::Network("默认翻译授权跳转地址不受支持，请检查网络或代理后重试");
    let location = headers
        .get(LOCATION)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty() && value.len() <= 4096)
        .ok_or_else(invalid)?;
    let mut next = current.join(location).map_err(|_| invalid())?;
    // Exact hosts deliberately exclude arbitrary *.bing.com tenants, suffix
    // lookalikes, embedded credentials, nonstandard ports, and HTTPS downgrades.
    if next.scheme() != "https"
        || !matches!(next.host_str(), Some("www.bing.com" | "cn.bing.com"))
        || next.port_or_known_default() != Some(443)
        || !next.username().is_empty()
        || next.password().is_some()
    {
        return Err(invalid().into());
    }
    // Fragments never reach HTTP; normalize them before detecting loops.
    next.set_fragment(None);
    Ok(next)
}

fn cookie_header(cookies: &Jar, url: &Url) -> Result<Option<HeaderValue>> {
    let Some(mut cookie) = cookies.cookies(url) else {
        return Ok(None);
    };
    if cookie.as_bytes().len() > MAX_COOKIE_BYTES {
        return Err(ServiceError::InvalidResponse("翻译会话 Cookie 过长").into());
    }
    cookie.set_sensitive(true);
    Ok(Some(cookie))
}

fn store_cookies(cookies: &Jar, headers: &HeaderMap, url: &Url) -> Result<()> {
    let headers = headers.get_all(SET_COOKIE);
    if headers
        .iter()
        .map(|value| value.as_bytes().len())
        .sum::<usize>()
        > MAX_COOKIE_BYTES
    {
        return Err(ServiceError::InvalidResponse("翻译会话 Cookie 过长").into());
    }
    // Jar enforces host/domain/path/expiry rules for every redirect hop and
    // replaces cookies by identity. A www host-only cookie cannot leak to cn.
    cookies.set_cookies(&mut headers.iter(), url);
    Ok(())
}

pub(super) fn translation_request(
    client: &Client,
    session: &mut BingSession,
    text: &str,
    source: &str,
    target: &str,
) -> Result<RequestBuilder> {
    let endpoint = session
        .page_url
        .join("/ttranslatev3")
        .map_err(|_| ServiceError::InvalidResponse("翻译会话地址无效"))?;
    session.requests += 1;
    let iid = format!("{}.{}", session.iid, session.requests);
    let mut request = client
        .post(endpoint.clone())
        .query(&[
            ("isVertical", "1"),
            ("IG", session.ig.as_str()),
            ("IID", iid.as_str()),
        ])
        .header(REFERER, session.page_url.as_str())
        .header(ORIGIN, session.page_url.origin().ascii_serialization())
        .form(&[
            ("fromLang", source),
            ("to", target),
            ("text", text),
            ("token", session.token.as_str()),
            ("key", session.key.as_str()),
        ]);
    if let Some(cookie) = cookie_header(&session.cookies, &endpoint)? {
        request = request.header(COOKIE, cookie);
    }
    Ok(request)
}

pub(super) fn store_response_cookies(session: &BingSession, response: &Response) -> Result<()> {
    store_cookies(&session.cookies, response.headers(), response.url())
}

#[cfg(test)]
#[path = "bing_tests.rs"]
mod tests;
