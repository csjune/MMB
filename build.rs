use std::env;
use std::io;
use std::path::Path;

use winresource::VersionInfo;

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
    let package_version = env::var("CARGO_PKG_VERSION")
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "CARGO_PKG_VERSION is missing"))?;
    let numeric_version = windows_numeric_version()?;

    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon(icon_path)
        .set("FileDescription", "MMB")
        .set("ProductName", "MMB")
        .set("FileVersion", &package_version)
        .set("ProductVersion", &package_version)
        .set_version_info(VersionInfo::FILEVERSION, numeric_version)
        .set_version_info(VersionInfo::PRODUCTVERSION, numeric_version);
    resource.compile()
}

fn windows_numeric_version() -> io::Result<u64> {
    let component = |name: &str| -> io::Result<u64> {
        let value = env::var(name)
            .map_err(|_| io::Error::new(io::ErrorKind::NotFound, format!("{name} is missing")))?;
        let value = value.parse::<u16>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{name} is not a valid Windows version component"),
            )
        })?;
        Ok(u64::from(value))
    };

    Ok((component("CARGO_PKG_VERSION_MAJOR")? << 48)
        | (component("CARGO_PKG_VERSION_MINOR")? << 32)
        | (component("CARGO_PKG_VERSION_PATCH")? << 16))
}
