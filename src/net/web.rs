use alloc::string::String;

pub struct Url<'a> {
    pub host: &'a str,
    pub port: u16,
    pub path: &'a str,
}

impl<'a> Url<'a> {
    pub fn parse(input: &'a str) -> Result<Self, &'static str> {
        let rest = input.strip_prefix("http://").ok_or("use an http:// URL")?;
        let (authority, path) = match rest.find('/') {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, "/"),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, port.parse().map_err(|_| "invalid HTTP port")?),
            None => (authority, 80),
        };
        if host.is_empty()
            || !host
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
            || port == 0
            || !path
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && byte != b'#')
        {
            return Err("invalid HTTP URL");
        }
        Ok(Self { host, port, path })
    }
}

pub fn render(response: &[u8]) -> Result<String, &'static str> {
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or("invalid HTTP response")?;
    let header = &response[..header_end];
    let body = &response[header_end + 4..];
    let status_end = header
        .windows(2)
        .position(|window| window == b"\r\n")
        .unwrap_or(header.len());
    let status = &header[..status_end];
    if !status.starts_with(b"HTTP/") {
        return Err("invalid HTTP status");
    }
    let mut output = String::new();
    for &byte in status {
        if byte.is_ascii_graphic() || byte == b' ' {
            output.push(byte as char);
        }
    }
    output.push('\n');
    let html = header.split(|byte| *byte == b'\n').any(|line| {
        line.to_ascii_lowercase()
            .starts_with(b"content-type: text/html")
    });
    if html {
        render_html(body, &mut output);
    } else {
        for &byte in body {
            if byte == b'\n' || byte == b'\t' || (byte.is_ascii() && !byte.is_ascii_control()) {
                output.push(byte as char);
            }
        }
    }
    Ok(output)
}

fn render_html(body: &[u8], output: &mut String) {
    let mut index = 0;
    let mut hidden = false;
    while index < body.len() {
        if body[index] == b'<'
            && let Some(end) = body[index..].iter().position(|byte| *byte == b'>')
        {
            let tag = &body[index + 1..index + end];
            let name_end = tag
                .iter()
                .position(|byte| byte.is_ascii_whitespace())
                .unwrap_or(tag.len());
            let name = &tag[..name_end];
            if name.eq_ignore_ascii_case(b"script") || name.eq_ignore_ascii_case(b"style") {
                hidden = true;
            } else if name.eq_ignore_ascii_case(b"/script") || name.eq_ignore_ascii_case(b"/style")
            {
                hidden = false;
            } else if !hidden && matches_block(name) && !output.ends_with('\n') {
                output.push('\n');
            }
            index += end + 1;
            continue;
        }
        if !hidden {
            let decoded = [
                (b"&amp;".as_slice(), '&'),
                (b"&lt;".as_slice(), '<'),
                (b"&gt;".as_slice(), '>'),
                (b"&quot;".as_slice(), '"'),
                (b"&nbsp;".as_slice(), ' '),
            ];
            if let Some((entity, character)) = decoded
                .iter()
                .find(|(entity, _)| body[index..].starts_with(entity))
            {
                output.push(*character);
                index += entity.len();
                continue;
            }
            let byte = body[index];
            if byte.is_ascii_whitespace() {
                if !output.ends_with(' ') && !output.ends_with('\n') {
                    output.push(' ');
                }
            } else if byte.is_ascii_graphic() {
                output.push(byte as char);
            }
        }
        index += 1;
    }
}

fn matches_block(name: &[u8]) -> bool {
    [
        b"br".as_slice(),
        b"p",
        b"/p",
        b"div",
        b"/div",
        b"li",
        b"/li",
        b"h1",
        b"/h1",
        b"h2",
        b"/h2",
        b"title",
        b"/title",
    ]
    .iter()
    .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

#[test_case]
fn parses_http_url_and_renders_html() {
    let url = Url::parse("http://example.com:8080/a").unwrap();
    assert_eq!(url.host, "example.com");
    assert_eq!(url.port, 8080);
    assert_eq!(url.path, "/a");
    assert!(Url::parse("https://example.com").is_err());
    let rendered = render(b"HTTP/1.0 200 OK\r\nContent-Type: text/html\r\n\r\n<h1>Hello &amp; world</h1><script>ignore</script><p>CrabOS</p>").unwrap();
    assert!(rendered.contains("Hello & world"));
    assert!(rendered.contains("CrabOS"));
    assert!(!rendered.contains("ignore"));
}
