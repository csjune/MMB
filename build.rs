use std::env;
use std::io;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=assets/tray-light.ico");
    println!("cargo:rerun-if-changed=assets/tray-dark.ico");
    println!("cargo:rerun-if-changed=ui/app.slint");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        compile_windows_resource().expect("failed to compile Windows resources");
    }

    slint_build::compile("ui/app.slint").expect("failed to compile Slint UI");
}

fn compile_windows_resource() -> io::Result<()> {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR")
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "CARGO_MANIFEST_DIR is missing"))?;
    let icon_path = Path::new(&manifest_dir).join("assets").join("app.ico");
    let icon_path = icon_path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "icon path is not UTF-8"))?;

    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon(icon_path)
        .set("FileDescription", "MMB")
        .set("ProductName", "MMB");
    resource.compile()
}
