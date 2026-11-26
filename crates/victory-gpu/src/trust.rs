//! Who signed a file: the check every vendor DLL passes before we load it
//! (NVIDIA's Streamline and DLSS, AMD's FidelityFX).
//!
//! `WinVerifyTrust` (the Authenticode policy: an embedded signature that
//! chains to a trusted root and matches the file's bytes), then the signing
//! certificate's organization, which must be the vendor's. The file is
//! opened without write or delete sharing first and stays open, so what was
//! checked is what the loader maps.

use std::ffi::c_void;
use std::fs::File;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::ptr::{null, null_mut};

/// The signer every DLL of the DLSS folder must have.
pub const NVIDIA: &str = "NVIDIA Corporation";
/// The signer of AMD's FidelityFX DLLs ("Advanced Micro Devices, Inc.").
pub const AMD: &str = "Advanced Micro Devices";

#[repr(C)]
struct Guid(u32, u16, u16, [u8; 8]);

/// `WINTRUST_ACTION_GENERIC_VERIFY_V2`.
const VERIFY_V2: Guid = Guid(0x00aa_c56b, 0xcd44, 0x11d0, [0x8c, 0xc2, 0x00, 0xc0, 0x4f, 0xc2, 0x95, 0xee]);

/// `WINTRUST_FILE_INFO`.
#[repr(C)]
struct FileInfo {
    size: u32,
    path: *const u16,
    file: *mut c_void,
    known_subject: *const Guid,
}

/// `WINTRUST_DATA`.
#[repr(C)]
struct Data {
    size: u32,
    policy_callback: *mut c_void,
    sip_client: *mut c_void,
    ui_choice: u32,
    revocation_checks: u32,
    union_choice: u32,
    file: *mut FileInfo,
    state_action: u32,
    state: *mut c_void,
    url_reference: *const u16,
    prov_flags: u32,
    ui_context: u32,
    signature_settings: *mut c_void,
}

// wintrust.h.
const _: () = {
    assert!(size_of::<FileInfo>() == 32);
    assert!(size_of::<Data>() == 88);
    assert!(std::mem::offset_of!(Data, state) == 56);
};

const UI_NONE: u32 = 2;
const REVOKE_NONE: u32 = 0;
const CHOICE_FILE: u32 = 1;
const STATE_VERIFY: u32 = 1;
const STATE_CLOSE: u32 = 2;
/// No network: revocation from the local cache only.
const CACHE_ONLY_URL_RETRIEVAL: u32 = 0x1000;
/// `CERT_NAME_ATTR_TYPE`, with the attribute's object id.
const NAME_ATTR: u32 = 3;
/// The organization of a subject name.
const ORGANIZATION: &std::ffi::CStr = c"2.5.4.10";

#[link(name = "wintrust")]
unsafe extern "system" {
    fn WinVerifyTrust(window: *mut c_void, action: *const Guid, data: *mut Data) -> i32;
    fn WTHelperProvDataFromStateData(state: *mut c_void) -> *mut c_void;
    fn WTHelperGetProvSignerFromChain(provider: *mut c_void, signer: u32, counter_signer: i32, counter: u32) -> *mut c_void;
    /// A `CRYPT_PROVIDER_CERT`: its size, then the certificate context.
    fn WTHelperGetProvCertFromChain(signer: *mut c_void, index: u32) -> *mut [*const c_void; 2];
}

#[link(name = "crypt32")]
unsafe extern "system" {
    fn CertGetNameStringW(certificate: *const c_void, kind: u32, flags: u32, parameter: *const c_void, name: *mut u16, len: u32) -> u32;
}

/// A file whose signature was checked, held open so that it cannot be
/// replaced while the process uses it.
pub struct Signed {
    _file: File,
}

/// Opens `path` and checks that its embedded signature is valid and of an
/// organization whose name starts with `organization`. Why not, otherwise.
pub fn signed_by(path: &Path, organization: &str) -> Result<Signed, String> {
    use std::os::windows::ffi::OsStrExt;
    const SHARE_READ: u32 = 1;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(SHARE_READ)
        .open(path)
        .map_err(|e| format!("{} cannot be opened for reading alone ({e})", path.display()))?;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut info = FileInfo {
        size: size_of::<FileInfo>() as u32,
        path: wide.as_ptr(),
        file: file.as_raw_handle().cast(),
        known_subject: null(),
    };
    let mut data = Data {
        size: size_of::<Data>() as u32,
        policy_callback: null_mut(),
        sip_client: null_mut(),
        ui_choice: UI_NONE,
        revocation_checks: REVOKE_NONE,
        union_choice: CHOICE_FILE,
        file: &mut info,
        state_action: STATE_VERIFY,
        state: null_mut(),
        url_reference: null(),
        prov_flags: CACHE_ONLY_URL_RETRIEVAL,
        ui_context: 0,
        signature_settings: null_mut(),
    };
    // SAFETY: the structures above, alive for both calls; no window (no UI).
    let status = unsafe { WinVerifyTrust(usize::MAX as *mut c_void, &VERIFY_V2, &mut data) };
    let signer = if status == 0 {
        // SAFETY: the state of a successful verification, released below.
        unsafe { signer_organization(data.state) }
    } else {
        None
    };
    data.state_action = STATE_CLOSE;
    // SAFETY: releases the state the first call made.
    unsafe { WinVerifyTrust(usize::MAX as *mut c_void, &VERIFY_V2, &mut data) };
    if status != 0 {
        return Err(format!("{} has no valid signature (WinVerifyTrust {:#010x})", path.display(), status as u32));
    }
    match signer {
        Some(o) if o.starts_with(organization) => Ok(Signed { _file: file }),
        Some(o) => Err(format!("{} is signed by {o:?}, not {organization}", path.display())),
        None => Err(format!("{}: its signer's organization cannot be read", path.display())),
    }
}

/// The organization in the subject of the certificate that signed.
///
/// # Safety
/// `state` is the live state of a successful file verification.
unsafe fn signer_organization(state: *mut c_void) -> Option<String> {
    // SAFETY: the helpers walk the state: first signer, its own certificate.
    unsafe {
        let provider = WTHelperProvDataFromStateData(state);
        if provider.is_null() {
            return None;
        }
        let signer = WTHelperGetProvSignerFromChain(provider, 0, 0, 0);
        if signer.is_null() {
            return None;
        }
        let entry = WTHelperGetProvCertFromChain(signer, 0);
        if entry.is_null() || (*entry)[1].is_null() {
            return None;
        }
        let mut name = [0u16; 256];
        let len = CertGetNameStringW((*entry)[1], NAME_ATTR, 0, ORGANIZATION.as_ptr().cast(), name.as_mut_ptr(), name.len() as u32);
        (len > 1).then(|| String::from_utf16_lossy(&name[..len as usize - 1]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A system DLL is Microsoft's, validly; an unsigned file is nobody's.
    #[test]
    fn a_signature_names_its_organization() {
        let system = std::env::var_os("SystemRoot").map(std::path::PathBuf::from).unwrap();
        // Catalog-signed system files have no embedded signature: use one
        // that has (the D3D compiler ships embedded-signed).
        let dll = system.join("System32").join("D3DCompiler_47.dll");
        if dll.is_file() {
            match signed_by(&dll, NVIDIA) {
                Err(why) => assert!(why.contains("Microsoft") || why.contains("no valid signature"), "{why}"),
                Ok(_) => panic!("a Microsoft DLL passed as NVIDIA's"),
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain.dll");
        std::fs::write(&plain, b"MZ not a signed file").unwrap();
        assert!(signed_by(&plain, NVIDIA).err().unwrap().contains("no valid signature"));
    }
}
