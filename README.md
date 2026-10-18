# Toonie Remoter

Công cụ Windows (Rust, thuần Win32, crate `windows-sys 0.52`, không cần .NET) để mở nhiều tab
MicroEmulator cùng lúc, sắp xếp cửa sổ, gửi phím đồng loạt và theo dõi CPU/RAM từng tab.
Chạy được từ Windows 7 / Server 2008 R2 trở lên.

## Tính năng

- Mở nhiều tab emulator, mỗi dòng trong danh sách là một tab với game riêng (`.jar` / `.jad`).
- Sắp xếp tab, ẩn / hiện, đóng từng tab hoặc đóng hết.
- Gửi phím đồng loạt tới các tab.
- Ngủ / dậy cho tab (thu nhỏ, hạ ưu tiên CPU, thu hồi RAM) và tự ngủ khi không thao tác.
- Xem CPU, RAM, PID từng tab và tổng toàn hệ thống.
- Tách dữ liệu từng tab (`--id` riêng cho mỗi tab).
- **Proxy riêng cho từng tab** (SOCKS5 hoặc HTTP, có hoặc không tài khoản).

## Proxy

Proxy được lưu thành danh sách có tên. Bấm **Quản lý…** (cạnh ô Proxy) để mở bảng quản lý:

| Ô | Ý nghĩa |
|---|---|
| Tên | tên để chọn proxy (bỏ trống thì tự đặt `ip:port`) |
| Loại | SOCKS5 hoặc HTTP |
| IP, Port | địa chỉ và cổng proxy |
| User, Pass | tài khoản (bỏ trống nếu proxy không cần) |
| Kết quả | kết quả kiểm tra gần nhất, ví dụ `OK - 230 ms` hoặc `Lỗi: Proxy từ chối tài khoản/mật khẩu` |

- **Thêm / Sửa / Xóa** thao tác trên dòng đang chọn. Đổi tên proxy thì các dòng đang dùng nó tự đổi theo.
- **Kiểm tra / Kiểm tra hết** mở kết nối tới `example.com:80` qua proxy và đo thời gian.
- Dán cả chuỗi vào ô **IP** cũng được: `host:port`, `host:port:user:pass`, `socks5://user:pass@host:port`, `http://user:pass@host:port`. Tool tự tách ra các ô.

Ngoài cửa sổ chính:

- Ô **Proxy** (danh sách chọn) là proxy mặc định cho các dòng chưa gán riêng.
- **Gán dòng** gán proxy đang chọn cho các dòng đã tick (không tick dòng nào = tất cả). Chọn
  `(không dùng proxy)` rồi bấm Gán dòng = bỏ proxy riêng của dòng đó.
- Cột **Proxy** trong danh sách hiện proxy của từng dòng (`-` = dùng proxy mặc định).
- Đổi proxy có hiệu lực khi đóng và mở lại tab.

### Cách hoạt động

Java bỏ qua `-Djava.net.socks.username/password` (nó gửi user hệ thống với mật khẩu rỗng), nên
không đăng nhập proxy trực tiếp bằng cờ JVM được. Thay vào đó mỗi tab có một relay SOCKS5 cục bộ
(`src/relay.rs`) nghe ở `127.0.0.1:<cổng ngẫu nhiên>`, không cần đăng nhập. Tab chạy với:

```
-DsocksProxyHost=127.0.0.1 -DsocksProxyPort=<cổng relay>
```

Relay tự đăng nhập lên proxy thật (SOCKS5 hoặc HTTP CONNECT) rồi chuyển dữ liệu hai chiều.
Relay sống cùng tab, đóng tab thì relay dừng.

- Tên miền được phân giải ở phía proxy.
- Kết nối tới `localhost` / `127.x` đi thẳng, không qua proxy.
- Cả socket (`socket://`) và `http://` của MIDlet đều đi qua proxy, vì emulator dùng `java.net.Socket`
  và `HttpURLConnection` thuần.
- Tài khoản và mật khẩu proxy lưu **chữ thường** trong `ToonieRemoter.ini`.

## Build

Xem `BUILD.txt`. Tóm tắt:

```
rustup toolchain install 1.77.2
cargo +1.77.2 build --release
```

File ra: `target\release\toonie_remoter.exe`. Có thể cross-compile từ Linux (Rust 1.75 +
`gcc-mingw-w64-x86-64`, target `x86_64-pc-windows-gnu`, `build-std` từ `rust-src`).

## Tham số dòng lệnh

```
ToonieRemoter.exe --autostart     tự bấm "Bắt đầu" khi mở
```

## File cài đặt `ToonieRemoter.ini` (cạnh file exe)

| Khóa | Ý nghĩa |
|---|---|
| `emulator`, `game`, `java` | đường dẫn emulator, game, `javaw.exe` |
| `width`, `height`, `tabs`, `wait` | kích thước tab, số tab, thời gian chờ giữa các tab (ms) |
| `keydelay` | độ trễ sau khi đưa tab lên trước (ms), mặc định 60 |
| `keyhold` | thời gian giữ phím (ms), mặc định 40 |
| `keymode` | `focus` (SendInput, chắc chắn) hoặc `post` (PostMessage, không chuyển cửa sổ nhưng Java có thể bỏ qua) |
| `capture` | `1` bật bắt phím cả khi đang gõ ở cửa sổ tab |
| `sleepafter` | tự ngủ sau N giây không thao tác, tối thiểu 5 |
| `separateid` | `1` = tách dữ liệu từng tab |
| `proxy` | **tên** proxy mặc định (để trống = không dùng proxy) |
| `px=tên<TAB>loại<TAB>ip<TAB>port<TAB>user<TAB>pass` | một proxy trong danh sách (loại: `socks5` hoặc `http`) |
| `slot=<auto 0/1>\|<đường dẫn game>\|<tên proxy riêng>` | mỗi dòng là một tab; trường proxy có thể để trống. Dạng cũ `slot=<auto>\|<game>` vẫn đọc được |

## Cấu trúc mã nguồn

```
src/main.rs    giao diện Win32, quản lý tab, gửi phím, ngủ/dậy, thống kê
src/relay.rs   relay SOCKS5 cục bộ cho proxy (chỉ dùng std)
build.rs       nhúng icon + manifest bằng windres nếu có (không có thì bỏ qua)
```

Logo lấy từ Toonie Emulator build 2.
