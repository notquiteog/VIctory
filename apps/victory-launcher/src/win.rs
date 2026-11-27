//! The Windows calls the launcher needs, each behind a plain function:
//! known folders, the folder dialog, shortcuts, the uninstall entry, job
//! objects, and what the PC has (adapter, memory, disk).

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, HANDLE};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1,
};
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, IPersistFile,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
    RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegGetValueW, RegOpenKeyExW,
    RegSetValueExW,
};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_LocalAppData, FOLDERID_Programs, FOS_FORCEFILESYSTEM,
    FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, IShellItem, IShellLinkW, KF_FLAG_DEFAULT,
    SHCreateItemFromParsingName, SHGetKnownFolderPath, SIGDN_FILESYSPATH, ShellExecuteW, ShellLink,
};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW, SW_SHOWNORMAL};
use windows::core::{GUID, HSTRING, Interface, PCWSTR, PWSTR};

/// `CREATE_NO_WINDOW`: a console program started without a console window.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// `BELOW_NORMAL_PRIORITY_CLASS`.
pub const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

/// COM for this thread (the window's thread and the short-lived workers
/// that make shortcuts). Safe to ask for more than once.
pub fn com_init() {
    // SAFETY: no reserved pointer is passed; a second call on a thread only
    // returns S_FALSE.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
}

#[derive(Clone, Copy)]
pub enum Folder {
    LocalAppData,
    Desktop,
    StartMenuPrograms,
}

pub fn known_folder(which: Folder) -> Option<PathBuf> {
    let id: &GUID = match which {
        Folder::LocalAppData => &FOLDERID_LocalAppData,
        Folder::Desktop => &FOLDERID_Desktop,
        Folder::StartMenuPrograms => &FOLDERID_Programs,
    };
    // SAFETY: `id` is a valid known-folder id; the returned string is ours
    // to free with CoTaskMemFree, which is done after copying it.
    unsafe {
        let text: PWSTR = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let path = text.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(text.0.cast()));
        path
    }
}

/// The system's file dialog for one PNG picture; `None` if the user closed it.
pub fn pick_png(title: &str) -> Option<PathBuf> {
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    com_init();
    let (name, pattern) = (HSTRING::from("PNG pictures"), HSTRING::from("*.png"));
    // SAFETY: plain COM calls on objects created here, on this thread; the
    // filter's strings outlive the dialog.
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog.SetOptions(options | FOS_FORCEFILESYSTEM).ok()?;
        let _ = dialog.SetTitle(&HSTRING::from(title));
        let _ = dialog.SetFileTypes(&[COMDLG_FILTERSPEC {
            pszName: PCWSTR(name.as_ptr()),
            pszSpec: PCWSTR(pattern.as_ptr()),
        }]);
        dialog.Show(None).ok()?;
        let item = dialog.GetResult().ok()?;
        let text = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = text.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(text.0.cast()));
        path
    }
}

/// The system's folder dialog; `None` if the user closed it.
pub fn pick_folder(title: &str, start: &str) -> Option<PathBuf> {
    com_init();
    // SAFETY: plain COM calls on objects created here, on this thread.
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog
            .SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)
            .ok()?;
        let _ = dialog.SetTitle(&HSTRING::from(title));
        if !start.trim().is_empty() && Path::new(start.trim()).is_dir() {
            let item: windows::core::Result<IShellItem> =
                SHCreateItemFromParsingName(&HSTRING::from(start.trim()), None);
            if let Ok(item) = item {
                let _ = dialog.SetFolder(&item);
            }
        }
        dialog.Show(None).ok()?;
        let item = dialog.GetResult().ok()?;
        let text = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = text.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(text.0.cast()));
        path
    }
}

/// Opens a folder (or a file, or a web address) as a double click would.
pub fn open_in_shell(target: &Path) -> Result<(), String> {
    let target_w = HSTRING::from(target.as_os_str());
    // SAFETY: both strings outlive the call.
    let code = unsafe {
        ShellExecuteW(
            None,
            &HSTRING::from("open"),
            &target_w,
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if code.0 as usize > 32 {
        Ok(())
    } else {
        Err(format!("Windows could not open {}", target.display()))
    }
}

/// What a shortcut says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortcut {
    pub target: PathBuf,
    pub arguments: String,
    pub working_dir: PathBuf,
    pub icon: PathBuf,
    pub description: String,
}

/// Writes a `.lnk` (IShellLinkW, saved through IPersistFile).
pub fn create_shortcut(link: &Path, s: &Shortcut) -> Result<(), String> {
    com_init();
    let failed = |e: windows::core::Error| {
        format!(
            "Could not create the shortcut {}: {}",
            link.display(),
            e.message()
        )
    };
    if let Some(dir) = link.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    }
    // SAFETY: plain COM calls on an object created here; every string
    // outlives its call.
    unsafe {
        let shell: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(failed)?;
        shell
            .SetPath(&HSTRING::from(s.target.as_os_str()))
            .map_err(failed)?;
        shell
            .SetArguments(&HSTRING::from(s.arguments.as_str()))
            .map_err(failed)?;
        shell
            .SetWorkingDirectory(&HSTRING::from(s.working_dir.as_os_str()))
            .map_err(failed)?;
        shell
            .SetIconLocation(&HSTRING::from(s.icon.as_os_str()), 0)
            .map_err(failed)?;
        shell
            .SetDescription(&HSTRING::from(s.description.as_str()))
            .map_err(failed)?;
        let file: IPersistFile = shell.cast().map_err(failed)?;
        file.Save(&HSTRING::from(link.as_os_str()), true)
            .map_err(failed)
    }
}

/// Reads a `.lnk` back (the tests look at what an install wrote).
#[cfg(test)]
pub fn read_shortcut(link: &Path) -> Result<Shortcut, String> {
    com_init();
    let failed = |e: windows::core::Error| {
        format!(
            "Could not read the shortcut {}: {}",
            link.display(),
            e.message()
        )
    };
    // SAFETY: as `create_shortcut`; the buffers are as long as said.
    unsafe {
        let shell: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(failed)?;
        let file: IPersistFile = shell.cast().map_err(failed)?;
        file.Load(
            &HSTRING::from(link.as_os_str()),
            windows::Win32::System::Com::STGM_READ,
        )
        .map_err(failed)?;
        let text = |buffer: &[u16]| {
            String::from_utf16_lossy(&buffer[..buffer.iter().position(|&c| c == 0).unwrap_or(0)])
        };
        let mut buffer = [0u16; 1024];
        shell
            .GetPath(&mut buffer, std::ptr::null_mut(), 0)
            .map_err(failed)?;
        let target = PathBuf::from(text(&buffer));
        buffer.fill(0);
        shell.GetArguments(&mut buffer).map_err(failed)?;
        let arguments = text(&buffer);
        buffer.fill(0);
        shell.GetWorkingDirectory(&mut buffer).map_err(failed)?;
        let working_dir = PathBuf::from(text(&buffer));
        buffer.fill(0);
        let mut index = 0;
        shell
            .GetIconLocation(&mut buffer, &mut index)
            .map_err(failed)?;
        let icon = PathBuf::from(text(&buffer));
        buffer.fill(0);
        shell.GetDescription(&mut buffer).map_err(failed)?;
        Ok(Shortcut {
            target,
            arguments,
            working_dir,
            icon,
            description: text(&buffer),
        })
    }
}

const UNINSTALL_ROOT: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";

pub enum RegValue<'a> {
    Text(&'a str),
    Number(u32),
}

fn uninstall_subkey(name: &str) -> String {
    format!(r"{UNINSTALL_ROOT}\{name}")
}

/// Writes `HKCU\...\Uninstall\<name>` (per user: no administrator rights).
pub fn write_uninstall_entry(name: &str, values: &[(&str, RegValue)]) -> Result<(), String> {
    let subkey = uninstall_subkey(name);
    let failed = |what: &str, code: u32| {
        format!("Could not {what} the registry key HKCU\\{subkey} (error {code})")
    };
    let mut key = HKEY::default();
    // SAFETY: `key` receives a key that is closed below; the value buffers
    // outlive their calls.
    unsafe {
        let status = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(subkey.as_str()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        );
        if status != ERROR_SUCCESS {
            return Err(failed("create", status.0));
        }
        let mut result = Ok(());
        for (value_name, value) in values {
            let status = match value {
                RegValue::Text(text) => {
                    let data = wide(OsStr::new(text));
                    let bytes =
                        std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), data.len() * 2);
                    RegSetValueExW(key, &HSTRING::from(*value_name), None, REG_SZ, Some(bytes))
                }
                RegValue::Number(n) => RegSetValueExW(
                    key,
                    &HSTRING::from(*value_name),
                    None,
                    REG_DWORD,
                    Some(&n.to_le_bytes()),
                ),
            };
            if status != ERROR_SUCCESS {
                result = Err(failed("write to", status.0));
                break;
            }
        }
        let _ = RegCloseKey(key);
        result
    }
}

/// One text value of the uninstall entry; `None` if it is not there.
pub fn read_uninstall_value(name: &str, value: &str) -> Option<String> {
    let subkey = uninstall_subkey(name);
    let mut key = HKEY::default();
    // SAFETY: the buffer is as long as said, in bytes; the key is closed.
    unsafe {
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(subkey.as_str()),
            None,
            KEY_READ,
            &mut key,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let mut buffer = [0u16; 2048];
        let mut bytes = (buffer.len() * 2) as u32;
        let status = RegGetValueW(
            key,
            PCWSTR::null(),
            &HSTRING::from(value),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut bytes),
        );
        let _ = RegCloseKey(key);
        if status != ERROR_SUCCESS {
            return None;
        }
        let len = buffer.iter().position(|&c| c == 0).unwrap_or(0);
        Some(String::from_utf16_lossy(&buffer[..len]))
    }
}

/// Removes the uninstall entry. Not there counts as removed.
pub fn delete_uninstall_entry(name: &str) -> Result<(), String> {
    if name.is_empty() || name.contains('\\') {
        return Err(format!(
            "Refusing to delete the registry key named '{name}'"
        ));
    }
    let subkey = uninstall_subkey(name);
    // SAFETY: the string outlives the call.
    let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(subkey.as_str())) };
    if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(format!(
            "Could not remove the registry key HKCU\\{subkey} (error {})",
            status.0
        ))
    }
}

/// A job object that ends every process in it when it is ended or dropped:
/// the shader steps start helpers of their own, and Cancel must stop them all.
pub struct Job(HANDLE);

// SAFETY: a job handle may be used from any thread.
unsafe impl Send for Job {}
// SAFETY: as above; the calls made through it are thread-safe.
unsafe impl Sync for Job {}

impl Job {
    pub fn new() -> Option<Job> {
        // SAFETY: the structure is plain data of the size given.
        unsafe {
            let handle = CreateJobObjectW(None, PCWSTR::null()).ok()?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if set.is_err() {
                let _ = CloseHandle(handle);
                return None;
            }
            Some(Job(handle))
        }
    }

    pub fn add(&self, child: &std::process::Child) -> bool {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: the child's handle is valid while `child` lives.
        unsafe { AssignProcessToJobObject(self.0, HANDLE(child.as_raw_handle())).is_ok() }
    }

    /// Ends every process in the job.
    pub fn end(&self) {
        // SAFETY: the handle is ours and open.
        unsafe {
            let _ = TerminateJobObject(self.0, 1);
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: closed once; with KILL_ON_JOB_CLOSE this also ends what
        // is still in it.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// What the PC has, for the Setup page.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Adapter {
    pub name: String,
    pub vendor_id: u32,
    pub vram_bytes: u64,
}

impl Adapter {
    pub fn vendor(&self) -> &'static str {
        match self.vendor_id {
            0x10DE => "NVIDIA",
            0x1002 | 0x1022 => "AMD",
            0x8086 => "Intel",
            0x1414 => "Microsoft",
            0x5143 => "Qualcomm",
            _ => "unknown vendor",
        }
    }
}

/// The hardware adapters, in the system's order (what
/// `VICTORY_D3D12_ADAPTER`'s index counts).
pub fn adapters() -> Vec<Adapter> {
    let mut found = Vec::new();
    // SAFETY: plain DXGI enumeration; descriptions are copied out.
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else {
            return found;
        };
        let mut n = 0;
        while let Ok(adapter) = factory.EnumAdapters1(n) {
            n += 1;
            let Ok(desc) = adapter.GetDesc1() else {
                continue;
            };
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                continue;
            }
            let len = desc
                .Description
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(desc.Description.len());
            found.push(Adapter {
                name: String::from_utf16_lossy(&desc.Description[..len])
                    .trim()
                    .to_owned(),
                vendor_id: desc.VendorId,
                vram_bytes: desc.DedicatedVideoMemory as u64,
            });
        }
    }
    found
}

/// The running processes whose program file is one of `names` (compared
/// without case): (process id, name).
pub fn processes_named(names: &[&str]) -> Vec<(u32, String)> {
    let mut found = Vec::new();
    // SAFETY: a snapshot walked with a correctly sized entry, then closed.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return found;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
            if names.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                found.push((entry.th32ProcessID, name));
            }
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }
    found
}

/// A message in a window of its own, for what happens before (or instead
/// of) the launcher's window.
pub fn message_box(title: &str, text: &str) {
    // SAFETY: both strings outlive the call.
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from(title),
            MB_OK | MB_ICONERROR,
        );
    }
}

/// (Installed memory, the commit limit: memory plus page file as it is now.)
pub fn memory() -> Option<(u64, u64)> {
    let mut status = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: the structure has its length set.
    unsafe { GlobalMemoryStatusEx(&mut status).ok()? };
    Some((status.ullTotalPhys, status.ullTotalPageFile))
}

/// Free bytes on the drive of `path` (which need not exist yet).
pub fn free_disk(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|p| p.is_dir())?;
    let mut free = 0u64;
    // SAFETY: the string outlives the call; `free` is a valid out pointer.
    unsafe {
        GetDiskFreeSpaceExW(
            &HSTRING::from(existing.as_os_str()),
            Some(&mut free),
            None,
            None,
        )
        .ok()?
    };
    Some(free)
}
