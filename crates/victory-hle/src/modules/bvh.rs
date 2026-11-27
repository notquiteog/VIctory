//! The title's calls into its ray-tracing structure builder
//! (`XboxBvhBuilder.dll`, the title's own DLL, which still serves them): two
//! are seen on the way, for `victory_gpu::bvhsize` (structures' compacted
//! sizes given a bound before the card has them), and with the two below for
//! `victory_gpu::hwrt` (the census, `VICTORY_RT_CENSUS=1`, and the hardware
//! path, `VICTORY_RT_HARDWARE=1`). With the structure builder on (unless
//! `VICTORY_BVH_BUILDER=0`) and unless `VICTORY_BVH_SIZES_BOUND=0`, or with
//! either of those, the main image's import slots of them are pointed here
//! ([`wrap_imports`]).

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

const DLL: &str = "xboxbvhbuilder.dll";

/// `XboxBvhBuilder::GetRaytracingAccelerationStructurePrebuildInfo(inputs,
/// stream, info)`.
const PREBUILD: &str = "?GetRaytracingAccelerationStructurePrebuildInfo@XboxBvhBuilder@@QEAAXPEBUD3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS@@PEBUD3D12XBOX_XDXR_STREAM_DESC@@PEAUD3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO@@@Z";
/// `XboxBvhBuilder::BuildRaytracingAccelerationStructureBatched2X(list,
/// count, descs)`.
const BATCHED: &str = "?BuildRaytracingAccelerationStructureBatched2X@XboxBvhBuilder@@QEAAXPEAUID3D12GraphicsCommandList6@@IPEBUD3D12XBOX_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_BATCHED_DESC2@@@Z";
/// `XboxBvhBuilder::BuildRaytracingAccelerationStructure(list, desc, count,
/// postbuild)` (not imported by the title known so far).
const BUILD: &str = "?BuildRaytracingAccelerationStructure@XboxBvhBuilder@@QEAAXPEAUID3D12GraphicsCommandList4@@PEBUD3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC@@IPEBUD3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_DESC@@@Z";

/// `XboxBvhBuilder::RehydrateRaytracingAccelerationStructureBatched2X(list,
/// count, descs, extra)`: packed structures unpacked in place.
const REHYDRATE: &str = "?RehydrateRaytracingAccelerationStructureBatched2X@XboxBvhBuilder@@QEAAJPEAUID3D12GraphicsCommandList8@@IPEBUD3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC@@PEB_K@Z";
/// `XboxBvhBuilder::CopyRaytracingAccelerationStructure(list, dest, source,
/// mode)`.
const COPY: &str = "?CopyRaytracingAccelerationStructure@XboxBvhBuilder@@QEAAXPEAUID3D12GraphicsCommandList4@@_K1W4D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE@@@Z";

/// The DLL's own functions, as the import slots held them.
static ORIGINAL: [AtomicUsize; 5] = [const { AtomicUsize::new(0) }; 5];

type Prebuild = unsafe extern "system" fn(*mut c_void, *const c_void, *const c_void, *mut u64);
type Batched = unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *const c_void);
type Build = unsafe extern "system" fn(*mut c_void, *mut c_void, *const c_void, u32, *const c_void);
type Rehydrate = unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *const c_void, *const u64) -> i32;
type Copy = unsafe extern "system" fn(*mut c_void, *mut c_void, u64, u64, u32);

/// A member function (`this` first, as any argument, on x64).
unsafe extern "system" fn prebuild(this: *mut c_void, inputs: *const c_void, stream: *const c_void, info: *mut u64) {
    // SAFETY: the slot's value, a function of this type (only a slot that held
    // one is pointed here).
    let original: Prebuild = unsafe { std::mem::transmute(ORIGINAL[0].load(Ordering::Acquire)) };
    // SAFETY: the title's call, passed on as it was made.
    unsafe { original(this, inputs, stream, info) };
    if !info.is_null() {
        // SAFETY: the title's answer structure, just written: its first
        // member is `ResultDataMaxSizeInBytes`.
        let bound = unsafe { info.read_unaligned() };
        victory_gpu::bvhsize::prebuilt(inputs as u64, bound);
        victory_gpu::hwrt::prebuilt(inputs as u64, bound);
    }
}

unsafe extern "system" fn batched(this: *mut c_void, list: *mut c_void, count: u32, descs: *const c_void) {
    // Before the builder records anything (it may translate the asking at
    // once).
    victory_gpu::bvhsize::batch(descs as u64, count);
    victory_gpu::hwrt::batched(descs as u64, count);
    let planned = victory_gpu::hwrt::plan_batched(descs as u64, count);
    // SAFETY: as in `prebuild`.
    let original: Batched = unsafe { std::mem::transmute(ORIGINAL[1].load(Ordering::Acquire)) };
    // SAFETY: as in `prebuild`.
    unsafe { original(this, list, count, descs) };
    // After the builder's work in the list: the host's structures of them.
    victory_gpu::hwrt::done(list.cast_const().cast(), planned);
}

unsafe extern "system" fn build(this: *mut c_void, list: *mut c_void, desc: *const c_void, count: u32, postbuild: *const c_void) {
    victory_gpu::bvhsize::batch(desc as u64, 1);
    // SAFETY: as in `prebuild`.
    let original: Build = unsafe { std::mem::transmute(ORIGINAL[2].load(Ordering::Acquire)) };
    // SAFETY: as in `prebuild`.
    unsafe { original(this, list, desc, count, postbuild) };
}

unsafe extern "system" fn rehydrate(this: *mut c_void, list: *mut c_void, count: u32, descs: *const c_void, extra: *const u64) -> i32 {
    victory_gpu::hwrt::rehydrate(descs as u64, count, extra as u64);
    let planned = victory_gpu::hwrt::plan_rehydrate(descs as u64, count);
    // SAFETY: as in `prebuild`.
    let original: Rehydrate = unsafe { std::mem::transmute(ORIGINAL[3].load(Ordering::Acquire)) };
    // SAFETY: as in `prebuild`.
    let result = unsafe { original(this, list, count, descs, extra) };
    if result >= 0 {
        victory_gpu::hwrt::done(list.cast_const().cast(), planned);
    }
    result
}

unsafe extern "system" fn copy(this: *mut c_void, list: *mut c_void, dest: u64, source: u64, mode: u32) {
    victory_gpu::hwrt::copied(dest, source, mode);
    let planned = victory_gpu::hwrt::plan_copy(dest, source);
    // SAFETY: as in `prebuild`.
    let original: Copy = unsafe { std::mem::transmute(ORIGINAL[4].load(Ordering::Acquire)) };
    // SAFETY: as in `prebuild`.
    unsafe { original(this, list, dest, source, mode) };
    victory_gpu::hwrt::done(list.cast_const().cast(), planned);
}

/// Ours for an import, and where the DLL's own is kept.
fn ours(dll: &str, name: &str) -> Option<(usize, usize)> {
    if !dll.eq_ignore_ascii_case(DLL) {
        return None;
    }
    match name {
        PREBUILD => Some((prebuild as *const () as usize, 0)),
        BATCHED => Some((batched as *const () as usize, 1)),
        BUILD => Some((build as *const () as usize, 2)),
        // Only for the census and the hardware path.
        REHYDRATE if victory_gpu::hwrt::census() || victory_gpu::hwrt::hardware() => Some((rehydrate as *const () as usize, 3)),
        COPY if victory_gpu::hwrt::census() || victory_gpu::hwrt::hardware() => Some((copy as *const () as usize, 4)),
        _ => None,
    }
}

/// Points the bound import slots of the image at `base` (described by
/// `info`) that name the builder's two calls here. How many. Call after the
/// imports are bound and before the title runs.
pub fn wrap_imports(base: usize, info: &victory_pe::ImageInfo) -> usize {
    use windows_sys::Win32::System::Memory::{PAGE_READWRITE, VirtualProtect};
    if !victory_gpu::hwrt::wanted() {
        return 0;
    }
    let mut wrapped = 0;
    for module in info.imports.iter().chain(&info.delay_imports) {
        for entry in &module.entries {
            let victory_pe::ImportSymbol::Name { name, .. } = &entry.symbol else { continue };
            let Some((thunk, i)) = ours(&module.dll, name) else { continue };
            let slot = (base + entry.iat_slot_rva as usize) as *mut usize;
            // SAFETY: an import slot of the mapped image, bound by now.
            let was = unsafe { slot.read_volatile() };
            if was == 0 || was == thunk {
                continue;
            }
            // Two slots naming the same call hold the same function.
            ORIGINAL[i].store(was, Ordering::Release);
            let mut old = 0;
            // SAFETY: the slot's page, made writable for the write and put
            // back.
            unsafe {
                if VirtualProtect(slot.cast(), 8, PAGE_READWRITE, &mut old) == 0 {
                    continue;
                }
                slot.write_volatile(thunk);
                VirtualProtect(slot.cast(), 8, old, &mut old);
            }
            wrapped += 1;
        }
    }
    tracing::info!(
        wrapped,
        "structure builder: its prebuild answers and batched builds are seen on the way, for structures' compacted sizes \
         (VICTORY_BVH_SIZES_BOUND)"
    );
    wrapped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_calls_are_ours_and_nothing_else() {
        assert_eq!(ours("XboxBvhBuilder.dll", PREBUILD).map(|o| o.1), Some(0));
        assert_eq!(ours("xboxbvhbuilder.dll", BATCHED).map(|o| o.1), Some(1));
        assert_eq!(ours("XboxBvhBuilder.dll", BUILD).map(|o| o.1), Some(2));
        assert!(ours("XboxBvhBuilder.dll", "?Set64BitMortonCodes@XboxBvhBuilder@@QEAAX_N@Z").is_none());
        assert!(ours("other.dll", PREBUILD).is_none());
    }
}
