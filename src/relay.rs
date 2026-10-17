// Relay SOCKS5 cuc bo (chi dung std).
// Java khong nhan duoc user/pass SOCKS qua tham so dong lenh (JVM bo qua -Djava.net.socks.username),
// nen moi tab tro vao relay nay (127.0.0.1:<cong>, khong mat khau); relay tu dang nhap proxy that
// (SOCKS5 hoac HTTP CONNECT, co hoac khong mat khau) roi chuyen du lieu hai chieu.
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const TMO: Duration = Duration::from_secs(15);

#[derive(Clone)]
pub struct Upstream {
    pub http: bool,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub pass: String,
}

impl Upstream {
    // Mo ta de hien trong danh sach (khong lo mat khau).
    pub fn describe(&self) -> String {
        format!(
            "{} {}:{}{}",
            if self.http { "http" } else { "socks5" },
            self.host,
            self.port,
            if self.user.is_empty() { "" } else { " (tk)" }
        )
    }
}

// Cac dang chap nhan:
//   host:port                      (SOCKS5, khong mat khau)
//   host:port:user:pass            (dang hay gap trong danh sach proxy)
//   socks5://[user:pass@]host:port
//   http://[user:pass@]host:port
pub fn parse(spec: &str) -> Result<Option<Upstream>, String> {
    let t = spec.trim().trim_matches('"');
    if t.is_empty() {
        return Ok(None);
    }
    if t.chars().any(|c| c.is_whitespace() || c == '"' || c == '|' || c.is_control()) {
        return Err("Proxy không được chứa khoảng trắng, dấu \" hoặc |".into());
    }
    let lower = t.to_lowercase();
    let mut http = false;
    let mut rest = t;
    for p in ["socks5h://", "socks5://", "socks://"] {
        if lower.starts_with(p) {
            rest = &t[p.len()..];
        }
    }
    for p in ["https://", "http://"] {
        if lower.starts_with(p) {
            http = true;
            rest = &t[p.len()..];
        }
    }
    let rest = rest.trim_end_matches('/');
    let (user, pass, host, port);
    if let Some((cred, hp)) = rest.rsplit_once('@') {
        let (u, p) = cred.split_once(':').unwrap_or((cred, ""));
        user = u.to_string();
        pass = p.to_string();
        let (h, po) = hp.rsplit_once(':').ok_or("Thiếu cổng proxy (host:cổng)")?;
        host = h.to_string();
        port = po.to_string();
    } else {
        let parts: Vec<&str> = if rest.starts_with('[') {
            match rest.rsplit_once("]:") {
                Some((h, po)) => vec![h, po], // IPv6: [::1]:1080
                None => vec![],
            }
        } else {
            rest.split(':').collect()
        };
        match parts.len() {
            2 => {
                host = parts[0].to_string();
                port = parts[1].to_string();
                user = String::new();
                pass = String::new();
            }
            4 => {
                host = parts[0].to_string();
                port = parts[1].to_string();
                user = parts[2].to_string();
                pass = parts[3].to_string();
            }
            _ => return Err("Proxy phải có dạng host:cổng, host:cổng:user:pass, socks5://user:pass@host:cổng hoặc http://host:cổng".into()),
        }
    }
    let host = host.trim_matches(|c| c == '[' || c == ']').to_string();
    let port: u16 = match port.parse() {
        Ok(p) if p > 0 => p,
        _ => return Err("Cổng proxy không đúng".into()),
    };
    if host.is_empty() {
        return Err("Thiếu địa chỉ proxy".into());
    }
    if user.len() > 255 || pass.len() > 255 {
        return Err("Tài khoản/mật khẩu proxy quá dài (tối đa 255 ký tự)".into());
    }
    Ok(Some(Upstream { http, host, port, user, pass }))
}

fn err<T>(m: &str) -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::Other, m.to_string()))
}

enum Dest {
    V4([u8; 4]),
    V6([u8; 16]),
    Name(String),
}

impl Dest {
    fn text(&self) -> String {
        match self {
            Dest::V4(a) => Ipv4Addr::from(*a).to_string(),
            Dest::V6(a) => format!("[{}]", Ipv6Addr::from(*a)),
            Dest::Name(n) => n.clone(),
        }
    }
    fn is_local(&self) -> bool {
        match self {
            Dest::V4(a) => a[0] == 127,
            Dest::V6(a) => Ipv6Addr::from(*a).is_loopback(),
            Dest::Name(n) => n.eq_ignore_ascii_case("localhost"),
        }
    }
    // them vao goi SOCKS5: ATYP + dia chi + cong
    fn put(&self, out: &mut Vec<u8>, port: u16) {
        match self {
            Dest::V4(a) => {
                out.push(1);
                out.extend_from_slice(a);
            }
            Dest::V6(a) => {
                out.push(4);
                out.extend_from_slice(a);
            }
            Dest::Name(n) => {
                out.push(3);
                out.push(n.len().min(255) as u8);
                out.extend_from_slice(&n.as_bytes()[..n.len().min(255)]);
            }
        }
        out.extend_from_slice(&port.to_be_bytes());
    }
}

fn b64(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for ch in data.chunks(3) {
        let n = (ch[0] as u32) << 16 | (*ch.get(1).unwrap_or(&0) as u32) << 8 | *ch.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if ch.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if ch.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

fn connect_any(host: &str, port: u16) -> io::Result<TcpStream> {
    let mut last = io::Error::new(io::ErrorKind::Other, "Không phân giải được địa chỉ");
    for addr in (host, port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&addr, TMO) {
            Ok(s) => return Ok(s),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn socks5_connect(s: &mut TcpStream, up: &Upstream, dest: &Dest, port: u16) -> io::Result<()> {
    let auth = !up.user.is_empty();
    s.write_all(if auth { &[5, 2, 0, 2][..] } else { &[5, 1, 0][..] })?;
    let mut r = [0u8; 2];
    s.read_exact(&mut r)?;
    if r[0] != 5 {
        return err("Proxy không phải SOCKS5");
    }
    match r[1] {
        0 => {}
        2 if auth => {
            let mut m = vec![1u8, up.user.len() as u8];
            m.extend_from_slice(up.user.as_bytes());
            m.push(up.pass.len() as u8);
            m.extend_from_slice(up.pass.as_bytes());
            s.write_all(&m)?;
            let mut a = [0u8; 2];
            s.read_exact(&mut a)?;
            if a[1] != 0 {
                return err("Proxy từ chối tài khoản/mật khẩu");
            }
        }
        _ => return err("Proxy yêu cầu cách xác thực không hỗ trợ"),
    }
    let mut req = vec![5u8, 1, 0];
    dest.put(&mut req, port);
    s.write_all(&req)?;
    let mut h = [0u8; 4];
    s.read_exact(&mut h)?;
    if h[1] != 0 {
        return Err(io::Error::new(io::ErrorKind::Other, format!("Proxy từ chối kết nối (mã SOCKS {})", h[1])));
    }
    let skip = match h[3] {
        1 => 4 + 2,
        4 => 16 + 2,
        3 => {
            let mut l = [0u8; 1];
            s.read_exact(&mut l)?;
            l[0] as usize + 2
        }
        _ => return err("Phản hồi proxy không hợp lệ"),
    };
    let mut junk = vec![0u8; skip];
    s.read_exact(&mut junk)
}

fn http_connect(s: &mut TcpStream, up: &Upstream, dest: &Dest, port: u16) -> io::Result<()> {
    let hp = format!("{}:{}", dest.text(), port);
    let mut req = format!("CONNECT {hp} HTTP/1.1\r\nHost: {hp}\r\n");
    if !up.user.is_empty() {
        req.push_str(&format!("Proxy-Authorization: Basic {}\r\n", b64(format!("{}:{}", up.user, up.pass).as_bytes())));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes())?;
    let mut buf: Vec<u8> = Vec::new();
    let mut b = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if s.read(&mut b)? == 0 || buf.len() > 16384 {
            return err("Proxy HTTP đóng kết nối sớm");
        }
        buf.push(b[0]);
    }
    let head = String::from_utf8_lossy(&buf).to_string();
    let code = head.lines().next().unwrap_or("").split_whitespace().nth(1).unwrap_or("");
    match code {
        "200" => Ok(()),
        "407" => err("Proxy HTTP từ chối tài khoản/mật khẩu (407)"),
        _ => Err(io::Error::new(io::ErrorKind::Other, format!("Proxy HTTP trả lỗi: {}", head.lines().next().unwrap_or("")))),
    }
}

fn open_remote(up: &Upstream, dest: &Dest, port: u16) -> io::Result<TcpStream> {
    // dich la may nay (localhost / 127.x) thi di thang, khong qua proxy
    if dest.is_local() {
        let host = match dest {
            Dest::Name(n) => n.clone(),
            other => other.text().trim_matches(|c| c == '[' || c == ']').to_string(),
        };
        return connect_any(&host, port);
    }
    let mut s = connect_any(&up.host, up.port)?;
    s.set_read_timeout(Some(TMO))?;
    s.set_write_timeout(Some(TMO))?;
    s.set_nodelay(true).ok();
    if up.http {
        http_connect(&mut s, up, dest, port)?;
    } else {
        socks5_connect(&mut s, up, dest, port)?;
    }
    s.set_read_timeout(None)?;
    s.set_write_timeout(None)?;
    Ok(s)
}

fn reply(code: u8) -> [u8; 10] {
    [5, code, 0, 1, 0, 0, 0, 0, 0, 0]
}

fn handle(mut c: TcpStream, up: Upstream) -> io::Result<()> {
    c.set_read_timeout(Some(TMO))?;
    c.set_nodelay(true).ok();
    let mut h = [0u8; 2];
    c.read_exact(&mut h)?;
    if h[0] != 5 {
        return err("Không phải SOCKS5");
    }
    let mut methods = vec![0u8; h[1] as usize];
    c.read_exact(&mut methods)?;
    c.write_all(&[5, 0])?; // khong xac thuc (chi lang nghe 127.0.0.1)
    let mut q = [0u8; 4];
    c.read_exact(&mut q)?;
    if q[0] != 5 || q[1] != 1 {
        let _ = c.write_all(&reply(7));
        return err("Chỉ hỗ trợ lệnh CONNECT");
    }
    let dest = match q[3] {
        1 => {
            let mut a = [0u8; 4];
            c.read_exact(&mut a)?;
            Dest::V4(a)
        }
        4 => {
            let mut a = [0u8; 16];
            c.read_exact(&mut a)?;
            Dest::V6(a)
        }
        3 => {
            let mut l = [0u8; 1];
            c.read_exact(&mut l)?;
            let mut n = vec![0u8; l[0] as usize];
            c.read_exact(&mut n)?;
            Dest::Name(String::from_utf8_lossy(&n).to_string())
        }
        _ => {
            let _ = c.write_all(&reply(8));
            return err("Kiểu địa chỉ không hỗ trợ");
        }
    };
    let mut p = [0u8; 2];
    c.read_exact(&mut p)?;
    let port = u16::from_be_bytes(p);
    match open_remote(&up, &dest, port) {
        Ok(r) => {
            c.write_all(&reply(0))?;
            c.set_read_timeout(None)?;
            pump(c, r)
        }
        Err(e) => {
            let _ = c.write_all(&reply(5));
            Err(e)
        }
    }
}

fn pump(c: TcpStream, r: TcpStream) -> io::Result<()> {
    let (mut c_in, mut r_out) = (c.try_clone()?, r.try_clone()?);
    thread::spawn(move || {
        let _ = io::copy(&mut c_in, &mut r_out);
        let _ = r_out.shutdown(Shutdown::Write);
    });
    let (mut r_in, mut c_out) = (r, c);
    let _ = io::copy(&mut r_in, &mut c_out);
    let _ = c_out.shutdown(Shutdown::Write);
    Ok(())
}

// Relay song cung Tab: bo Tab (drop) thi relay dung nhan ket noi moi.
pub struct Relay {
    port: u16,
    stop: Arc<AtomicBool>,
}

impl Relay {
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], self.port))); // danh thuc luong accept
    }
}

pub fn start(up: Upstream) -> io::Result<Relay> {
    let l = TcpListener::bind(("127.0.0.1", 0))?;
    let port = l.local_addr()?.port();
    let stop = Arc::new(AtomicBool::new(false));
    let st = stop.clone();
    thread::spawn(move || {
        for c in l.incoming() {
            if st.load(Ordering::Relaxed) {
                break;
            }
            if let Ok(c) = c {
                let up = up.clone();
                thread::spawn(move || {
                    let _ = handle(c, up);
                });
            }
        }
    });
    Ok(Relay { port, stop })
}
