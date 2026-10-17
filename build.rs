// Nhung manifest (giao dien hien dai + tuong thich Win7/8/Server2012) bang windres neu co.
use std::{env, path::PathBuf, process::Command};
fn main() {
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=app.manifest");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("app_res.o");
    let tool = if env::var("TARGET").unwrap_or_default().starts_with("x86_64") { "x86_64-w64-mingw32-windres" } else { "windres" };
    for t in [tool, "windres"] {
        if let Ok(s) = Command::new(t).args(["-i", "app.rc", "-o"]).arg(&out).args(["-O", "coff"]).status() {
            if s.success() { println!("cargo:rustc-link-arg-bins={}", out.display()); return; }
        }
    }
    println!("cargo:warning=khong co windres - bo qua manifest (giao dien kieu cu)");
}
