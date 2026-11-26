//! Xbox Series shaders to host shaders.
//!
//! An Xbox Series shader is a standard DXBC container ([`dxbc`]). Its `DXIL`
//! part ([`dxil`]) may carry Xbox additions around the LLVM bitcode: an
//! `XDAT` table before it indexing blobs after it, among them `xbop`
//! (compiler options) and the zlib-compressed GFX10 code. The bitcode may use
//! console-only intrinsics, so the GFX10 code is translated in general.

pub mod bvh;
pub mod dxbc;
pub mod dxil;
pub mod emit;
pub mod gfx10;
pub mod io;
pub mod ir;
pub mod native;
pub mod resource;
pub mod spill;
pub mod structure;

pub use dxbc::{Container, FourCC, Part};
pub use dxil::{DxilProgram, ShaderKind, XboxExtras};
pub use native::{
    HwStage, NativeProgram, ResourceType, RootParam, RootType, UsedResource, native_programs,
};

/// Whether the title's ray-tracing structure builder runs, so that its rays
/// have structures to trace: on unless `VICTORY_BVH_BUILDER=0` (on by default
/// since 2026-11-26, docs/fork-ray-tracing.md). Read once; every crate that
/// asks goes through here, so they cannot disagree.
pub fn bvh_builder_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| builder_setting(std::env::var_os("VICTORY_BVH_BUILDER").as_deref()))
}

/// [`bvh_builder_on`] for a value of the setting.
fn builder_setting(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_none_or(|v| v != "0" && !v.eq_ignore_ascii_case("off"))
}

#[cfg(test)]
mod builder_setting_tests {
    use super::builder_setting;
    use std::ffi::OsStr;

    #[test]
    fn the_builder_is_on_unless_turned_off() {
        assert!(builder_setting(None));
        assert!(builder_setting(Some(OsStr::new("1"))));
        assert!(!builder_setting(Some(OsStr::new("0"))));
        assert!(!builder_setting(Some(OsStr::new("off"))));
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ShaderError {
    #[error("not a DXBC container")]
    NotDxbc,
    #[error("malformed shader: {0}")]
    Malformed(&'static str),
    #[error("GFX10 code at {offset:#x}: {reason}")]
    Isa { offset: usize, reason: &'static str },
    /// Valid code the translator cannot handle yet: an opcode name or a construct.
    #[error("GFX10 code at {offset:#x}: {what} is not supported")]
    Unsupported { offset: usize, what: &'static str },
}
