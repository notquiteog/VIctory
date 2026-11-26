//! Makes the program's icon and the logo the window shows, and embeds the
//! icon (with the version information) as the executables' resource.
//!
//! By default the picture is drawn here: a dark rounded tile with the
//! letters "VI" made of plain polygons. With `VICTORY_LAUNCHER_LOGO=<png>`
//! set at build time, that picture is used instead (see README.md: such a
//! file is the builder's own and is not part of this repository).

use std::path::PathBuf;

use image::codecs::ico::{IcoEncoder, IcoFrame};
use image::imageops::FilterType;
use image::{Rgba, RgbaImage};

const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];
const LOGO_ENV: &str = "VICTORY_LAUNCHER_LOGO";

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
</assembly>
"#;

fn git(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (out.status.success() && !text.is_empty()).then_some(text)
}

/// Which commit this is built from (as `apps/victory/build.rs`): the
/// updater's "latest build" channel compares it with the published one.
fn commit() {
    println!("cargo:rerun-if-env-changed=GIT_COMMIT");
    println!("cargo:rerun-if-env-changed=GIT_COMMIT_DATE");
    for file in ["HEAD", "logs/HEAD"] {
        if let Some(path) = git(&["rev-parse", "--git-path", file]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let commit = env("GIT_COMMIT")
        .or_else(|| git(&["rev-parse", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_owned());
    let date = env("GIT_COMMIT_DATE")
        .or_else(|| git(&["show", "-s", "--format=%cs", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=VICTORY_COMMIT={}", commit.trim());
    println!("cargo:rustc-env=VICTORY_COMMIT_DATE={}", date.trim());
}

/// Whether the point is inside the polygon (even-odd).
fn inside(polygon: &[(f32, f32)], x: f32, y: f32) -> bool {
    let mut odd = false;
    for i in 0..polygon.len() {
        let (ax, ay) = polygon[i];
        let (bx, by) = polygon[(i + 1) % polygon.len()];
        if (ay > y) != (by > y) && x < ax + (y - ay) / (by - ay) * (bx - ax) {
            odd = !odd;
        }
    }
    odd
}

/// The default picture, 1024 pixels square: a dark tile with rounded
/// corners and the two letters in straight strokes of one width.
fn default_logo() -> RgbaImage {
    const SIDE: u32 = 1024;
    const RADIUS: f32 = 190.0;
    let tile = Rgba([24, 27, 33, 255]);
    let ink = Rgba([240, 242, 245, 255]);
    let v = [
        (212.0, 300.0),
        (326.0, 300.0),
        (432.0, 612.0),
        (538.0, 300.0),
        (652.0, 300.0),
        (489.0, 724.0),
        (375.0, 724.0),
    ];
    let i = [
        (708.0, 300.0),
        (812.0, 300.0),
        (812.0, 724.0),
        (708.0, 724.0),
    ];
    RgbaImage::from_fn(SIDE, SIDE, |px, py| {
        let (x, y) = (px as f32 + 0.5, py as f32 + 0.5);
        // The distance into the nearest corner's circle decides the rounding.
        let side = SIDE as f32;
        let cx = x.clamp(RADIUS, side - RADIUS);
        let cy = y.clamp(RADIUS, side - RADIUS);
        if (x - cx).hypot(y - cy) > RADIUS {
            Rgba([0, 0, 0, 0])
        } else if inside(&v, x, y) || inside(&i, x, y) {
            ink
        } else {
            tile
        }
    })
}

/// The picture fitted into a square of `size`, keeping its shape.
fn square(logo: &RgbaImage, size: u32) -> RgbaImage {
    let scale = size as f32 / logo.width().max(logo.height()) as f32;
    let w = ((logo.width() as f32 * scale).round() as u32).clamp(1, size);
    let h = ((logo.height() as f32 * scale).round() as u32).clamp(1, size);
    let small = image::imageops::resize(logo, w, h, FilterType::Lanczos3);
    let mut out = RgbaImage::new(size, size);
    image::imageops::overlay(
        &mut out,
        &small,
        i64::from((size - w) / 2),
        i64::from((size - h) / 2),
    );
    out
}

fn main() {
    commit();
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed={LOGO_ENV}");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let own = std::env::var_os(LOGO_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let logo = match &own {
        Some(path) => {
            println!("cargo:rerun-if-changed={}", path.display());
            image::open(path)
                .unwrap_or_else(|e| panic!("{LOGO_ENV}={}: {e}", path.display()))
                .to_rgba8()
        }
        None => default_logo(),
    };
    // (The window puts a builder's own picture on a light tile.)
    println!(
        "cargo:rustc-env=VICTORY_LOGO_IS_OWN={}",
        u8::from(own.is_some())
    );
    square(&logo, 256)
        .save(out.join("logo.png"))
        .expect("writing logo.png");
    let frames: Vec<IcoFrame> = SIZES
        .iter()
        .map(|&size| {
            let small = square(&logo, size);
            IcoFrame::as_png(small.as_raw(), size, size, image::ExtendedColorType::Rgba8)
                .expect("icon frame")
        })
        .collect();
    let ico = out.join("vi.ico");
    let file = std::fs::File::create(&ico).expect("vi.ico");
    IcoEncoder::new(file)
        .encode_images(&frames)
        .expect("writing vi.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon(ico.to_str().expect("OUT_DIR is not valid Unicode"))
            .set("ProductName", "VI")
            .set("FileDescription", "VI")
            .set("LegalCopyright", "GPL-3.0-or-later")
            .set_manifest(MANIFEST);
        resource
            .compile()
            .expect("embedding the icon resource (needs the Windows SDK's rc.exe)");
    }
}
