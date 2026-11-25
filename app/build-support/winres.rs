//! Windows resources for an executable: the app icon (resource id 1, from
//! `assets/icon/moku.ico`) and a VERSIONINFO block (CompanyName, ProductName, FileDescription,
//! versions from `CARGO_PKG_VERSION`).
//!
//! No crate: the build script writes an `.rc` file, compiles it with `rc.exe` from the Windows SDK
//! (found without a Developer Command Prompt, see `find_rc`) and links the resulting `.res` into
//! the binaries with `cargo:rustc-link-arg-bins`. Test harnesses do not get it.
//!
//! Shared by the build scripts of `moku`, `moku-settings`, `moku-setup` and `mokuctl`:
//!
//! ```ignore
//! #[path = "../build-support/winres.rs"]
//! mod winres;
//! fn main() { winres::embed("Moku dynamic island", "moku"); }
//! ```
//!
//! Environment: `MOKU_RC` names the `rc.exe` to use; `MOKU_NO_RESOURCES=1` builds without the
//! resources (a warning says so) for machines that have no Windows SDK.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Embed the icon and the VERSIONINFO of `<name>.exe` (`file_description` is what Task Manager and
/// the file's Properties dialog show).
pub fn embed(file_description: &str, name: &str) {
    println!("cargo:rerun-if-env-changed=MOKU_RC");
    println!("cargo:rerun-if-env-changed=MOKU_NO_RESOURCES");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../build-support/winres.rs");

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let ico = manifest_dir.parent().expect("workspace root").join("assets").join("icon").join("moku.ico");
    println!("cargo:rerun-if-changed={}", ico.display());

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        println!("cargo:warning={name}.exe: icon and version resources are only embedded for the MSVC target");
        return;
    }
    if env::var_os("MOKU_NO_RESOURCES").is_some_and(|v| !v.is_empty() && v != "0") {
        println!("cargo:warning={name}.exe is built without its icon and version resources (MOKU_NO_RESOURCES)");
        return;
    }
    assert!(ico.is_file(), "{} is missing (regenerate it with app\\assets\\icon\\make-icon.ps1)", ico.display());

    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let rc_file = out.join(format!("{name}.rc"));
    let res_file = out.join(format!("{name}.res"));
    std::fs::write(&rc_file, rc_source(&ico, file_description, name)).expect("write .rc");

    let rc = find_rc().unwrap_or_else(|| {
        panic!(
            "rc.exe (the Windows SDK resource compiler) was not found. Install the \"Windows 10/11 SDK\" component of Visual Studio \
             / Build Tools, or point MOKU_RC at rc.exe, or set MOKU_NO_RESOURCES=1 to build without the icon"
        )
    });
    let o = Command::new(&rc)
        .args(["/nologo", "/c65001", "/fo"])
        .arg(&res_file)
        .arg(&rc_file)
        .output()
        .unwrap_or_else(|e| panic!("cannot run {}: {e}", rc.display()));
    if !o.status.success() {
        panic!(
            "{} failed ({}):\n{}{}",
            rc.display(),
            o.status,
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
    }
    println!("cargo:rustc-link-arg-bins={}", res_file.display());
    embed_identity_manifest(&manifest_dir, &out, name);
}

/// Only for a build that is to get a package identity (`release.ps1 -SparsePackage` sets
/// `MOKU_MSIX_PUBLISHER`): link the application manifest with the `msix` element that ties
/// `moku.exe` to the identity package (`packaging/sparse/moku.manifest.in`). Without the variable,
/// nothing changes: no manifest is embedded and the exe is built exactly as before.
fn embed_identity_manifest(manifest_dir: &Path, out: &Path, name: &str) {
    println!("cargo:rerun-if-env-changed=MOKU_MSIX_PUBLISHER");
    println!("cargo:rerun-if-env-changed=MOKU_MSIX_NAME");
    let Some(publisher) = env::var("MOKU_MSIX_PUBLISHER").ok().filter(|p| !p.trim().is_empty()) else { return };
    if name != "moku" {
        return;
    }
    let package = env::var("MOKU_MSIX_NAME").ok().filter(|p| !p.trim().is_empty()).unwrap_or_else(|| "Moku.Island".to_string());
    let template = manifest_dir.parent().expect("workspace root").join("packaging").join("sparse").join("moku.manifest.in");
    println!("cargo:rerun-if-changed={}", template.display());
    let text = std::fs::read_to_string(&template).unwrap_or_else(|e| panic!("{}: {e}", template.display()));
    let xml_attr = |s: &str| s.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;");
    let text = text.replace("@PUBLISHER@", &xml_attr(publisher.trim())).replace("@PACKAGE_NAME@", &xml_attr(package.trim())).replace("@APPLICATION_ID@", "MokuIsland");
    let file = out.join("moku.identity.manifest");
    std::fs::write(&file, text).expect("write the identity manifest");
    // rustc links without a manifest of its own; these two give the exe this one.
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", file.display());
}

/// A string literal for an `.rc` file.
fn rc_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\"\""))
}

fn rc_source(ico: &Path, file_description: &str, name: &str) -> String {
    let var = |k: &str| env::var(k).unwrap_or_default();
    let num = |k: &str| var(k).parse::<u16>().unwrap_or(0);
    let (major, minor, patch) = (num("CARGO_PKG_VERSION_MAJOR"), num("CARGO_PKG_VERSION_MINOR"), num("CARGO_PKG_VERSION_PATCH"));
    let version = rc_str(&var("CARGO_PKG_VERSION"));
    // VS_FF_PRERELEASE for 0.3.0-rc.1 and the like.
    let flags = if var("CARGO_PKG_VERSION_PRE").is_empty() { 0 } else { 2 };
    let ico = rc_str(&ico.display().to_string());
    let company = rc_str("Moku contributors");
    let description = rc_str(file_description);
    let internal = rc_str(name);
    let original = rc_str(&format!("{name}.exe"));
    let product = rc_str("Moku");
    format!(
        r#"#pragma code_page(65001)
1 ICON {ico}
1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEFLAGSMASK 0x3fL
FILEFLAGS {flags}L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName", {company}
            VALUE "FileDescription", {description}
            VALUE "FileVersion", {version}
            VALUE "InternalName", {internal}
            VALUE "OriginalFilename", {original}
            VALUE "ProductName", {product}
            VALUE "ProductVersion", {version}
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#
    )
}

/// The Windows SDK's `rc.exe`, from (in this order): `MOKU_RC`; the Developer Command Prompt's
/// `WindowsSdkVerBinPath`; the SDK's install folder (registry, then the default one), newest
/// version first; `rc.exe` on `PATH`.
fn find_rc() -> Option<PathBuf> {
    if let Some(p) = env::var_os("MOKU_RC").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    if let Some(dir) = env::var_os("WindowsSdkVerBinPath").filter(|p| !p.is_empty()) {
        if let Some(rc) = rc_in_version_dir(Path::new(&dir)) {
            return Some(rc);
        }
    }
    let mut roots = kits_root_from_registry();
    if let Some(pf) = env::var_os("ProgramFiles(x86)") {
        roots.push(PathBuf::from(pf).join("Windows Kits").join("10"));
    }
    for root in roots {
        let bin = root.join("bin");
        let mut versions: Vec<(Vec<u32>, PathBuf)> = std::fs::read_dir(&bin)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let parts: Option<Vec<u32>> = name.split('.').map(|p| p.parse().ok()).collect();
                parts.filter(|p| p.len() >= 3).map(|p| (p, e.path()))
            })
            .collect();
        versions.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, dir) in versions {
            if let Some(rc) = rc_in_version_dir(&dir) {
                return Some(rc);
            }
        }
        if let Some(rc) = rc_in_version_dir(&bin) {
            return Some(rc);
        }
    }
    env::var_os("PATH").and_then(|paths| env::split_paths(&paths).map(|d| d.join("rc.exe")).find(|p| p.is_file()))
}

/// `<dir>\x64\rc.exe` (or x86 / arm64 on a host without x64).
fn rc_in_version_dir(dir: &Path) -> Option<PathBuf> {
    ["x64", "x86", "arm64"].iter().map(|arch| dir.join(arch).join("rc.exe")).find(|p| p.is_file())
}

/// `KitsRoot10` from `HKLM\SOFTWARE\Microsoft\Windows Kits\Installed Roots` (both registry views),
/// read with `reg.exe` so that the build script needs no crate.
fn kits_root_from_registry() -> Vec<PathBuf> {
    let system = PathBuf::from(env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into())).join("System32").join("reg.exe");
    let mut roots = vec![];
    for view in ["/reg:32", "/reg:64"] {
        let out = Command::new(&system).args(["query", r"HKLM\SOFTWARE\Microsoft\Windows Kits\Installed Roots", "/v", "KitsRoot10", view]).output();
        let Ok(out) = out else { continue };
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some((_, value)) = line.trim().split_once("REG_SZ") {
                let p = PathBuf::from(value.trim());
                if !roots.contains(&p) {
                    roots.push(p);
                }
            }
        }
    }
    roots
}
