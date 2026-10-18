// Toonie Remoter (Rust, thuan Win32) - mo nhieu tab MicroEmulator, sap xep cua so, gui phim dong loat.
// Chay duoc tu Windows 7 / Server 2008 R2 tro len (ke ca Windows 8 / Server 2012). Khong can .NET.
#![windows_subsystem = "windows"]
#![allow(non_snake_case)]
#![allow(dead_code)]
#![allow(unused_imports)]

use std::ffi::c_void;
use std::collections::VecDeque;
use std::io::Read;
use std::mem::{size_of, zeroed};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

mod relay;

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::*;
use windows_sys::Win32::System::ProcessStatus::*;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::SystemInformation::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::UI::Controls::Dialogs::*;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

// ------------------------------------------------------------------ tien ich
fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn get_text(h: HWND) -> String {
    let n = GetWindowTextLengthW(h);
    if n <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; n as usize + 1];
    let got = GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32);
    String::from_utf16_lossy(&buf[..got.max(0) as usize])
}

unsafe fn set_text(h: HWND, s: &str) {
    SetWindowTextW(h, w(s).as_ptr());
}

unsafe fn msgbox(owner: HWND, text: &str, title: &str, flags: u32) -> i32 {
    MessageBoxW(owner, w(text).as_ptr(), w(title).as_ptr(), flags)
}

// ------------------------------------------------------------------ proxy da luu
#[derive(Clone)]
struct ProxyItem {
    name: String,
    http: bool, // false = SOCKS5, true = HTTP
    host: String,
    port: u16,
    user: String,
    pass: String,
    result: String, // ket qua kiem tra gan nhat (khong luu file)
}

impl ProxyItem {
    fn upstream(&self) -> relay::Upstream {
        relay::Upstream { http: self.http, host: self.host.clone(), port: self.port, user: self.user.clone(), pass: self.pass.clone() }
    }
    fn cells(&self) -> Vec<String> {
        vec![
            self.name.clone(),
            (if self.http { "HTTP" } else { "SOCKS5" }).to_string(),
            self.host.clone(),
            self.port.to_string(),
            self.user.clone(),
            if self.pass.is_empty() { String::new() } else { "****".to_string() },
            self.result.clone(),
        ]
    }
}

static PROXIES: Mutex<Vec<ProxyItem>> = Mutex::new(Vec::new());

// proxy theo ten (dong/o mac dinh luu TEN proxy); neu khong thay ten thi thu hieu nhu chuoi proxy cu
fn resolve_proxy(spec: &str) -> Result<Option<relay::Upstream>, String> {
    let t = spec.trim();
    if t.is_empty() {
        return Ok(None);
    }
    if let Some(p) = PROXIES.lock().unwrap().iter().find(|p| p.name == t) {
        return Ok(Some(p.upstream()));
    }
    relay::parse(t).map_err(|_| format!("Không tìm thấy proxy \"{}\" (đã bị xóa?)", t))
}

// ------------------------------------------------------------------ cai dat
#[derive(Clone)]
struct Settings {
    emulator: String,
    game: String,
    java: String,
    width: i32,
    height: i32,
    tabs: i32,
    wait: u64,
    key_delay: u64,
    key_hold: u64,
    key_post: bool,
    capture: bool,
    sleep_after: u64,
    separate_id: bool,
    proxy: String,                    // TEN proxy mac dinh cho cac dong chua gan proxy rieng
    proxies: Vec<ProxyItem>,          // danh sach proxy da luu (chi dung luc nap file ini)
    slots: Vec<(bool, String, String)>, // (auto, game, proxy) - chi dung luc nap file ini
}

impl Settings {
    const fn new() -> Self {
        Settings {
            emulator: String::new(),
            game: String::new(),
            java: String::new(),
            width: 180,
            height: 160,
            tabs: 1,
            wait: 1000,
            key_delay: 60,
            key_hold: 40,
            key_post: false,
            capture: true,
            sleep_after: 30,
            separate_id: false,
            proxy: String::new(),
            proxies: Vec::new(),
            slots: Vec::new(),
        }
    }

    fn path() -> PathBuf {
        let mut p = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ToonieRemoter.exe"));
        p.set_file_name("ToonieRemoter.ini");
        p
    }

    fn load() -> Self {
        let mut s = Settings::new();
        if let Ok(txt) = std::fs::read_to_string(Self::path()) {
            for line in txt.trim_start_matches('\u{feff}').lines() {
                if let Some((k, v)) = line.split_once('=') {
                    let (k, v) = (k.trim().to_lowercase(), v.trim());
                    match k.as_str() {
                        "emulator" => s.emulator = v.into(),
                        "game" => s.game = v.into(),
                        "java" => s.java = v.into(),
                        "width" => s.width = v.parse().unwrap_or(s.width),
                        "height" => s.height = v.parse().unwrap_or(s.height),
                        "tabs" => s.tabs = v.parse().unwrap_or(s.tabs),
                        "wait" => s.wait = v.parse().unwrap_or(s.wait),
                        "keydelay" => s.key_delay = v.parse().unwrap_or(s.key_delay),
                        "keyhold" => s.key_hold = v.parse().unwrap_or(s.key_hold),
                        "keymode" => s.key_post = v.eq_ignore_ascii_case("post"),
                        "capture" => s.capture = v != "0",
                        "sleepafter" => s.sleep_after = v.parse().unwrap_or(s.sleep_after),
                        "slot" => {
                            let mut it = v.splitn(3, '|');
                            if let (Some(a), Some(g)) = (it.next(), it.next()) {
                                s.slots.push((a == "1", g.to_string(), it.next().unwrap_or("").to_string()));
                            }
                        }
                        "separateid" => s.separate_id = v == "1",
                        "proxy" => s.proxy = v.into(),
                        "px" => {
                            let f: Vec<&str> = v.split('\t').collect();
                            if f.len() >= 4 && !f[0].is_empty() && !f[2].is_empty() {
                                if let Ok(port) = f[3].trim().parse::<u16>() {
                                    s.proxies.push(ProxyItem {
                                        name: f[0].to_string(),
                                        http: f[1] == "http",
                                        host: f[2].to_string(),
                                        port,
                                        user: f.get(4).unwrap_or(&"").to_string(),
                                        pass: f.get(5).unwrap_or(&"").to_string(),
                                        result: String::new(),
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        s
    }

    fn save(&self) {
        let txt = format!(
            "emulator={}\r\ngame={}\r\njava={}\r\nwidth={}\r\nheight={}\r\ntabs={}\r\nwait={}\r\nkeydelay={}\r\nkeyhold={}\r\nkeymode={}\r\ncapture={}\r\nsleepafter={}\r\nseparateid={}\r\nproxy={}\r\n",
            self.emulator, self.game, self.java, self.width, self.height, self.tabs, self.wait, self.key_delay, self.key_hold,
            if self.key_post { "post" } else { "focus" },
            if self.capture { 1 } else { 0 },
            self.sleep_after,
            if self.separate_id { 1 } else { 0 },
            self.proxy
        );
        let mut txt = txt;
        if let Ok(g) = SLOTS.lock() {
            for sl in g.iter() {
                txt.push_str(&format!("slot={}|{}|{}\r\n", if sl.auto { 1 } else { 0 }, sl.game, sl.proxy));
            }
        }
        if let Ok(g) = PROXIES.lock() {
            for p in g.iter() {
                txt.push_str(&format!("px={}\t{}\t{}\t{}\t{}\t{}\r\n", p.name, if p.http { "http" } else { "socks5" }, p.host, p.port, p.user, p.pass));
            }
        }
        let _ = std::fs::write(Self::path(), txt);
    }
}

static CFG: Mutex<Settings> = Mutex::new(Settings::new());

// ------------------------------------------------------------------ tim Java
fn add_home(c: &mut Vec<PathBuf>, home: &str) {
    if home.is_empty() {
        return;
    }
    let p = Path::new(home);
    c.push(p.join("bin").join("javaw.exe"));
    c.push(p.join("jre").join("bin").join("javaw.exe"));
}

unsafe fn reg_open(root: HKEY, path: &str, wow: u32) -> Option<HKEY> {
    let mut k: HKEY = 0;
    if RegOpenKeyExW(root, w(path).as_ptr(), 0, KEY_READ | wow, &mut k) == 0 {
        Some(k)
    } else {
        None
    }
}

unsafe fn reg_str(k: HKEY, name: &str) -> Option<String> {
    let n = w(name);
    let (mut ty, mut sz) = (0u32, 0u32);
    if RegQueryValueExW(k, n.as_ptr(), null(), &mut ty, null_mut(), &mut sz) != 0 || sz == 0 {
        return None;
    }
    let mut buf = vec![0u16; sz as usize / 2 + 2];
    let mut sz2 = (buf.len() * 2) as u32;
    if RegQueryValueExW(k, n.as_ptr(), null(), &mut ty, buf.as_mut_ptr() as *mut u8, &mut sz2) != 0 {
        return None;
    }
    let len = (sz2 as usize / 2).min(buf.len());
    let end = buf[..len].iter().position(|&c| c == 0).unwrap_or(len);
    Some(String::from_utf16_lossy(&buf[..end]))
}

unsafe fn reg_subkeys(k: HKEY) -> Vec<String> {
    let mut v = Vec::new();
    let mut i = 0u32;
    loop {
        let mut name = [0u16; 256];
        let mut len = 256u32;
        let r = RegEnumKeyExW(k, i, name.as_mut_ptr(), &mut len, null(), null_mut(), null_mut(), null_mut());
        if r != 0 {
            break;
        }
        v.push(String::from_utf16_lossy(&name[..len as usize]));
        i += 1;
    }
    v
}

unsafe fn java_from_registry(c: &mut Vec<PathBuf>) {
    let javasoft = [
        r"SOFTWARE\JavaSoft\Java Runtime Environment",
        r"SOFTWARE\JavaSoft\JDK",
        r"SOFTWARE\JavaSoft\Java Development Kit",
    ];
    let vendors = [
        r"SOFTWARE\Eclipse Adoptium\JRE",
        r"SOFTWARE\Eclipse Adoptium\JDK",
        r"SOFTWARE\Microsoft\JDK",
        r"SOFTWARE\Azul Systems\Zulu",
        r"SOFTWARE\Amazon Corretto",
        r"SOFTWARE\Temurin\JRE",
        r"SOFTWARE\Temurin\JDK",
    ];
    for wow in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
        for p in javasoft {
            if let Some(k) = reg_open(HKEY_LOCAL_MACHINE, p, wow) {
                if let Some(cur) = reg_str(k, "CurrentVersion") {
                    if let Some(v) = reg_open(HKEY_LOCAL_MACHINE, &format!(r"{}\{}", p, cur), wow) {
                        if let Some(h) = reg_str(v, "JavaHome") {
                            add_home(c, &h);
                        }
                        RegCloseKey(v);
                    }
                }
                for sub in reg_subkeys(k) {
                    if let Some(v) = reg_open(HKEY_LOCAL_MACHINE, &format!(r"{}\{}", p, sub), wow) {
                        if let Some(h) = reg_str(v, "JavaHome") {
                            add_home(c, &h);
                        }
                        RegCloseKey(v);
                    }
                }
                RegCloseKey(k);
            }
        }
        for p in vendors {
            if let Some(k) = reg_open(HKEY_LOCAL_MACHINE, p, wow) {
                for sub in reg_subkeys(k) {
                    for tail in [r"\hotspot\MSI", r"\MSI", ""] {
                        if let Some(v) = reg_open(HKEY_LOCAL_MACHINE, &format!(r"{}\{}{}", p, sub, tail), wow) {
                            for name in ["Path", "InstallationPath", "JavaHome"] {
                                if let Some(h) = reg_str(v, name) {
                                    add_home(c, &h);
                                    break;
                                }
                            }
                            RegCloseKey(v);
                        }
                    }
                }
                RegCloseKey(k);
            }
        }
    }
}

fn find_java(preferred: &str) -> Option<PathBuf> {
    let mut c: Vec<PathBuf> = Vec::new();
    if !preferred.is_empty() {
        c.push(PathBuf::from(preferred));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            add_home(&mut c, &dir.join("jre").to_string_lossy());
            add_home(&mut c, &dir.join("java").to_string_lossy());
        }
    }
    if let Ok(jh) = std::env::var("JAVA_HOME") {
        add_home(&mut c, &jh);
    }
    unsafe { java_from_registry(&mut c) };
    if let Some(path) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&path) {
            c.push(d.join("javaw.exe"));
        }
    }
    let vendor_dirs = [
        "Java", "Eclipse Adoptium", "Microsoft", "Zulu", "Amazon Corretto", "Temurin", "BellSoft", "Semeru", "OpenJDK",
    ];
    for env in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
        if let Ok(root) = std::env::var(env) {
            for vd in vendor_dirs {
                if let Ok(rd) = std::fs::read_dir(Path::new(&root).join(vd)) {
                    for e in rd.flatten() {
                        add_home(&mut c, &e.path().to_string_lossy());
                    }
                }
            }
        }
    }
    c.into_iter().find(|p| p.is_file())
}

// ------------------------------------------------------------------ cua so cua tab
struct FindCtx {
    pid: u32,
    best: HWND,
    area: i64,
}

unsafe extern "system" fn enum_cb(h: HWND, lp: LPARAM) -> BOOL {
    let ctx = &mut *(lp as *mut FindCtx);
    let mut p = 0u32;
    GetWindowThreadProcessId(h, &mut p);
    if p != ctx.pid || IsWindowVisible(h) == 0 {
        return 1;
    }
    if GetWindow(h, GW_OWNER) != 0 || GetWindowTextLengthW(h) == 0 {
        return 1;
    }
    let mut r: RECT = zeroed();
    if GetWindowRect(h, &mut r) == 0 {
        return 1;
    }
    let a = (r.right - r.left) as i64 * (r.bottom - r.top) as i64;
    if a > ctx.area {
        ctx.area = a;
        ctx.best = h;
    }
    1
}

fn find_main_window(pid: u32) -> HWND {
    let mut ctx = FindCtx { pid, best: 0, area: -1 };
    unsafe { EnumWindows(Some(enum_cb), &mut ctx as *mut FindCtx as LPARAM) };
    ctx.best
}

type DwmFn = unsafe extern "system" fn(HWND, u32, *mut c_void, u32) -> i32;

// dwmapi.dll nap dong: Windows Server 2012 (khong cai Desktop Experience) co the khong co DWM.
fn dwm_fn() -> Option<DwmFn> {
    static F: OnceLock<usize> = OnceLock::new();
    let a = *F.get_or_init(|| unsafe {
        let m = LoadLibraryW(w("dwmapi.dll").as_ptr());
        if m == 0 {
            return 0;
        }
        match GetProcAddress(m, b"DwmGetWindowAttribute\0".as_ptr()) {
            Some(f) => f as usize,
            None => 0,
        }
    });
    if a == 0 {
        None
    } else {
        Some(unsafe { std::mem::transmute::<usize, DwmFn>(a) })
    }
}

// (hinh chu nhat day du, hinh chu nhat nhin thay)
fn get_rects(h: HWND) -> Option<(RECT, RECT)> {
    unsafe {
        let mut o: RECT = zeroed();
        if GetWindowRect(h, &mut o) == 0 {
            return None;
        }
        let mut v = o;
        if let Some(f) = dwm_fn() {
            let mut r: RECT = zeroed();
            if f(h, 9, &mut r as *mut RECT as *mut c_void, size_of::<RECT>() as u32) == 0 && r.right > r.left {
                v = r;
            }
        }
        Some((o, v))
    }
}

fn move_visible_to(h: HWND, x: i32, y: i32) {
    if let Some((o, v)) = get_rects(h) {
        unsafe {
            SetWindowPos(h, 0, x - (v.left - o.left), y - (v.top - o.top), 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
}

fn force_foreground(h: HWND) {
    unsafe {
        if GetForegroundWindow() == h {
            return;
        }
        if IsIconic(h) != 0 {
            ShowWindow(h, SW_RESTORE);
        }
        let cur = GetCurrentThreadId();
        let fg = GetWindowThreadProcessId(GetForegroundWindow(), null_mut());
        let att = fg != 0 && fg != cur && AttachThreadInput(cur, fg, 1) != 0;
        SetForegroundWindow(h);
        if att {
            AttachThreadInput(cur, fg, 0);
        }
    }
}

fn is_extended(vk: u32) -> bool {
    matches!(vk, 0x21..=0x28 | 0x2D | 0x2E)
}

// Gui phim that (SendInput) toi cua so dang la foreground.
fn send_key_input(vk: u32, hold_ms: u64) {
    unsafe {
        let sc = MapVirtualKeyW(vk, 0) as u16;
        let ext: u32 = if is_extended(vk) { KEYEVENTF_EXTENDEDKEY } else { 0 };
        let mk = |flags: u32| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk as u16, wScan: sc, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
        };
        let d = mk(ext);
        SendInput(1, &d, size_of::<INPUT>() as i32);
        thread::sleep(Duration::from_millis(hold_ms));
        let u = mk(ext | KEYEVENTF_KEYUP);
        SendInput(1, &u, size_of::<INPUT>() as i32);
    }
}

// Che do "post": gui WM_KEYDOWN/UP thang vao cua so (khong doi cua so, nhung Java co the bo qua).
fn tap_key_post(h: HWND, vk: u32) {
    unsafe {
        let sc = MapVirtualKeyW(vk, 0) as isize;
        let mut down: isize = 1 | (sc << 16);
        let mut up: isize = down | (1 << 30) | (1 << 31);
        if is_extended(vk) {
            down |= 1 << 24;
            up |= 1 << 24;
        }
        PostMessageW(h, WM_KEYDOWN, vk as WPARAM, down);
        thread::sleep(Duration::from_millis(15));
        PostMessageW(h, WM_KEYUP, vk as WPARAM, up);
    }
}

fn wait_foreground(h: HWND, ms: u64) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(ms) {
        if unsafe { GetForegroundWindow() } == h {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

fn work_area() -> RECT {
    unsafe {
        let mut r: RECT = zeroed();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut r as *mut RECT as *mut c_void, 0);
        r
    }
}

// ------------------------------------------------------------------ tab & dong (slot)
struct Tab {
    child: Child,
    pid: u32,
    hwnd: HWND,
    hidden: bool,
    sleep: u8, // 0 = thuc, 1 = ngu tu dong, 2 = ngu thu cong
    slept_at: Instant,
    last_active: Instant,
    last_cpu: u64,
    last_t: Instant,
    cpu: f32,
    ws: u64,
    private: u64,
    relay: Option<relay::Relay>, // relay proxy cua tab (bo tab = dung relay)
}

// Mot dong trong danh sach: game rieng + auto sleep rieng, co the dang chay (tab) hoac chua mo.
struct Slot {
    game: String,
    auto: bool,
    proxy: String, // rong = dung proxy mac dinh o o "Proxy"
    tab: Option<Tab>,
}

#[derive(Clone)]
struct Snap {
    slot: usize,
    pid: u32,
    hwnd: HWND,
    hidden: bool,
    sleep: u8,
}

static SLOTS: Mutex<Vec<Slot>> = Mutex::new(Vec::new());
static LAUNCHING: AtomicBool = AtomicBool::new(false);
static FONT: AtomicIsize = AtomicIsize::new(0);

fn snapshot() -> Vec<Snap> {
    let mut g = SLOTS.lock().unwrap();
    let mut out = Vec::new();
    for (i, sl) in g.iter_mut().enumerate() {
        let alive = match sl.tab.as_mut() {
            Some(t) => matches!(t.child.try_wait(), Ok(None)),
            None => false,
        };
        if !alive {
            sl.tab = None;
            continue;
        }
        let auto_ = sl.auto;
        let _ = auto_;
        let t = sl.tab.as_mut().unwrap();
        unsafe {
            if t.hwnd == 0 || IsWindow(t.hwnd) == 0 {
                t.hwnd = find_main_window(t.pid);
            }
        }
        out.push(Snap { slot: i, pid: t.pid, hwnd: t.hwnd, hidden: t.hidden, sleep: t.sleep });
    }
    out
}

fn with_tab<F: FnOnce(&mut Tab)>(slot: usize, f: F) {
    if let Some(sl) = SLOTS.lock().unwrap().get_mut(slot) {
        if let Some(t) = sl.tab.as_mut() {
            f(t);
        }
    }
}

fn get_cell(snaps: &[Snap]) -> Option<(i32, i32)> {
    let (mut cw, mut ch) = (0, 0);
    for s in snaps {
        if s.hwnd == 0 {
            continue;
        }
        if let Some((_, v)) = get_rects(s.hwnd) {
            cw = cw.max(v.right - v.left);
            ch = ch.max(v.bottom - v.top);
        }
    }
    if cw > 0 && ch > 0 {
        Some((cw, ch))
    } else {
        None
    }
}

fn place_slot(h: HWND, pos: usize, cw: i32, ch: i32) {
    let wa = work_area();
    let cols = ((wa.right - wa.left) / cw).max(1) as usize;
    let rows = ((wa.bottom - wa.top) / ch).max(1) as usize;
    let (x, y) = if pos < cols * rows {
        (wa.left + (pos % cols) as i32 * cw, wa.top + (pos / cols) as i32 * ch)
    } else {
        let k = (pos - cols * rows + 1) as i32;
        (wa.left + 30 * k, wa.top + 30 * k)
    };
    move_visible_to(h, x, y);
}

fn capacity(cw: i32, ch: i32) -> usize {
    let wa = work_area();
    (((wa.right - wa.left) / cw).max(1) * ((wa.bottom - wa.top) / ch).max(1)) as usize
}

fn build_args(jvm: &str, emu: &str, game: &str, wd: i32, ht: i32, id: Option<&str>) -> String {
    let mut s = format!("{}-jar \"{}\" --resizableDevice {} {} --logCallLocation false", jvm, emu, wd, ht);
    if let Some(id) = id {
        s.push_str(&format!(" --id {}", id));
    }
    s.push_str(&format!(" \"{}\"", game));
    s
}

fn file_name(p: &str) -> String {
    Path::new(p).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| p.to_string())
}

fn drain<R: Read + Send + 'static>(mut r: R, log: Arc<Mutex<String>>) {
    thread::spawn(move || {
        let mut buf = [0u8; 2048];
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let s = String::from_utf8_lossy(&buf[..n]).to_string();
                    let mut l = log.lock().unwrap();
                    l.push_str(&s);
                    if l.len() > 4000 {
                        let mut cut = l.len() - 4000;
                        while !l.is_char_boundary(cut) {
                            cut += 1;
                        }
                        l.drain(..cut);
                    }
                }
            }
        }
    });
}

fn start_tab(java: &Path, s: &Settings, game: &str, slot_proxy: &str, ord: usize) -> Result<Tab, String> {
    let id = if s.separate_id {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() % 100_000_000)
            .unwrap_or(0);
        Some(format!("tab{}_{}", ms, ord))
    } else {
        None
    };
    // proxy: dong co proxy rieng thi dung, khong thi dung proxy mac dinh. Tab tro vao relay cuc bo.
    let spec = if slot_proxy.trim().is_empty() { s.proxy.as_str() } else { slot_proxy };
    let up = resolve_proxy(spec).map_err(|e| format!("Proxy không dùng được:\n{}", e))?;
    let (relay, jvm) = match up {
        Some(u) => {
            let r = relay::start(u).map_err(|e| format!("Không mở được relay proxy:\n{}", e))?;
            let a = format!("-DsocksProxyHost=127.0.0.1 -DsocksProxyPort={} ", r.port());
            (Some(r), a)
        }
        None => (None, String::new()),
    };
    let mut cmd = Command::new(java);
    cmd.raw_arg(build_args(&jvm, &s.emulator, game, s.width, s.height, id.as_deref()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    if let Some(dir) = Path::new(&s.emulator).parent() {
        if dir.is_dir() {
            cmd.current_dir(dir);
        }
    }
    let mut child = cmd.spawn().map_err(|e| format!("Không chạy được Java:\n{}", e))?;
    let log = Arc::new(Mutex::new(String::new()));
    if let Some(o) = child.stdout.take() {
        drain(o, log.clone());
    }
    if let Some(e) = child.stderr.take() {
        drain(e, log.clone());
    }
    let pid = child.id();
    let t0 = Instant::now();
    let mut hwnd: HWND = 0;
    let mut exited = false;
    while t0.elapsed() < Duration::from_secs(25) {
        if !matches!(child.try_wait(), Ok(None)) {
            exited = true;
            break;
        }
        hwnd = find_main_window(pid);
        if hwnd != 0 {
            break;
        }
        thread::sleep(Duration::from_millis(150));
    }
    if hwnd == 0 {
        let _ = child.kill();
        let tail = log.lock().unwrap().trim().to_string();
        let tail: String = if tail.chars().count() > 900 {
            tail.chars().skip(tail.chars().count() - 900).collect()
        } else {
            tail
        };
        return Err(format!(
            "{}\n(game: {})\n\nCó thể do một trong những nguyên nhân sau:\n - Máy chưa cài Java hoặc Java quá cũ\n - File giả lập hoặc game không chạy được\n - Tốc độ mở tab quá nhanh{}",
            if exited { "Tab đã thoát ngay sau khi mở." } else { "Không thấy cửa sổ emulator sau 25 giây." },
            file_name(game),
            if tail.is_empty() { String::new() } else { format!("\n\nChi tiết từ Java:\n{}", tail) }
        ));
    }
    // cho kich thuoc cua so on dinh
    let (mut lw, mut lh, mut same) = (-1, -1, 0);
    let t1 = Instant::now();
    while t1.elapsed() < Duration::from_secs(3) && same < 2 {
        match get_rects(hwnd) {
            Some((_, v)) => {
                let (cw, ch) = (v.right - v.left, v.bottom - v.top);
                if cw == lw && ch == lh {
                    same += 1;
                } else {
                    same = 0;
                    lw = cw;
                    lh = ch;
                }
            }
            None => break,
        }
        thread::sleep(Duration::from_millis(150));
    }
    let last_cpu = unsafe { proc_cpu(child.as_raw_handle() as isize) };
    let now = Instant::now();
    Ok(Tab { child, pid, hwnd, hidden: false, sleep: 0, slept_at: now, last_active: now, last_cpu, last_t: now, cpu: 0.0, ws: 0, private: 0, relay })
}

// ------------------------------------------------------------------ ngu / day (sleep)
// "Ngu" = thu nho cua so (emulator build_2 se ngung ve hinh), ha do uu tien CPU xuong thap nhat va thu hoi RAM.
unsafe fn do_sleep(t: &mut Tab, kind: u8) {
    let h = t.child.as_raw_handle() as isize;
    if t.sleep == 0 {
        t.slept_at = Instant::now();
    }
    t.sleep = kind;
    SetPriorityClass(h, 0x40); // IDLE_PRIORITY_CLASS
    K32EmptyWorkingSet(h);
    if !t.hidden && t.hwnd != 0 {
        ShowWindow(t.hwnd, 7); // SW_SHOWMINNOACTIVE
    }
}

unsafe fn do_wake(t: &mut Tab) {
    if t.sleep == 0 {
        return;
    }
    t.sleep = 0;
    t.last_active = Instant::now();
    SetPriorityClass(t.child.as_raw_handle() as isize, 0x20); // NORMAL_PRIORITY_CLASS
    if !t.hidden && t.hwnd != 0 {
        ShowWindow(t.hwnd, SW_SHOWNOACTIVATE);
        if IsIconic(t.hwnd) != 0 {
            ShowWindow(t.hwnd, SW_RESTORE);
        }
    }
}

// Tu ngu bang cach thu nho cua so da bo: emulator co Auto Sleep rieng, phim '\\' gui cho tat ca tab.
unsafe fn auto_tick() {}

// ------------------------------------------------------------------ thong ke CPU / RAM
static SYS_LAST: Mutex<(u64, u64, u64)> = Mutex::new((0, 0, 0));
static LISTCACHE: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());
static SUMMARY_TXT: Mutex<String> = Mutex::new(String::new());

fn ft(f: &FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

unsafe fn proc_cpu(h: isize) -> u64 {
    let (mut c, mut e, mut k, mut u): (FILETIME, FILETIME, FILETIME, FILETIME) = (zeroed(), zeroed(), zeroed(), zeroed());
    if GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u) == 0 {
        return 0;
    }
    ft(&k) + ft(&u)
}

unsafe fn proc_mem(h: isize) -> (u64, u64) {
    let mut m: PROCESS_MEMORY_COUNTERS_EX = zeroed();
    m.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    if K32GetProcessMemoryInfo(h, &mut m as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS, m.cb) == 0 {
        return (0, 0);
    }
    (m.WorkingSetSize as u64, m.PrivateUsage as u64)
}

fn fmt_mem(b: u64) -> String {
    let mb = b as f64 / 1048576.0;
    if mb >= 1024.0 {
        format!("{:.2} GB", mb / 1024.0)
    } else {
        format!("{:.0} MB", mb)
    }
}

fn state_text(t: &Tab) -> &'static str {
    match (t.hidden, t.sleep) {
        (false, 0) => "Đang chạy",
        (true, 0) => "Ẩn",
        (false, 1) => "Ngủ (tự động)",
        (false, _) => "Ngủ",
        (true, _) => "Ẩn + Ngủ",
    }
}

// ------------------------------------------------------------------ ListView
const LVM_GETITEMCOUNT: u32 = 0x1004;
const LVM_DELETEITEM: u32 = 0x1008;
const LVM_GETNEXTITEM: u32 = 0x100C;

const LVM_GETITEMSTATE: u32 = 0x102C;
const LVM_SETEXTENDEDLISTVIEWSTYLE: u32 = 0x1036;
const LVM_INSERTITEMW: u32 = 0x104D;
const LVM_INSERTCOLUMNW: u32 = 0x1061;
const LVM_SETITEMTEXTW: u32 = 0x1074;

const COLS: [(&str, i32); 8] = [("Tab", 34), ("Game", 118), ("Trạng thái", 80), ("CPU", 46), ("RAM", 70), ("Tự ngủ", 54), ("PID", 50), ("Proxy", 150)];

unsafe fn lv_insert(lv: HWND, row: usize, cells: &[String]) {
    let txt = w(&cells[0]);
    let mut it: LVITEMW = zeroed();
    it.mask = 0x1; // LVIF_TEXT
    it.iItem = row as i32;
    it.pszText = txt.as_ptr() as *mut u16;
    SendMessageW(lv, LVM_INSERTITEMW, 0, &it as *const LVITEMW as LPARAM);
    for (c, t) in cells.iter().enumerate().skip(1) {
        lv_put(lv, row, c, t);
    }
    let mut cache = LISTCACHE.lock().unwrap();
    if row <= cache.len() {
        cache.insert(row, cells.to_vec());
    }
}

unsafe fn lv_put(lv: HWND, row: usize, col: usize, text: &str) {
    let txt = w(text);
    let mut it: LVITEMW = zeroed();
    it.iSubItem = col as i32;
    it.pszText = txt.as_ptr() as *mut u16;
    SendMessageW(lv, LVM_SETITEMTEXTW, row, &it as *const LVITEMW as LPARAM);
}

unsafe fn lv_set(lv: HWND, row: usize, col: usize, text: &str) {
    {
        let mut cache = LISTCACHE.lock().unwrap();
        if let Some(r) = cache.get_mut(row) {
            if r[col] == text {
                return;
            }
            r[col] = text.to_string();
        }
    }
    lv_put(lv, row, col, text);
}

unsafe fn lv_checked(lv: HWND, row: usize) -> bool {
    let st = SendMessageW(lv, LVM_GETITEMSTATE, row, 0xF000) as u32;
    (st >> 12) & 0xF == 2
}

unsafe fn lv_count(lv: HWND) -> usize {
    SendMessageW(lv, LVM_GETITEMCOUNT, 0, 0).max(0) as usize
}

// Hien proxy cua dong (khong lo mat khau). "-" = dung proxy mac dinh o o Proxy.
fn proxy_label(spec: &str) -> String {
    let t = spec.trim();
    if t.is_empty() {
        return "-".to_string();
    }
    if let Some(p) = PROXIES.lock().unwrap().iter().find(|p| p.name == t) {
        return format!("{} ({}:{})", p.name, p.host, p.port);
    }
    match relay::parse(t) {
        Ok(Some(u)) => u.describe(),
        _ => "(không tìm thấy)".to_string(),
    }
}

fn slot_cells(i: usize, sl: &Slot) -> Vec<String> {
    vec![
        (i + 1).to_string(),
        file_name(&sl.game),
        match sl.tab.as_ref() {
            Some(t) => state_text(t).to_string(),
            None => "Chưa mở".to_string(),
        },
        "-".to_string(),
        "-".to_string(),
        if sl.auto { "Bật".to_string() } else { "Tắt".to_string() },
        sl.tab.as_ref().map(|t| t.pid.to_string()).unwrap_or_else(|| "-".to_string()),
        proxy_label(&sl.proxy),
    ]
}

unsafe fn update_stats(c: &Ctl) {
    let mut si: SYSTEM_INFO = zeroed();
    GetSystemInfo(&mut si);
    let ncpu = si.dwNumberOfProcessors.max(1) as f64;

    let (mut idle, mut kern, mut user): (FILETIME, FILETIME, FILETIME) = (zeroed(), zeroed(), zeroed());
    let mut sys_cpu = 0.0f64;
    if GetSystemTimes(&mut idle, &mut kern, &mut user) != 0 {
        let (i, k, u) = (ft(&idle), ft(&kern), ft(&user));
        let mut last = SYS_LAST.lock().unwrap();
        let (di, dt) = (i.saturating_sub(last.0), (k + u).saturating_sub(last.1 + last.2));
        if dt > 0 && last.1 + last.2 > 0 {
            sys_cpu = (dt.saturating_sub(di)) as f64 / dt as f64 * 100.0;
        }
        *last = (i, k, u);
    }
    let mut ms: MEMORYSTATUSEX = zeroed();
    ms.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
    GlobalMemoryStatusEx(&mut ms);

    let (mut n_run, mut tc, mut tw, mut tp) = (0usize, 0.0f32, 0u64, 0u64);
    let mut rows: Vec<Vec<String>> = Vec::new();
    {
        let mut g = SLOTS.lock().unwrap();
        let now = Instant::now();
        for (i, sl) in g.iter_mut().enumerate() {
            if let Some(t) = sl.tab.as_mut() {
                if !matches!(t.child.try_wait(), Ok(None)) {
                    sl.tab = None;
                }
            }
            let mut cells = slot_cells(i, sl);
            if let Some(t) = sl.tab.as_mut() {
                let h = t.child.as_raw_handle() as isize;
                let cpu_now = proc_cpu(h);
                let dt = now.duration_since(t.last_t).as_nanos() as f64 / 100.0;
                if dt > 0.0 {
                    t.cpu = ((cpu_now.saturating_sub(t.last_cpu)) as f64 / dt / ncpu * 100.0) as f32;
                }
                t.last_cpu = cpu_now;
                t.last_t = now;
                let (ws, pv) = proc_mem(h);
                t.ws = ws;
                t.private = pv;
                cells[3] = format!("{:.1}%", t.cpu);
                cells[4] = fmt_mem(ws);
                n_run += 1;
                tc += t.cpu;
                tw += ws;
                tp += pv;
            }
            rows.push(cells);
        }
    }
    for (i, cells) in rows.iter().enumerate() {
        for (col, t) in cells.iter().enumerate().skip(1) {
            lv_set(c.lv, i, col, t);
        }
    }
    let txt = format!(
        "Hệ thống: CPU {:.1}%  |  RAM {} / {} ({}%)\r\nTất cả {} tab đang chạy: CPU {:.1}%  |  RAM {} (riêng {})",
        sys_cpu,
        fmt_mem(ms.ullTotalPhys.saturating_sub(ms.ullAvailPhys)),
        fmt_mem(ms.ullTotalPhys),
        ms.dwMemoryLoad,
        n_run,
        tc,
        fmt_mem(tw),
        fmt_mem(tp)
    );
    let mut last = SUMMARY_TXT.lock().unwrap();
    if *last != txt {
        set_text(c.summary, &txt);
        *last = txt;
    }
}

// ------------------------------------------------------------------ gui phim dong bo

// Gui 1 phim lan luot toi tung tab dang thuc va hien: dua tab len truoc -> cho -> SendInput -> cho xu ly -> tab ke tiep.
// Nhat ky chan doan: ghi canh file exe (remoter_log.txt), de biet tab nao nhan / bi bo qua va ly do.
fn log_line(msg: &str) {
    use std::io::Write;
    let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_else(|| PathBuf::from("."));
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("remoter_log.txt")) {
        let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let _ = writeln!(f, "[{}] {}", ms, msg);
    }
}








unsafe fn set_icons(h: HWND) {
    let hi = GetModuleHandleW(null());
    for (kind, cx, cy) in [
        (0usize, GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON)),
        (1usize, GetSystemMetrics(SM_CXICON), GetSystemMetrics(SM_CYICON)),
    ] {
        let ic = LoadImageW(hi, 1 as *const u16, IMAGE_ICON, cx, cy, LR_DEFAULTCOLOR);
        if ic != 0 {
            SendMessageW(h, WM_SETICON, kind, ic);
        }
    }
}

// ------------------------------------------------------------------ giao dien
const WM_APP_MSGBOX: u32 = WM_APP + 1;
const WM_APP_DONE: u32 = WM_APP + 2;

const ID_BROWSE_EMU: isize = 101;
const ID_BROWSE_GAME: isize = 102;
const ID_BROWSE_JAVA: isize = 103;
const ID_START: isize = 110;
const ID_ARRANGE: isize = 111;
const ID_CLOSEALL: isize = 114;
const ID_ADDGAME: isize = 120;
const ID_ADDN: isize = 121;
const ID_DELROW: isize = 122;
const ID_HIDE: isize = 130;
const ID_SHOW: isize = 131;
const ID_CLOSETABS: isize = 136;
const ID_SETPROXY: isize = 140;
const ID_PROXYMGR: isize = 141;
const ID_PROXYCOMBO: isize = 142;

struct Ctl {
    main: HWND,
    emu: HWND,
    game: HWND,
    java: HWND,
    proxy: HWND,
    wd: HWND,
    ht: HWND,
    tabs: HWND,
    wait: HWND,
    sleep: HWND,
    chk: HWND,
    lv: HWND,
    summary: HWND,
    status: HWND,
    btn_all_running: Vec<HWND>, // can tab dang chay
    btn_all_rows: Vec<HWND>,    // can co dong
    start: HWND,
}

static CTL: OnceLock<Ctl> = OnceLock::new();

unsafe fn child(parent: HWND, class: &str, text: &str, style: u32, ex: u32, x: i32, y: i32, wd: i32, ht: i32, id: isize) -> HWND {
    let h = CreateWindowExW(
        ex,
        w(class).as_ptr(),
        w(text).as_ptr(),
        WS_CHILD | WS_VISIBLE | style,
        x,
        y,
        wd,
        ht,
        parent,
        id,
        GetModuleHandleW(null()),
        null(),
    );
    SendMessageW(h, WM_SETFONT, FONT.load(Ordering::Relaxed) as WPARAM, 1);
    h
}

unsafe fn label(p: HWND, t: &str, x: i32, y: i32, wd: i32) -> HWND {
    child(p, "STATIC", t, 0, 0, x, y, wd, 18, 0)
}
unsafe fn edit(p: HWND, t: &str, x: i32, y: i32, wd: i32) -> HWND {
    child(p, "EDIT", t, WS_TABSTOP | ES_AUTOHSCROLL as u32, WS_EX_CLIENTEDGE, x, y, wd, 22, 0)
}
unsafe fn button(p: HWND, t: &str, x: i32, y: i32, wd: i32, id: isize) -> HWND {
    child(p, "BUTTON", t, WS_TABSTOP, 0, x, y, wd, 28, id)
}

unsafe fn create_controls(main: HWND) {
    let s = CFG.lock().unwrap().clone();
    label(main, "Emulator:", 14, 18, 66);
    let emu = edit(main, &s.emulator, 84, 14, 468);
    button(main, "Duyệt", 560, 11, 66, ID_BROWSE_EMU);
    label(main, "Game:", 14, 50, 66);
    let game = edit(main, &s.game, 84, 46, 468);
    button(main, "Duyệt", 560, 43, 66, ID_BROWSE_GAME);
    label(main, "Java:", 14, 82, 66);
    let java_txt = if s.java.is_empty() { find_java("").map(|p| p.to_string_lossy().to_string()).unwrap_or_default() } else { s.java.clone() };
    let java = edit(main, &java_txt, 84, 78, 468);
    button(main, "Duyệt", 560, 75, 66, ID_BROWSE_JAVA);
    label(main, "Proxy:", 14, 114, 66);
    let proxy = child(main, "COMBOBOX", "", WS_TABSTOP | WS_VSCROLL | 0x3 /* CBS_DROPDOWNLIST */, 0, 84, 110, 380, 220, ID_PROXYCOMBO);
    fill_proxy_combo(proxy, &s.proxy);
    button(main, "Quản lý…", 472, 107, 80, ID_PROXYMGR);
    button(main, "Gán dòng", 560, 107, 66, ID_SETPROXY);

    label(main, "Kích thước tab:", 14, 150, 94);
    let wd = edit(main, &s.width.to_string(), 110, 146, 46);
    label(main, "X", 164, 150, 14);
    let ht = edit(main, &s.height.to_string(), 182, 146, 46);
    label(main, "Số lượng tab:", 250, 150, 84);
    let tabs = edit(main, &s.tabs.to_string(), 338, 146, 40);
    label(main, "Chờ tab (ms):", 400, 150, 84);
    let wait = edit(main, &s.wait.to_string(), 488, 146, 56);

    let chk = child(main, "BUTTON", "Tách dữ liệu từng tab", WS_TABSTOP | 3 /* BS_AUTOCHECKBOX */, 0, 14, 180, 160, 22, 0);
    if s.separate_id {
        SendMessageW(chk, 0x00F1 /* BM_SETCHECK */, 1, 0);
    }
    label(main, "Tự ngủ khi không thao tác sau (giây):", 200, 184, 230);
    let sleep = edit(main, &s.sleep_after.to_string(), 434, 180, 46);

    let bw = 84;
    let bx = |i: i32| 14 + i * 88;
    let b_addgame = button(main, "Thêm game…", bx(0), 216, bw, ID_ADDGAME);
    let b_addn = button(main, "Thêm N tab", bx(1), 216, bw, ID_ADDN);
    let b_del = button(main, "Xóa dòng", bx(2), 216, bw, ID_DELROW);
    let start = button(main, "Bắt đầu", bx(3), 216, bw, ID_START);
    let b_arr = button(main, "Sắp xếp tab", bx(4), 216, bw, ID_ARRANGE);
    let b_closeall = button(main, "Đóng hết", bx(5), 216, bw, ID_CLOSEALL);

    // danh sach tab
    let lv = child(main, "SysListView32", "", WS_TABSTOP | 0x1 /* LVS_REPORT */ | 0x4 /* LVS_SINGLESEL */ | 0x8 /* LVS_SHOWSELALWAYS */, WS_EX_CLIENTEDGE, 14, 254, 612, 196, 0);
    SendMessageW(lv, LVM_SETEXTENDEDLISTVIEWSTYLE, 0, 0x4 | 0x20 | 0x1); // CHECKBOXES | FULLROWSELECT | GRIDLINES
    for (i, (name, width)) in COLS.iter().enumerate() {
        let t = w(name);
        let mut col: LVCOLUMNW = zeroed();
        col.mask = 0x1 | 0x2 | 0x4 | 0x8; // FMT | WIDTH | TEXT | SUBITEM
        col.cx = *width;
        col.pszText = t.as_ptr() as *mut u16;
        col.iSubItem = i as i32;
        SendMessageW(lv, LVM_INSERTCOLUMNW, i, &col as *const LVCOLUMNW as LPARAM);
    }
    {
        let g = SLOTS.lock().unwrap();
        for (i, sl) in g.iter().enumerate() {
            lv_insert(lv, i, &slot_cells(i, sl));
        }
    }

    label(main, "Áp dụng cho các dòng đã tick (không tick dòng nào = tất cả):", 14, 458, 450);
    let b_hide = button(main, "Ẩn", bx(0), 478, bw, ID_HIDE);
    let b_show = button(main, "Hiện", bx(1), 478, bw, ID_SHOW);
    let b_close = button(main, "Đóng tab", bx(2), 478, bw, ID_CLOSETABS);

    let summary = child(main, "STATIC", "", 0x1000 /* SS_SUNKEN */, 0, 14, 518, 612, 38, 0);
    let status = child(main, "STATIC", " Sẵn sàng", 0x1000 | 0x0200 /* SS_SUNKEN|SS_CENTERIMAGE */, 0, 0, 566, 640, 22, 0);

    let _ = CTL.set(Ctl {
        main,
        emu,
        game,
        java,
        proxy,
        wd,
        ht,
        tabs,
        wait,
        sleep,
        chk,
        lv,
        summary,
        status,
        btn_all_running: vec![b_arr, b_closeall, b_hide, b_show, b_close],
        btn_all_rows: vec![b_del],
        start,
    });
    let _ = (b_addgame, b_addn);
}

unsafe fn refresh_buttons() {
    if let Some(c) = CTL.get() {
        let n = snapshot().len();
        let rows = lv_count(c.lv);
        for &h in &c.btn_all_running {
            EnableWindow(h, (n > 0) as BOOL);
        }
        for &h in &c.btn_all_rows {
            EnableWindow(h, (rows > 0) as BOOL);
        }
        update_stats(c);
        let launching = LAUNCHING.load(Ordering::Relaxed);
        EnableWindow(c.start, (!launching) as BOOL);
        if !launching {
            set_text(c.status, &if n > 0 { format!(" Đang chạy: {} / {} tab", n, rows) } else { " Sẵn sàng".to_string() });
        }
    }
}

// Chon file: 1 file hoac nhieu file (moi file = 1 tab).
unsafe fn pick_files(owner: HWND, filter: &str, multi: bool, current: &str) -> Vec<String> {
    let mut buf = vec![0u16; 32768];
    let init: Vec<u16> = Path::new(current)
        .parent()
        .map(|p| w(&p.to_string_lossy()))
        .unwrap_or_else(|| vec![0]);
    let f: Vec<u16> = filter.encode_utf16().chain([0, 0]).collect();
    let mut ofn: OPENFILENAMEW = zeroed();
    ofn.lStructSize = size_of::<OPENFILENAMEW>() as u32;
    ofn.hwndOwner = owner;
    ofn.lpstrFilter = f.as_ptr();
    ofn.nFilterIndex = 1;
    ofn.lpstrFile = buf.as_mut_ptr();
    ofn.nMaxFile = buf.len() as u32;
    ofn.lpstrInitialDir = init.as_ptr();
    ofn.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR | OFN_EXPLORER | if multi { OFN_ALLOWMULTISELECT } else { 0 };
    if GetOpenFileNameW(&mut ofn) == 0 {
        return Vec::new();
    }
    let mut parts: Vec<String> = Vec::new();
    let mut start = 0;
    for i in 0..buf.len() {
        if buf[i] == 0 {
            if i == start {
                break;
            }
            parts.push(String::from_utf16_lossy(&buf[start..i]));
            start = i + 1;
        }
    }
    if parts.len() <= 1 {
        parts
    } else {
        let dir = parts[0].clone();
        parts[1..].iter().map(|f| Path::new(&dir).join(f).to_string_lossy().to_string()).collect()
    }
}

unsafe fn browse(owner: HWND, target: HWND, filter: &str) {
    let cur = get_text(target);
    if let Some(p) = pick_files(owner, filter, false, &cur).into_iter().next() {
        set_text(target, &p);
    }
}

const GAME_FILTER: &str = "Game (*.jad;*.jar)\0*.jad;*.jar\0Tất cả (*.*)\0*.*\0";

fn is_game_file(p: &str) -> bool {
    let g = p.to_lowercase();
    (g.ends_with(".jad") || g.ends_with(".jar")) && Path::new(p).is_file()
}

unsafe fn read_settings(c: &Ctl) -> Result<Settings, String> {
    let mut s = CFG.lock().unwrap().clone();
    let clean = |t: String| t.trim().trim_matches('"').to_string();
    s.emulator = clean(get_text(c.emu));
    s.game = clean(get_text(c.game));
    s.java = clean(get_text(c.java));
    s.proxy = combo_selected_name(c.proxy);
    s.separate_id = SendMessageW(c.chk, 0x00F0 /* BM_GETCHECK */, 0, 0) == 1;
    if !s.emulator.to_lowercase().ends_with(".jar") || !Path::new(&s.emulator).is_file() {
        return Err("Đường dẫn file Emulator không đúng!".into());
    }
    let (wd, ht) = (get_text(c.wd).trim().parse::<i32>(), get_text(c.ht).trim().parse::<i32>());
    match (wd, ht) {
        (Ok(a), Ok(b)) if (50..=4000).contains(&a) && (50..=4000).contains(&b) => {
            s.width = a;
            s.height = b;
        }
        _ => return Err("Kích thước tab không đúng!".into()),
    }
    match get_text(c.tabs).trim().parse::<i32>() {
        Ok(n) if (1..=200).contains(&n) => s.tabs = n,
        _ => return Err("Số lượng tab không đúng!".into()),
    }
    match get_text(c.wait).trim().parse::<u64>() {
        Ok(n) => s.wait = n,
        _ => return Err("Thời gian chờ tab không đúng!".into()),
    }
    match get_text(c.sleep).trim().parse::<u64>() {
        Ok(n) if n >= 5 => s.sleep_after = n,
        _ => return Err("Thời gian tự ngủ phải từ 5 giây trở lên!".into()),
    }
    Ok(s)
}

fn persist() {
    let s = CFG.lock().unwrap().clone();
    s.save();
}

unsafe fn ui_msg(main: HWND, text: String, title: &str, icon: u32) {
    let b = Box::new((text, title.to_string(), icon));
    SendMessageW(main, WM_APP_MSGBOX, 0, Box::into_raw(b) as LPARAM);
}

unsafe fn add_rows(c: &Ctl, games: Vec<String>) {
    for g in games {
        let (i, cells) = {
            let mut sl = SLOTS.lock().unwrap();
            sl.push(Slot { game: g, auto: false, proxy: String::new(), tab: None });
            let i = sl.len() - 1;
            (i, slot_cells(i, &sl[i]))
        };
        lv_insert(c.lv, i, &cells);
    }
    persist();
}

// cac dong duoc tick; khong tick dong nao = tat ca
unsafe fn targets(c: &Ctl, running_only: bool) -> Vec<usize> {
    let _ = snapshot(); // don dep tab da thoat
    let n = lv_count(c.lv);
    let checked: Vec<usize> = (0..n).filter(|&i| lv_checked(c.lv, i)).collect();
    let base: Vec<usize> = if checked.is_empty() { (0..n).collect() } else { checked };
    if !running_only {
        return base;
    }
    let g = SLOTS.lock().unwrap();
    base.into_iter().filter(|&i| g.get(i).map(|s| s.tab.is_some()).unwrap_or(false)).collect()
}

unsafe fn on_start(c: &Ctl) {
    if LAUNCHING.load(Ordering::Relaxed) {
        return;
    }
    let s = match read_settings(c) {
        Ok(s) => s,
        Err(e) => {
            msgbox(c.main, &e, "Lỗi", MB_OK | MB_ICONWARNING);
            return;
        }
    };
    *CFG.lock().unwrap() = s.clone();
    // chua co dong nao -> tao tu o Game x So luong tab
    if SLOTS.lock().unwrap().is_empty() {
        if !is_game_file(&s.game) {
            msgbox(c.main, "Đường dẫn file Game không đúng!\nHãy chọn game ở ô Game hoặc bấm \"Thêm game…\".", "Lỗi", MB_OK | MB_ICONWARNING);
            return;
        }
        add_rows(c, vec![s.game.clone(); s.tabs as usize]);
    }
    persist();
    // kiem tra game cua cac dong se mo
    let bad: Option<String> = {
        let g = SLOTS.lock().unwrap();
        g.iter()
            .enumerate()
            .find(|(_, sl)| sl.tab.is_none() && !is_game_file(&sl.game))
            .map(|(i, sl)| format!("Dòng {}: không tìm thấy file game:\n{}", i + 1, sl.game))
    };
    if let Some(m) = bad {
        msgbox(c.main, &m, "Lỗi", MB_OK | MB_ICONWARNING);
        return;
    }
    let java = match find_java(&s.java) {
        Some(p) => p,
        None => {
            msgbox(
                c.main,
                "Không tìm thấy javaw.exe.\nHãy cài Java (JRE 8 trở lên) hoặc bấm Duyệt ở ô Java để chọn file javaw.exe.",
                "Thiếu Java",
                MB_OK | MB_ICONWARNING,
            );
            return;
        }
    };
    set_text(c.java, &java.to_string_lossy());
    LAUNCHING.store(true, Ordering::Relaxed);
    EnableWindow(c.start, 0);
    let main = c.main;
    thread::spawn(move || launch_worker(main, s, java));
}

fn launch_worker(main: HWND, s: Settings, java: PathBuf) {
    unsafe {
        let status = CTL.get().map(|c| c.status).unwrap_or(0);
        let todo: Vec<usize> = SLOTS.lock().unwrap().iter().enumerate().filter(|(_, sl)| sl.tab.is_none()).map(|(i, _)| i).collect();
        for (n, &slot) in todo.iter().enumerate() {
            let snaps = snapshot();
            let running = snaps.len();
            if running >= 1 {
                if let Some((cw, ch)) = get_cell(&snaps) {
                    if running >= capacity(cw, ch) {
                        ui_msg(main, "Màn hình đã kín. Nếu muốn mở thêm tab hãy mở thêm tool này!".into(), "Toonie Remoter", MB_OK | MB_ICONINFORMATION);
                        break;
                    }
                }
            }
            let (game, px) = match SLOTS.lock().unwrap().get(slot) {
                Some(sl) if sl.tab.is_none() => (sl.game.clone(), sl.proxy.clone()),
                _ => continue,
            };
            set_text(status, &format!(" Đang mở dòng {} ({}) ...", slot + 1, file_name(&game)));
            match start_tab(&java, &s, &game, &px, slot) {
                Ok(tab) => {
                    let h = tab.hwnd;
                    if let Some(sl) = SLOTS.lock().unwrap().get_mut(slot) {
                        sl.tab = Some(tab);
                    }
                    let snaps = snapshot();
                    if let Some((cw, ch)) = get_cell(&snaps) {
                        place_slot(h, running, cw, ch);
                    }
                }
                Err(e) => {
                    ui_msg(main, e, "Lỗi mở tab!", MB_OK | MB_ICONERROR);
                    break;
                }
            }
            if n + 1 < todo.len() && s.wait > 0 {
                thread::sleep(Duration::from_millis(s.wait));
            }
        }
        PostMessageW(main, WM_APP_DONE, 0, 0);
    }
}

unsafe fn arrange(c: &Ctl) {
    let snaps = snapshot();
    if let Some((cw, ch)) = get_cell(&snaps) {
        for (i, s) in snaps.iter().enumerate() {
            if s.hwnd != 0 {
                place_slot(s.hwnd, i, cw, ch);
            }
        }
        let cap = capacity(cw, ch);
        if snaps.len() > cap {
            msgbox(
                c.main,
                &format!("Màn hình chỉ chứa {} tab, {} tab còn lại được xếp chồng.", cap, snaps.len() - cap),
                "Sắp xếp tab",
                MB_OK | MB_ICONINFORMATION,
            );
        }
    }
}

unsafe fn act_hide(c: &Ctl, hide: bool) {
    let t = targets(c, true);
    let mut g = SLOTS.lock().unwrap();
    for i in t {
        if let Some(tb) = g.get_mut(i).and_then(|s| s.tab.as_mut()) {
            if tb.hwnd == 0 || IsWindow(tb.hwnd) == 0 {
                tb.hwnd = find_main_window(tb.pid);
            }
            if tb.hwnd != 0 {
                if hide {
                    ShowWindow(tb.hwnd, SW_HIDE);
                } else {
                    ShowWindow(tb.hwnd, if tb.sleep > 0 { 7 } else { SW_SHOWNOACTIVATE });
                }
            }
            tb.hidden = hide;
        }
    }
}

// Phim '\\' (nut "Auto Sleep"): gui phim '\\' toi TAT CA tab nhu mot phim binh thuong.
// Moi emulator tu xu ly '\\' (Auto Sleep cua no: ngu tai cho, cua so van hien, khong thu nho).
// EMU_SLEEP nho rang cac tab vua duoc cho ngu: phim tiep theo chi danh thuc chung (phim Shift, emulator bo qua).

unsafe fn close_slots(c: &Ctl, idx: &[usize]) {
    let mut handles: Vec<(usize, u32, HWND, bool)> = Vec::new();
    {
        let g = SLOTS.lock().unwrap();
        for &i in idx {
            if let Some(t) = g.get(i).and_then(|s| s.tab.as_ref()) {
                handles.push((i, t.pid, t.hwnd, t.hidden || t.sleep != 0));
            }
        }
    }
    for &(_, _, h, hidden) in &handles {
        if h != 0 {
            if hidden {
                ShowWindow(h, SW_SHOWNOACTIVATE);
            }
            PostMessageW(h, WM_CLOSE, 0, 0);
        }
    }
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(2500) {
        let running = snapshot();
        if !handles.iter().any(|(i, _, _, _)| running.iter().any(|s| s.slot == *i)) {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    {
        let mut g = SLOTS.lock().unwrap();
        for &(i, _, _, _) in &handles {
            if let Some(sl) = g.get_mut(i) {
                if let Some(t) = sl.tab.as_mut() {
                    let _ = t.child.kill();
                }
                sl.tab = None;
            }
        }
    }
    refresh_buttons();
    let _ = c;
}

unsafe fn act_close_tabs(c: &Ctl, ask_all: bool) {
    let t = if ask_all { targets_all_running() } else { targets(c, true) };
    if t.is_empty() {
        return;
    }
    let q = if ask_all { "Bạn có chắc muốn đóng tất cả Tab?".to_string() } else { format!("Đóng {} tab đã chọn?", t.len()) };
    if msgbox(c.main, &q, "Xác nhận đóng tab", MB_YESNO | MB_ICONQUESTION) != IDYES {
        return;
    }
    close_slots(c, &t);
}

fn targets_all_running() -> Vec<usize> {
    snapshot().iter().map(|s| s.slot).collect()
}

unsafe fn act_remove_rows(c: &Ctl) {
    if LAUNCHING.load(Ordering::Relaxed) {
        return;
    }
    let n = lv_count(c.lv);
    let checked: Vec<usize> = (0..n).filter(|&i| lv_checked(c.lv, i)).collect();
    if checked.is_empty() {
        msgbox(c.main, "Hãy tick các dòng cần xóa (chỉ xóa được dòng chưa mở).", "Xóa dòng", MB_OK | MB_ICONINFORMATION);
        return;
    }
    let _ = snapshot();
    for &i in checked.iter().rev() {
        let removable = SLOTS.lock().unwrap().get(i).map(|s| s.tab.is_none()).unwrap_or(false);
        if removable {
            SLOTS.lock().unwrap().remove(i);
            SendMessageW(c.lv, LVM_DELETEITEM, i, 0);
            let mut cache = LISTCACHE.lock().unwrap();
            if i < cache.len() {
                cache.remove(i);
            }
        }
    }
    // danh so lai cot "Tab"
    {
        let g = SLOTS.lock().unwrap();
        for i in 0..g.len() {
            lv_set(c.lv, i, 0, &(i + 1).to_string());
        }
    }
    persist();
    refresh_buttons();
}

// Gan proxy o o "Proxy" cho cac dong da tick (khong tick dong nao = tat ca). O trong = xoa proxy rieng.
unsafe fn act_set_proxy(c: &Ctl) {
    let spec = combo_selected_name(c.proxy);
    let idx = targets(c, false);
    let mut running = 0;
    {
        let mut g = SLOTS.lock().unwrap();
        for &i in &idx {
            if let Some(sl) = g.get_mut(i) {
                sl.proxy = spec.clone();
                if sl.tab.is_some() {
                    running += 1;
                }
            }
        }
    }
    for &i in &idx {
        lv_set(c.lv, i, 7, &proxy_label(&spec));
    }
    persist();
    if running > 0 {
        msgbox(c.main, &format!("{} tab đang chạy sẽ dùng proxy mới sau khi đóng và mở lại.", running), "Proxy", MB_OK | MB_ICONINFORMATION);
    }
}

unsafe fn change_game(c: &Ctl, row: usize) {
    let cur = SLOTS.lock().unwrap().get(row).map(|s| (s.game.clone(), s.tab.is_some()));
    if let Some((game, running)) = cur {
        if running {
            return;
        }
        if let Some(p) = pick_files(c.main, GAME_FILTER, false, &game).into_iter().next() {
            if let Some(sl) = SLOTS.lock().unwrap().get_mut(row) {
                sl.game = p.clone();
            }
            lv_set(c.lv, row, 1, &file_name(&p));
            persist();
        }
    }
}



// ------------------------------------------------------------------ o chon proxy (cua so chinh)
unsafe fn fill_proxy_combo(cb: HWND, sel: &str) {
    SendMessageW(cb, 0x014B /* CB_RESETCONTENT */, 0, 0);
    SendMessageW(cb, 0x0143 /* CB_ADDSTRING */, 0, w("(không dùng proxy)").as_ptr() as LPARAM);
    let mut idx = 0usize;
    let items: Vec<(String, bool)> = PROXIES
        .lock()
        .unwrap()
        .iter()
        .map(|p| (format!("{}   [{} {}:{}]", p.name, if p.http { "http" } else { "socks5" }, p.host, p.port), p.name == sel))
        .collect();
    for (i, (t, is_sel)) in items.iter().enumerate() {
        SendMessageW(cb, 0x0143, 0, w(t).as_ptr() as LPARAM);
        if *is_sel {
            idx = i + 1;
        }
    }
    SendMessageW(cb, 0x014E /* CB_SETCURSEL */, idx, 0);
}

// ten proxy dang chon trong o chon; rong = "(khong dung proxy)"
unsafe fn combo_selected_name(cb: HWND) -> String {
    let i = SendMessageW(cb, 0x0147 /* CB_GETCURSEL */, 0, 0);
    if i <= 0 {
        return String::new();
    }
    PROXIES.lock().unwrap().get(i as usize - 1).map(|p| p.name.clone()).unwrap_or_default()
}

// ------------------------------------------------------------------ cua so quan ly proxy
const ID_PM_LIST: isize = 201;
const ID_PM_NAME: isize = 202;
const ID_PM_TYPE: isize = 203;
const ID_PM_HOST: isize = 204;
const ID_PM_PORT: isize = 205;
const ID_PM_USER: isize = 206;
const ID_PM_PASS: isize = 207;
const ID_PM_INFO: isize = 208;
const ID_PM_ADD: isize = 210;
const ID_PM_UPD: isize = 211;
const ID_PM_DEL: isize = 212;
const ID_PM_TEST: isize = 213;
const ID_PM_TESTALL: isize = 214;
const ID_PM_CLOSE: isize = 215;
const WM_APP_PXRESULT: u32 = WM_APP + 3;
const TEST_HOST: &str = "example.com"; // dich de thu: mo ket noi toi day qua proxy
const TEST_PORT: u16 = 80;
const PM_COLS: [(&str, i32); 7] = [("Tên", 90), ("Loại", 56), ("IP / Host", 118), ("Port", 46), ("User", 80), ("Pass", 48), ("Kết quả", 170)];

static PMGR: AtomicIsize = AtomicIsize::new(0);

unsafe fn pm_item(h: HWND, id: isize) -> HWND {
    GetDlgItem(h, id as i32)
}

unsafe fn pm_text(h: HWND, id: isize) -> String {
    get_text(pm_item(h, id)).trim().to_string()
}

// them 1 dong vao ListView khong dung LISTCACHE (cache chi danh cho danh sach tab)
unsafe fn lv_add(lv: HWND, row: usize, cells: &[String]) {
    let txt = w(&cells[0]);
    let mut it: LVITEMW = zeroed();
    it.mask = 0x1; // LVIF_TEXT
    it.iItem = row as i32;
    it.pszText = txt.as_ptr() as *mut u16;
    SendMessageW(lv, LVM_INSERTITEMW, 0, &it as *const LVITEMW as LPARAM);
    for (c, t) in cells.iter().enumerate().skip(1) {
        lv_put(lv, row, c, t);
    }
}

unsafe fn pm_select(list: HWND, row: usize) {
    let mut it: LVITEMW = zeroed();
    it.stateMask = 0x3; // LVIS_FOCUSED | LVIS_SELECTED
    it.state = 0x3;
    SendMessageW(list, 0x102B /* LVM_SETITEMSTATE */, row, &it as *const LVITEMW as LPARAM);
}

unsafe fn pm_selected(h: HWND) -> Option<usize> {
    let sel = SendMessageW(pm_item(h, ID_PM_LIST), LVM_GETNEXTITEM, usize::MAX, 0x2 /* LVNI_SELECTED */);
    if sel >= 0 {
        Some(sel as usize)
    } else {
        None
    }
}

fn pm_info_text(name: &str, result: &str) -> String {
    format!("{}  ->  {}", name, if result.is_empty() { "Chưa kiểm tra" } else { result })
}

// doc cac o nhap thanh 1 proxy; o IP cho phep dan ca chuoi (host:port, host:port:user:pass, socks5://user:pass@host:port ...)
unsafe fn pm_read(h: HWND) -> Result<ProxyItem, String> {
    let mut name = pm_text(h, ID_PM_NAME);
    let mut host = pm_text(h, ID_PM_HOST);
    let mut port_s = pm_text(h, ID_PM_PORT);
    let mut user = pm_text(h, ID_PM_USER);
    let mut pass = pm_text(h, ID_PM_PASS);
    let mut http = SendMessageW(pm_item(h, ID_PM_TYPE), 0x0147 /* CB_GETCURSEL */, 0, 0) == 1;
    if host.contains(':') || host.contains('@') {
        match relay::parse(&host)? {
            Some(u) => {
                if host.contains("://") {
                    http = u.http;
                }
                host = u.host;
                port_s = u.port.to_string();
                if !u.user.is_empty() {
                    user = u.user;
                    pass = u.pass;
                }
            }
            None => {}
        }
    }
    if host.is_empty() {
        return Err("Chưa nhập IP / host".into());
    }
    let port: u16 = match port_s.parse() {
        Ok(p) if p > 0 => p,
        _ => return Err("Port không đúng (1 - 65535)".into()),
    };
    if name.is_empty() {
        name = format!("{}:{}", host, port);
    }
    let ctl = |t: &str| t.chars().any(|c| c.is_control());
    if ctl(&name) || ctl(&host) || ctl(&user) || ctl(&pass) || host.chars().any(|c| c.is_whitespace()) {
        return Err("Không được dùng ký tự điều khiển, và IP không được có khoảng trắng".into());
    }
    if name.contains('|') {
        return Err("Tên không được chứa dấu |".into());
    }
    if user.len() > 255 || pass.len() > 255 {
        return Err("User / Pass tối đa 255 ký tự".into());
    }
    Ok(ProxyItem { name, http, host, port, user, pass, result: String::new() })
}

unsafe fn pm_fill_form(h: HWND) {
    let sel = match pm_selected(h) {
        Some(i) => i,
        None => return,
    };
    let p = match PROXIES.lock().unwrap().get(sel) {
        Some(p) => p.clone(),
        None => return,
    };
    set_text(pm_item(h, ID_PM_NAME), &p.name);
    SendMessageW(pm_item(h, ID_PM_TYPE), 0x014E /* CB_SETCURSEL */, p.http as usize, 0);
    set_text(pm_item(h, ID_PM_HOST), &p.host);
    set_text(pm_item(h, ID_PM_PORT), &p.port.to_string());
    set_text(pm_item(h, ID_PM_USER), &p.user);
    set_text(pm_item(h, ID_PM_PASS), &p.pass);
    set_text(pm_item(h, ID_PM_INFO), &pm_info_text(&p.name, &p.result));
}

// sau khi them/sua/xoa: luu file va nap lai o chon proxy o cua so chinh
unsafe fn pm_after_change() {
    if let Some(c) = CTL.get() {
        let keep = CFG.lock().unwrap().proxy.clone();
        fill_proxy_combo(c.proxy, &keep);
        CFG.lock().unwrap().proxy = combo_selected_name(c.proxy); // proxy mac dinh bi xoa -> ve "(khong dung proxy)"
    }
    persist();
}

unsafe fn pm_apply_result(h: HWND, name: &str, res: &str) {
    let idx = {
        let mut g = PROXIES.lock().unwrap();
        let pos = g.iter().position(|p| p.name == name);
        if let Some(i) = pos {
            g[i].result = res.to_string();
        }
        pos
    };
    if let Some(i) = idx {
        lv_put(pm_item(h, ID_PM_LIST), i, 6, res);
        if pm_selected(h) == Some(i) {
            set_text(pm_item(h, ID_PM_INFO), &pm_info_text(name, res));
        }
    }
}

unsafe fn pm_start_test(h: HWND, names: Vec<String>) {
    for name in names {
        let up = PROXIES.lock().unwrap().iter().find(|p| p.name == name).map(|p| p.upstream());
        if let Some(up) = up {
            pm_apply_result(h, &name, "Đang thử...");
            thread::spawn(move || {
                let r = match relay::test(&up, TEST_HOST, TEST_PORT) {
                    Ok(ms) => format!("OK - {} ms", ms),
                    Err(e) => format!("Lỗi: {}", e),
                };
                if let Some(p) = PROXIES.lock().unwrap().iter_mut().find(|p| p.name == name) {
                    p.result = r.clone();
                }
                let hm = PMGR.load(Ordering::Relaxed);
                if hm != 0 {
                    let b = Box::into_raw(Box::new((name, r)));
                    if unsafe { PostMessageW(hm, WM_APP_PXRESULT, 0, b as LPARAM) } == 0 {
                        drop(unsafe { Box::from_raw(b) });
                    }
                }
            });
        }
    }
}

unsafe fn pm_command(h: HWND, id: isize) {
    let list = pm_item(h, ID_PM_LIST);
    let warn = |t: &str| {
        msgbox(h, t, "Proxy", MB_OK | MB_ICONWARNING);
    };
    match id {
        ID_PM_ADD => match pm_read(h) {
            Err(e) => {
                warn(&e);
            }
            Ok(p) => {
                if PROXIES.lock().unwrap().iter().any(|x| x.name == p.name) {
                    warn("Đã có proxy trùng tên. Hãy đổi tên khác (hoặc chọn dòng đó rồi bấm Sửa).");
                    return;
                }
                let n = {
                    let mut g = PROXIES.lock().unwrap();
                    g.push(p.clone());
                    g.len() - 1
                };
                lv_add(list, n, &p.cells());
                pm_after_change();
                pm_select(list, n);
            }
        },
        ID_PM_UPD => {
            let sel = match pm_selected(h) {
                Some(i) => i,
                None => {
                    warn("Hãy chọn một proxy trong danh sách để sửa.");
                    return;
                }
            };
            match pm_read(h) {
                Err(e) => {
                    warn(&e);
                }
                Ok(p) => {
                    let (dup, old) = {
                        let g = PROXIES.lock().unwrap();
                        (g.iter().enumerate().any(|(i, x)| i != sel && x.name == p.name), g.get(sel).map(|x| x.name.clone()).unwrap_or_default())
                    };
                    if dup {
                        warn("Đã có proxy khác trùng tên này.");
                        return;
                    }
                    if let Some(slot) = PROXIES.lock().unwrap().get_mut(sel) {
                        *slot = p.clone();
                    }
                    if old != p.name {
                        // doi ten: cac dong / o mac dinh dang dung ten cu chuyen sang ten moi
                        for sl in SLOTS.lock().unwrap().iter_mut() {
                            if sl.proxy == old {
                                sl.proxy = p.name.clone();
                            }
                        }
                        let mut cfg = CFG.lock().unwrap();
                        if cfg.proxy == old {
                            cfg.proxy = p.name.clone();
                        }
                    }
                    for (c, t) in p.cells().iter().enumerate() {
                        lv_put(list, sel, c, t);
                    }
                    set_text(pm_item(h, ID_PM_INFO), &pm_info_text(&p.name, ""));
                    pm_after_change();
                }
            }
        }
        ID_PM_DEL => {
            let sel = match pm_selected(h) {
                Some(i) => i,
                None => {
                    warn("Hãy chọn một proxy trong danh sách để xóa.");
                    return;
                }
            };
            let name = PROXIES.lock().unwrap().get(sel).map(|x| x.name.clone()).unwrap_or_default();
            if msgbox(h, &format!("Xóa proxy \"{}\"?\nCác dòng đang dùng proxy này sẽ báo lỗi khi mở tab cho tới khi gán proxy khác.", name), "Xóa proxy", MB_YESNO | MB_ICONQUESTION) != IDYES {
                return;
            }
            {
                let mut g = PROXIES.lock().unwrap();
                if sel < g.len() {
                    g.remove(sel);
                }
            }
            SendMessageW(list, LVM_DELETEITEM, sel, 0);
            set_text(pm_item(h, ID_PM_INFO), "");
            pm_after_change();
        }
        ID_PM_TEST => match pm_selected(h) {
            Some(i) => {
                let name = PROXIES.lock().unwrap().get(i).map(|x| x.name.clone());
                if let Some(n) = name {
                    pm_start_test(h, vec![n]);
                }
            }
            None => {
                warn("Hãy chọn một proxy trong danh sách để kiểm tra.");
            }
        },
        ID_PM_TESTALL => {
            let names: Vec<String> = PROXIES.lock().unwrap().iter().map(|x| x.name.clone()).collect();
            pm_start_test(h, names);
        }
        ID_PM_CLOSE => {
            DestroyWindow(h);
        }
        _ => {}
    }
}

unsafe fn pm_edit(h: HWND, x: i32, y: i32, wd: i32, id: isize) -> HWND {
    child(h, "EDIT", "", WS_TABSTOP | ES_AUTOHSCROLL as u32, WS_EX_CLIENTEDGE, x, y, wd, 22, id)
}

unsafe fn pm_create(h: HWND) {
    let lv = child(h, "SysListView32", "", WS_TABSTOP | 0x1 /* LVS_REPORT */ | 0x4 /* LVS_SINGLESEL */ | 0x8 /* LVS_SHOWSELALWAYS */, WS_EX_CLIENTEDGE, 10, 10, 620, 200, ID_PM_LIST);
    SendMessageW(lv, LVM_SETEXTENDEDLISTVIEWSTYLE, 0, 0x20 | 0x1); // FULLROWSELECT | GRIDLINES
    for (i, (name, width)) in PM_COLS.iter().enumerate() {
        let t = w(name);
        let mut col: LVCOLUMNW = zeroed();
        col.mask = 0x1 | 0x2 | 0x4 | 0x8; // FMT | WIDTH | TEXT | SUBITEM
        col.cx = *width;
        col.pszText = t.as_ptr() as *mut u16;
        col.iSubItem = i as i32;
        SendMessageW(lv, LVM_INSERTCOLUMNW, i, &col as *const LVCOLUMNW as LPARAM);
    }
    label(h, "Tên:", 10, 222, 38);
    pm_edit(h, 50, 218, 130, ID_PM_NAME);
    label(h, "Loại:", 196, 222, 38);
    let ty = child(h, "COMBOBOX", "", WS_TABSTOP | WS_VSCROLL | 0x3 /* CBS_DROPDOWNLIST */, 0, 236, 218, 90, 100, ID_PM_TYPE);
    SendMessageW(ty, 0x0143, 0, w("SOCKS5").as_ptr() as LPARAM);
    SendMessageW(ty, 0x0143, 0, w("HTTP").as_ptr() as LPARAM);
    SendMessageW(ty, 0x014E, 0, 0);
    label(h, "IP:", 340, 222, 24);
    pm_edit(h, 366, 218, 170, ID_PM_HOST);
    label(h, "Port:", 548, 222, 32);
    pm_edit(h, 582, 218, 48, ID_PM_PORT);
    label(h, "User:", 10, 254, 38);
    pm_edit(h, 50, 250, 130, ID_PM_USER);
    label(h, "Pass:", 196, 254, 38);
    pm_edit(h, 236, 250, 170, ID_PM_PASS);
    label(h, "Dán cả chuỗi proxy vào ô IP cũng được", 416, 254, 214);
    button(h, "Thêm", 10, 286, 80, ID_PM_ADD);
    button(h, "Sửa", 96, 286, 80, ID_PM_UPD);
    button(h, "Xóa", 182, 286, 80, ID_PM_DEL);
    button(h, "Kiểm tra", 290, 286, 90, ID_PM_TEST);
    button(h, "Kiểm tra hết", 386, 286, 110, ID_PM_TESTALL);
    button(h, "Đóng", 550, 286, 80, ID_PM_CLOSE);
    child(h, "STATIC", "", 0x1000 /* SS_SUNKEN */, 0, 10, 326, 620, 38, ID_PM_INFO);
    let items: Vec<ProxyItem> = PROXIES.lock().unwrap().clone();
    for (i, p) in items.iter().enumerate() {
        lv_add(lv, i, &p.cells());
    }
    if !items.is_empty() {
        pm_select(lv, 0);
    }
}

unsafe extern "system" fn pm_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            pm_create(hwnd);
            0
        }
        WM_COMMAND => {
            pm_command(hwnd, (wp & 0xFFFF) as isize);
            0
        }
        WM_NOTIFY => {
            let nm = &*(lp as *const NMHDR);
            if nm.hwndFrom == pm_item(hwnd, ID_PM_LIST) && nm.code == 0xFFFF_FF9B /* LVN_ITEMCHANGED */ {
                pm_fill_form(hwnd);
            }
            0
        }
        WM_APP_PXRESULT => {
            let b = Box::from_raw(lp as *mut (String, String));
            pm_apply_result(hwnd, &b.0, &b.1);
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            PMGR.store(0, Ordering::Relaxed);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn open_proxy_mgr(owner: HWND) {
    let cur = PMGR.load(Ordering::Relaxed);
    if cur != 0 && IsWindow(cur) != 0 {
        SetForegroundWindow(cur);
        return;
    }
    let hinst = GetModuleHandleW(null());
    let cls = w("ToonieProxyMgr");
    let mut wc: WNDCLASSW = zeroed();
    wc.lpfnWndProc = Some(pm_proc);
    wc.hInstance = hinst;
    wc.hCursor = LoadCursorW(0, IDC_ARROW);
    wc.hIcon = LoadIconW(hinst, 1 as *const u16);
    wc.hbrBackground = (COLOR_BTNFACE + 1) as HBRUSH;
    wc.lpszClassName = cls.as_ptr();
    RegisterClassW(&wc); // lan mo thu 2 tra ve 0 vi lop da dang ky - khong sao
    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
    let mut r = RECT { left: 0, top: 0, right: 640, bottom: 374 };
    AdjustWindowRectEx(&mut r, style, 0, WS_EX_CONTROLPARENT);
    let (ww, wh) = (r.right - r.left, r.bottom - r.top);
    let mut orc: RECT = zeroed();
    GetWindowRect(owner, &mut orc);
    let hwnd = CreateWindowExW(WS_EX_CONTROLPARENT, cls.as_ptr(), w("Quản lý proxy").as_ptr(), style, orc.left + 30, orc.top + 30, ww, wh, owner, 0, hinst, null());
    if hwnd != 0 {
        PMGR.store(hwnd, Ordering::Relaxed);
        set_icons(hwnd);
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
    }
}

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            create_controls(hwnd);
            SetTimer(hwnd, 1, 1000, None);
            refresh_buttons();
            0
        }
        WM_COMMAND => {
            if let Some(c) = CTL.get() {
                match (wp & 0xFFFF) as isize {
                    ID_BROWSE_EMU => browse(hwnd, c.emu, "Emulator (*.jar)\0*.jar\0"),
                    ID_BROWSE_GAME => browse(hwnd, c.game, GAME_FILTER),
                    ID_BROWSE_JAVA => browse(hwnd, c.java, "javaw.exe\0javaw.exe\0"),
                    ID_START => on_start(c),
                    ID_ARRANGE => arrange(c),
                    ID_CLOSEALL => act_close_tabs(c, true),
                    ID_ADDGAME => {
                        let cur = get_text(c.game);
                        let files = pick_files(hwnd, GAME_FILTER, true, &cur);
                        let files: Vec<String> = files.into_iter().filter(|f| is_game_file(f)).collect();
                        if !files.is_empty() {
                            add_rows(c, files);
                        }
                    }
                    ID_ADDN => {
                        let g = get_text(c.game).trim().trim_matches('"').to_string();
                        let n = get_text(c.tabs).trim().parse::<usize>().unwrap_or(0);
                        if !is_game_file(&g) {
                            msgbox(hwnd, "Đường dẫn file Game không đúng!", "Lỗi", MB_OK | MB_ICONWARNING);
                        } else if n == 0 || n > 200 {
                            msgbox(hwnd, "Số lượng tab không đúng!", "Lỗi", MB_OK | MB_ICONWARNING);
                        } else {
                            add_rows(c, vec![g; n]);
                        }
                    }
                    ID_DELROW => act_remove_rows(c),
                    ID_SETPROXY => act_set_proxy(c),
                    ID_PROXYMGR => open_proxy_mgr(hwnd),
                    ID_PROXYCOMBO => {
                        if (wp >> 16) & 0xFFFF == 1 /* CBN_SELCHANGE */ {
                            CFG.lock().unwrap().proxy = combo_selected_name(c.proxy);
                        }
                    }
                    ID_HIDE => act_hide(c, true),
                    ID_SHOW => act_hide(c, false),
                    ID_CLOSETABS => act_close_tabs(c, false),
                    _ => {}
                }
                refresh_buttons();
            }
            0
        }
        WM_NOTIFY => {
            let nm = &*(lp as *const NMHDR);
            if let Some(c) = CTL.get() {
                if nm.hwndFrom == c.lv && nm.code == 0xFFFF_FFFD /* NM_DBLCLK */ {
                    let sel = SendMessageW(c.lv, LVM_GETNEXTITEM, usize::MAX, 0x2 /* LVNI_SELECTED */);
                    if sel >= 0 {
                        change_game(c, sel as usize);
                    }
                }
            }
            0
        }
        WM_TIMER => {
            auto_tick();
            refresh_buttons();
            0
        }
        WM_APP_MSGBOX => {
            let b = Box::from_raw(lp as *mut (String, String, u32));
            msgbox(hwnd, &b.0, &b.1, MB_OK | b.2);
            0
        }
        WM_APP_DONE => {
            LAUNCHING.store(false, Ordering::Relaxed);
            refresh_buttons();
            0
        }
        WM_CLOSE => {
            if let Some(c) = CTL.get() {
                if !snapshot().is_empty() {
                    if msgbox(hwnd, "Bạn có chắc muốn thoát và đóng hết Tab?", "Xác nhận thoát", MB_YESNO | MB_ICONQUESTION) != IDYES {
                        return 0;
                    }
                    close_slots(c, &targets_all_running());
                }
                if let Ok(n) = read_settings(c) {
                    *CFG.lock().unwrap() = n;
                }
                persist();
            }
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn main() {
    unsafe {
        let mut loaded = Settings::load();
        // file ini ban truoc luu proxy dang chuoi o khoa proxy= -> doi thanh 1 proxy co ten
        if !loaded.proxy.is_empty() && !loaded.proxies.iter().any(|p| p.name == loaded.proxy) {
            if let Ok(Some(u)) = relay::parse(&loaded.proxy) {
                let name = "Proxy cũ".to_string();
                loaded.proxies.push(ProxyItem { name: name.clone(), http: u.http, host: u.host, port: u.port, user: u.user, pass: u.pass, result: String::new() });
                loaded.proxy = name;
            } else {
                loaded.proxy.clear();
            }
        }
        *PROXIES.lock().unwrap() = loaded.proxies.clone();
        {
            let mut sl = SLOTS.lock().unwrap();
            for (a, g, px) in &loaded.slots {
                sl.push(Slot { game: g.clone(), auto: *a, proxy: px.clone(), tab: None });
            }
        }
        *CFG.lock().unwrap() = loaded;
        {
            let mut cache = LISTCACHE.lock().unwrap();
            cache.clear();
        }
        let icc = INITCOMMONCONTROLSEX { dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: 0x1 /* ICC_LISTVIEW_CLASSES */ };
        InitCommonControlsEx(&icc);

        let hinst = GetModuleHandleW(null());
        let font = CreateFontW(-12, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 5, 0, w("Segoe UI").as_ptr());
        FONT.store(font, Ordering::Relaxed);

        let cls_main = w("ToonieRemoterMain");
        let mut wc: WNDCLASSW = zeroed();
        wc.lpfnWndProc = Some(main_proc);
        wc.hInstance = hinst;
        wc.hCursor = LoadCursorW(0, IDC_ARROW);
        wc.hIcon = LoadIconW(hinst, 1 as *const u16);
        wc.hbrBackground = (COLOR_BTNFACE + 1) as HBRUSH;
        wc.lpszClassName = cls_main.as_ptr();
        RegisterClassW(&wc);

        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut r = RECT { left: 0, top: 0, right: 640, bottom: 588 };
        AdjustWindowRectEx(&mut r, style, 0, 0);
        let (ww, wh) = (r.right - r.left, r.bottom - r.top);
        let x = (GetSystemMetrics(SM_CXSCREEN) - ww) / 2;
        let y = (GetSystemMetrics(SM_CYSCREEN) - wh) / 3;
        let main = CreateWindowExW(0, cls_main.as_ptr(), w("Toonie Remoter").as_ptr(), style, x.max(0), y.max(0), ww, wh, 0, 0, hinst, null());
        set_icons(main);
        ShowWindow(main, SW_SHOW);
        UpdateWindow(main);

        if std::env::args().any(|a| a == "--autostart") {
            PostMessageW(main, WM_COMMAND, ID_START as WPARAM, 0);
        }

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {
            let mg = PMGR.load(Ordering::Relaxed);
            if mg != 0 && IsDialogMessageW(mg, &msg) != 0 {
                continue;
            }
            if IsDialogMessageW(main, &msg) != 0 {
                continue;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
