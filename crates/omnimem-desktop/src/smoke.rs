//! The desktop smoke test's check of the settings panel: the `/admin`
//! routes served on a spare loopback port, signed in to with a sign-in link
//! as the tray's Settings item does, then a page, the stylesheet and signing
//! out, all over real HTTP. No services start and no browser opens.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use omnimem_settings::{AccessConfig, Panel, TRAY_CODE_LIFETIME, mint_sign_in_code};
use omnimem_store::Store;

/// One HTTP/1.1 exchange: the status, the raw headers and the body.
struct Exchange {
    status: u16,
    headers: String,
    body: Vec<u8>,
}

impl Exchange {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }
}

fn exchange(addr: SocketAddr, request: &str) -> Result<Exchange, String> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .map_err(|e| format!("connecting to {addr}: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("sending: {e}"))?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("reading: {e}"))?;
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("no end of headers")?;
    let headers = String::from_utf8_lossy(&raw[..split]).into_owned();
    let status = headers
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("no status in {headers:?}"))?;
    Ok(Exchange {
        status,
        headers,
        body: raw[split + 4..].to_vec(),
    })
}

fn get(addr: SocketAddr, path: &str, cookie: &str) -> Result<Exchange, String> {
    exchange(
        addr,
        &format!(
            "GET {path} HTTP/1.1\r\nHost: {addr}\r\nCookie: {cookie}\r\nConnection: close\r\n\r\n"
        ),
    )
}

/// Serve the panel on `runtime`, run the checks, and say what they found.
pub fn check(runtime: &tokio::runtime::Runtime) -> Result<String, String> {
    let store = Arc::new(Store::open_in_memory().map_err(|e| e.to_string())?);
    let app = Panel::new().admin_router(store.clone(), AccessConfig::default());
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .map_err(|e| format!("binding a port: {e}"))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    runtime.spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });

    let code = mint_sign_in_code(&store, TRAY_CODE_LIFETIME)?;
    let signed_in = get(addr, &format!("/admin/sign-in?code={code}"), "")?;
    let cookie = signed_in
        .header("set-cookie")
        .and_then(|c| c.split(';').next())
        .filter(|_| signed_in.status == 303)
        .ok_or_else(|| format!("the sign-in link answered {}", signed_in.status))?
        .to_owned();

    let page = get(addr, "/admin/", &cookie)?;
    let html = String::from_utf8_lossy(&page.body);
    if page.status != 200 || !html.contains("OmniMem") {
        return Err(format!(
            "the panel answered {} after signing in",
            page.status
        ));
    }
    let css = get(addr, "/admin/static/style.css", &cookie)?;
    if css.status != 200 || css.body.len() < 1000 {
        return Err(format!(
            "the stylesheet came back with {} bytes",
            css.body.len()
        ));
    }
    let out = exchange(
        addr,
        &format!(
            "POST /admin/logout HTTP/1.1\r\nHost: {addr}\r\nCookie: {cookie}\r\nOrigin: http://{addr}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ),
    )?;
    let after = get(addr, "/admin/", &cookie)?;
    if out.status != 303 || after.status != 303 {
        return Err("signing out left the session working".to_owned());
    }
    Ok(format!(
        "signed in with a link, page and stylesheet ({} bytes) served, signed out",
        css.body.len()
    ))
}
