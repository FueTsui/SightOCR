//! Thin Win32 wrappers used by setup: registry, processes, elevation, shell
//! folders, shortcuts and dialogs.

use std::ffi::{c_void, OsStr, OsString};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_CANCELLED, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS,
    ERROR_SUCCESS, HANDLE, HWND, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegEnumValueW, RegOpenKeyExW,
    RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_ALL_ACCESS,
    KEY_READ, REG_DWORD, REG_EXPAND_SZ, REG_OPTION_NON_VOLATILE, REG_SZ,
};

pub fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(Some(0)).collect()
}

/// Case-insensitive comparison of normalised Windows paths.
pub fn same_path(a: &Path, b: &Path) -> bool {
    normalize(a) == normalize(b)
}

pub fn normalize(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\");
    let text = text
        .strip_prefix(r"\\?\UNC\")
        .map(|rest| format!(r"\\{rest}"))
        .unwrap_or_else(|| text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned());
    let trimmed = text.trim_end_matches('\\');
    let text = if trimmed.ends_with(':') {
        text.as_str()
    } else {
        trimmed
    };
    text.to_lowercase()
}

/// A path without the `\\?\` prefix that `canonicalize` adds.
pub fn plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

// ---------------------------------------------------------------- registry

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hive {
    CurrentUser,
    LocalMachine,
}

impl Hive {
    fn handle(self) -> HKEY {
        match self {
            Hive::CurrentUser => HKEY_CURRENT_USER,
            Hive::LocalMachine => HKEY_LOCAL_MACHINE,
        }
    }
}

pub struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: The key was opened by this wrapper and is closed exactly once.
        unsafe { RegCloseKey(self.0) };
    }
}

fn status(code: u32) -> io::Result<()> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code as i32))
    }
}

/// Open an existing key; `None` when it does not exist.
pub fn open_key(hive: Hive, path: &str, view: u32, write: bool) -> io::Result<Option<Key>> {
    let path = wide(path);
    let mut key = null_mut();
    let access = if write { KEY_ALL_ACCESS } else { KEY_READ };
    // SAFETY: The path is NUL-terminated and `key` is valid storage for the result.
    let code = unsafe { RegOpenKeyExW(hive.handle(), path.as_ptr(), 0, access | view, &mut key) };
    if code == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    status(code)?;
    Ok(Some(Key(key)))
}

pub fn create_key(hive: Hive, path: &str, view: u32) -> io::Result<Key> {
    let path = wide(path);
    let mut key = null_mut();
    // SAFETY: Valid NUL-terminated path, default security and valid out-pointers.
    let code = unsafe {
        RegCreateKeyExW(
            hive.handle(),
            path.as_ptr(),
            0,
            null(),
            REG_OPTION_NON_VOLATILE,
            KEY_ALL_ACCESS | view,
            null(),
            &mut key,
            null_mut(),
        )
    };
    status(code)?;
    Ok(Key(key))
}

impl Key {
    pub fn get_string(&self, name: &str) -> Option<String> {
        let name = wide(name);
        let mut buffer = vec![0u16; 512];
        loop {
            let mut kind = 0;
            let mut bytes = (buffer.len() * 2) as u32;
            // SAFETY: `buffer` is writable for `bytes` bytes; other pointers are valid.
            let code = unsafe {
                RegQueryValueExW(
                    self.0,
                    name.as_ptr(),
                    null(),
                    &mut kind,
                    buffer.as_mut_ptr().cast(),
                    &mut bytes,
                )
            };
            if code == ERROR_MORE_DATA && buffer.len() < 32768 {
                buffer.resize(buffer.len() * 4, 0);
                continue;
            }
            if code != ERROR_SUCCESS || (kind != REG_SZ && kind != REG_EXPAND_SZ) {
                return None;
            }
            let mut units = &buffer[..(bytes as usize / 2).min(buffer.len())];
            while let [rest @ .., 0] = units {
                units = rest;
            }
            return Some(String::from_utf16_lossy(units));
        }
    }

    pub fn set_string(&self, name: &str, value: &str) -> io::Result<()> {
        let name = wide(name);
        let value = wide(value);
        // SAFETY: Both strings are NUL-terminated; the byte count includes the terminator.
        status(unsafe {
            RegSetValueExW(
                self.0,
                name.as_ptr(),
                0,
                REG_SZ,
                value.as_ptr().cast(),
                (value.len() * 2) as u32,
            )
        })
    }

    pub fn set_dword(&self, name: &str, value: u32) -> io::Result<()> {
        let name = wide(name);
        let bytes = value.to_le_bytes();
        // SAFETY: The name is NUL-terminated and the data is four readable bytes.
        status(unsafe { RegSetValueExW(self.0, name.as_ptr(), 0, REG_DWORD, bytes.as_ptr(), 4) })
    }

    /// Delete a value; a missing value is not an error.
    pub fn delete_value(&self, name: &str) -> io::Result<()> {
        let name = wide(name);
        // SAFETY: The name is NUL-terminated and the key is open.
        let code = unsafe { RegDeleteValueW(self.0, name.as_ptr()) };
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        status(code)
    }

    pub fn value_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for index in 0.. {
            let mut buffer = [0u16; 16384];
            let mut length = buffer.len() as u32;
            // SAFETY: `buffer` holds `length` UTF-16 units; data is not requested.
            let code = unsafe {
                RegEnumValueW(
                    self.0,
                    index,
                    buffer.as_mut_ptr(),
                    &mut length,
                    null(),
                    null_mut(),
                    null_mut(),
                    null_mut(),
                )
            };
            if code == ERROR_NO_MORE_ITEMS || code != ERROR_SUCCESS {
                break;
            }
            names.push(String::from_utf16_lossy(&buffer[..length as usize]));
        }
        names
    }

    /// Delete a subkey with everything below it; a missing subkey is not an error.
    pub fn delete_tree(&self, name: &str) -> io::Result<()> {
        let name = wide(name);
        // SAFETY: The subkey name is NUL-terminated and the parent key is open.
        let code = unsafe { RegDeleteTreeW(self.0, name.as_ptr()) };
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        status(code)
    }
}

// --------------------------------------------------------------- processes

pub struct Process {
    pub pid: u32,
    pub path: PathBuf,
    handle: HANDLE,
}

// SAFETY: A process handle may be used and closed from any thread.
unsafe impl Send for Process {}

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: The handle is owned by this value and closed exactly once.
        unsafe { CloseHandle(self.handle) };
    }
}

fn image_path(handle: HANDLE) -> Option<PathBuf> {
    use windows_sys::Win32::System::Threading::QueryFullProcessImageNameW;
    let mut buffer = vec![0u16; 32768];
    let mut length = buffer.len() as u32;
    // SAFETY: `buffer` is writable for `length` UTF-16 units.
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length) };
    (ok != 0).then(|| PathBuf::from(OsString::from_wide(&buffer[..length as usize])))
}

impl Process {
    /// Running processes whose executable matches `wanted`.
    pub fn find(wanted: impl Fn(&Path) -> bool) -> io::Result<Vec<Process>> {
        use windows_sys::Win32::System::ProcessStatus::K32EnumProcesses;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        };
        let mut ids = vec![0u32; 8192];
        let mut bytes = 0;
        // SAFETY: `ids` is writable for the stated number of bytes.
        let ok = unsafe { K32EnumProcesses(ids.as_mut_ptr(), (ids.len() * 4) as u32, &mut bytes) };
        if ok == 0 || bytes as usize >= ids.len() * 4 {
            return Err(io::Error::other("无法检查正在运行的程序"));
        }
        let mut found = Vec::new();
        for &pid in &ids[..bytes as usize / 4] {
            if pid == 0 || pid == std::process::id() {
                continue;
            }
            // SAFETY: Query/synchronize access only; a null handle is checked below.
            let handle = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                    0,
                    pid,
                )
            };
            if handle.is_null() {
                continue;
            }
            let path = image_path(handle).unwrap_or_default();
            // Dropping a process that is not wanted closes its handle.
            let process = Process { pid, path, handle };
            if !process.path.as_os_str().is_empty() && wanted(&process.path) && !process.exited() {
                found.push(process);
            }
        }
        Ok(found)
    }

    pub fn exited(&self) -> bool {
        self.wait(0)
    }

    /// Wait up to `milliseconds`; true once the process has exited.
    pub fn wait(&self, milliseconds: u32) -> bool {
        use windows_sys::Win32::System::Threading::WaitForSingleObject;
        // SAFETY: The handle is open with SYNCHRONIZE access.
        unsafe { WaitForSingleObject(self.handle, milliseconds) == WAIT_OBJECT_0 }
    }

    /// Ask SightOCR's main window to exit, the same way its tray menu does.
    pub fn request_exit(&self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            FindWindowExW, GetPropW, GetWindowThreadProcessId, PostMessageW, RegisterWindowMessageW,
        };
        let message_name = wide(sightocr::platform::MAIN_EXIT_MESSAGE);
        let property = wide(sightocr::platform::MAIN_EXIT_PROPERTY);
        // SAFETY: The name is NUL-terminated.
        let message = unsafe { RegisterWindowMessageW(message_name.as_ptr()) };
        if message == 0 {
            return;
        }
        let mut window: HWND = null_mut();
        loop {
            // SAFETY: Enumerates top-level windows; all string arguments are null.
            window = unsafe { FindWindowExW(null_mut(), window, null(), null()) };
            if window.is_null() {
                break;
            }
            let mut owner = 0;
            // SAFETY: `window` came from FindWindowExW and `owner` is valid storage.
            unsafe { GetWindowThreadProcessId(window, &mut owner) };
            // SAFETY: GetPropW only reads the property; its value is never dereferenced.
            if owner == self.pid && !unsafe { GetPropW(window, property.as_ptr()) }.is_null() {
                // SAFETY: Posts a scalar message to the verified window.
                unsafe { PostMessageW(window, message, 0, 0) };
            }
        }
    }

    /// End the process, after checking the PID still belongs to it.
    pub fn terminate(&self) {
        use windows_sys::Win32::System::Threading::{
            OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
        };
        if self.exited() {
            return;
        }
        // SAFETY: Opens a second handle; it is checked and closed below.
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                0,
                self.pid,
            )
        };
        if handle.is_null() {
            return;
        }
        // The original handle keeps the PID from being reused while it is open.
        if image_path(handle).is_some_and(|path| same_path(&path, &self.path)) && !self.exited() {
            // SAFETY: The handle has terminate access to the verified process.
            unsafe { TerminateProcess(handle, 0) };
        }
        // SAFETY: Closes the second handle exactly once.
        unsafe { CloseHandle(handle) };
    }
}

// --------------------------------------------------------------- elevation

pub fn is_elevated() -> bool {
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut token = null_mut();
    // SAFETY: The current-process pseudo handle is always valid.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return false;
    }
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut length = 0;
    // SAFETY: `elevation` is writable for its size; the token is open with query access.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut length,
        )
    };
    // SAFETY: Closes the token handle exactly once.
    unsafe { CloseHandle(token) };
    ok != 0 && elevation.TokenIsElevated != 0
}

/// Quote one argument for the Windows command-line parser.
pub fn quote(argument: &OsStr) -> OsString {
    let text: Vec<u16> = argument.encode_wide().collect();
    let needs_quotes = text.is_empty()
        || text
            .iter()
            .any(|&unit| unit == b' ' as u16 || unit == b'\t' as u16 || unit == b'"' as u16);
    if !needs_quotes {
        return argument.to_owned();
    }
    let mut quoted = vec![b'"' as u16];
    let mut backslashes = 0;
    for &unit in &text {
        if unit == b'\\' as u16 {
            backslashes += 1;
            continue;
        }
        let repeat = if unit == b'"' as u16 {
            backslashes * 2 + 1
        } else {
            backslashes
        };
        quoted.extend(std::iter::repeat_n(b'\\' as u16, repeat));
        quoted.push(unit);
        backslashes = 0;
    }
    quoted.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2));
    quoted.push(b'"' as u16);
    OsString::from_wide(&quoted)
}

pub enum ElevationError {
    Cancelled,
    Failed(io::Error),
}

/// Run `exe` with administrator rights (Windows asks the user) and wait for its exit code.
pub fn run_elevated(exe: &Path, arguments: &[OsString], show: bool) -> Result<i32, ElevationError> {
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, WaitForSingleObject, INFINITE,
    };
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNORMAL};
    let mut parameters = OsString::new();
    for (index, argument) in arguments.iter().enumerate() {
        if index > 0 {
            parameters.push(" ");
        }
        parameters.push(quote(argument));
    }
    let file = wide(exe);
    let parameters = wide(parameters);
    let verb = wide("runas");
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: parameters.as_ptr(),
        nShow: if show { SW_SHOWNORMAL } else { SW_HIDE },
        // SAFETY: The remaining optional fields accept zero/null.
        ..unsafe { std::mem::zeroed() }
    };
    // SAFETY: All strings and the structure stay alive for the call.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        let error = io::Error::last_os_error();
        return Err(if error.raw_os_error() == Some(ERROR_CANCELLED as i32) {
            ElevationError::Cancelled
        } else {
            ElevationError::Failed(error)
        });
    }
    if info.hProcess.is_null() {
        return Err(ElevationError::Failed(io::Error::other("无法等待安装程序")));
    }
    let mut code = 0;
    // SAFETY: ShellExecuteExW returned an owned process handle; wait, read and close it once.
    let ok = unsafe {
        let ok = WaitForSingleObject(info.hProcess, INFINITE) == WAIT_OBJECT_0
            && GetExitCodeProcess(info.hProcess, &mut code) != 0;
        CloseHandle(info.hProcess);
        ok
    };
    if !ok {
        return Err(ElevationError::Failed(io::Error::other(
            "无法读取安装程序的结果",
        )));
    }
    Ok(code as i32)
}

// ------------------------------------------------------------ shell folders

pub fn known_folder(id: &GUID) -> Option<PathBuf> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::SHGetKnownFolderPath;
    let mut path = null_mut();
    // SAFETY: `id` is a valid GUID; the returned string is freed below.
    let result = unsafe { SHGetKnownFolderPath(id, 0, null_mut(), &mut path) };
    if result < 0 || path.is_null() {
        return None;
    }
    // SAFETY: SHGetKnownFolderPath returned a NUL-terminated string.
    let length = unsafe { (0..).take_while(|&i| *path.add(i) != 0).count() };
    // SAFETY: `length` units are readable; the buffer is then freed exactly once.
    let folder = unsafe {
        let folder = OsString::from_wide(std::slice::from_raw_parts(path, length));
        CoTaskMemFree(path as *const c_void);
        folder
    };
    Some(PathBuf::from(folder))
}

// ---------------------------------------------------------------- shortcuts

type HResult = i32;

#[repr(C)]
struct UnknownVtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HResult,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

#[repr(C)]
struct ShellLinkVtbl {
    base: UnknownVtbl,
    get_path: unsafe extern "system" fn(*mut c_void, *mut u16, i32, *mut c_void, u32) -> HResult,
    get_id_list: usize,
    set_id_list: usize,
    get_description: usize,
    set_description: unsafe extern "system" fn(*mut c_void, *const u16) -> HResult,
    get_working_directory: usize,
    set_working_directory: unsafe extern "system" fn(*mut c_void, *const u16) -> HResult,
    get_arguments: usize,
    set_arguments: usize,
    get_hotkey: usize,
    set_hotkey: usize,
    get_show_cmd: usize,
    set_show_cmd: usize,
    get_icon_location: usize,
    set_icon_location: unsafe extern "system" fn(*mut c_void, *const u16, i32) -> HResult,
    set_relative_path: usize,
    resolve: usize,
    set_path: unsafe extern "system" fn(*mut c_void, *const u16) -> HResult,
}

#[repr(C)]
struct PersistFileVtbl {
    base: UnknownVtbl,
    get_class_id: usize,
    is_dirty: usize,
    load: unsafe extern "system" fn(*mut c_void, *const u16, u32) -> HResult,
    save: unsafe extern "system" fn(*mut c_void, *const u16, i32) -> HResult,
    save_completed: usize,
    get_cur_file: usize,
}

const CLSID_SHELL_LINK: GUID = GUID::from_u128(0x00021401_0000_0000_c000_000000000046);
const IID_SHELL_LINK_W: GUID = GUID::from_u128(0x000214f9_0000_0000_c000_000000000046);
const IID_PERSIST_FILE: GUID = GUID::from_u128(0x0000010b_0000_0000_c000_000000000046);

/// An owned COM interface pointer.
struct Com<V> {
    pointer: *mut c_void,
    _vtable: std::marker::PhantomData<V>,
}

impl<V> Com<V> {
    fn vtable(&self) -> &V {
        // SAFETY: A COM interface pointer points to a pointer to its vtable of type V.
        unsafe { &**(self.pointer as *const *const V) }
    }

    fn query<W>(&self, iid: &GUID) -> Option<Com<W>> {
        let mut pointer = null_mut();
        // SAFETY: Every COM vtable starts with IUnknown.
        let unknown = unsafe { &**(self.pointer as *const *const UnknownVtbl) };
        // SAFETY: Valid interface pointer, IID and out-pointer.
        let result = unsafe { (unknown.query_interface)(self.pointer, iid, &mut pointer) };
        (result >= 0 && !pointer.is_null()).then_some(Com {
            pointer,
            _vtable: std::marker::PhantomData,
        })
    }
}

impl<V> Drop for Com<V> {
    fn drop(&mut self) {
        // SAFETY: Every COM vtable starts with IUnknown; this releases our single reference.
        unsafe {
            let unknown = &**(self.pointer as *const *const UnknownVtbl);
            (unknown.release)(self.pointer);
        }
    }
}

/// Initialises COM on this thread for the guard's lifetime.
struct ComScope(bool);

impl ComScope {
    fn enter() -> Self {
        use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        // SAFETY: Standard per-thread initialisation, balanced in Drop when it succeeded.
        let result = unsafe { CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32) };
        Self(result >= 0)
    }
}

impl Drop for ComScope {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: Balances the successful CoInitializeEx on this thread.
            unsafe { windows_sys::Win32::System::Com::CoUninitialize() };
        }
    }
}

fn shell_link() -> io::Result<Com<ShellLinkVtbl>> {
    use windows_sys::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    let mut pointer = null_mut();
    // SAFETY: Valid CLSID/IID and out-pointer.
    let result = unsafe {
        CoCreateInstance(
            &CLSID_SHELL_LINK,
            null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_SHELL_LINK_W,
            &mut pointer,
        )
    };
    if result < 0 || pointer.is_null() {
        return Err(io::Error::other(format!(
            "无法创建快捷方式（0x{result:08x}）"
        )));
    }
    Ok(Com {
        pointer,
        _vtable: std::marker::PhantomData,
    })
}

fn check(result: HResult, what: &str) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::other(format!("{what}（0x{result:08x}）")))
    } else {
        Ok(())
    }
}

pub fn create_shortcut(lnk: &Path, target: &Path, description: &str) -> io::Result<()> {
    let _com = ComScope::enter();
    let link = shell_link()?;
    let target_text = wide(target);
    let directory = wide(target.parent().unwrap_or(target));
    let description = wide(description);
    let vtable = link.vtable();
    // SAFETY: All strings are NUL-terminated and outlive the calls.
    unsafe {
        check(
            (vtable.set_path)(link.pointer, target_text.as_ptr()),
            "无法设置快捷方式目标",
        )?;
        check(
            (vtable.set_working_directory)(link.pointer, directory.as_ptr()),
            "无法设置快捷方式目录",
        )?;
        check(
            (vtable.set_icon_location)(link.pointer, target_text.as_ptr(), 0),
            "无法设置快捷方式图标",
        )?;
        check(
            (vtable.set_description)(link.pointer, description.as_ptr()),
            "无法设置快捷方式说明",
        )?;
    }
    let file: Com<PersistFileVtbl> = link
        .query(&IID_PERSIST_FILE)
        .ok_or_else(|| io::Error::other("无法保存快捷方式"))?;
    if let Some(parent) = lnk.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let path = wide(lnk);
    // SAFETY: The path is NUL-terminated; TRUE makes it the current file.
    check(
        unsafe { (file.vtable().save)(file.pointer, path.as_ptr(), 1) },
        "无法保存快捷方式",
    )
}

/// The program a shortcut opens, if it can be read.
pub fn shortcut_target(lnk: &Path) -> Option<PathBuf> {
    if !lnk.is_file() {
        return None;
    }
    let _com = ComScope::enter();
    let link = shell_link().ok()?;
    let file: Com<PersistFileVtbl> = link.query(&IID_PERSIST_FILE)?;
    let path = wide(lnk);
    // SAFETY: NUL-terminated path, read-only mode (STGM_READ = 0).
    if unsafe { (file.vtable().load)(file.pointer, path.as_ptr(), 0) } < 0 {
        return None;
    }
    let mut buffer = vec![0u16; 32768];
    // SAFETY: `buffer` is writable for the stated length; find data is not requested.
    let result = unsafe {
        (link.vtable().get_path)(
            link.pointer,
            buffer.as_mut_ptr(),
            buffer.len() as i32,
            null_mut(),
            0,
        )
    };
    if result != 0 {
        return None;
    }
    let length = buffer.iter().position(|&unit| unit == 0).unwrap_or(0);
    (length > 0).then(|| PathBuf::from(OsString::from_wide(&buffer[..length])))
}

// ------------------------------------------------------------------ dialogs

const TITLE: &str = "SightOCR 安装";

pub fn message_box(text: &str, error: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK,
    };
    let text = wide(text);
    let title = wide(TITLE);
    let icon = if error {
        MB_ICONERROR
    } else {
        MB_ICONINFORMATION
    };
    // SAFETY: Both strings are NUL-terminated and live for the call.
    unsafe { MessageBoxW(null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | icon) };
}

/// OK/Cancel question; true for OK.
pub fn confirm(text: &str) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDOK, MB_ICONWARNING, MB_OKCANCEL,
    };
    let text = wide(text);
    let title = wide(TITLE);
    // SAFETY: Both strings are NUL-terminated and live for the call.
    unsafe {
        MessageBoxW(
            null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OKCANCEL | MB_ICONWARNING,
        ) == IDOK
    }
}

/// Let the user pick a folder. `owner` is the setup window (or 0).
pub fn browse_folder(owner: isize, title: &str) -> Option<PathBuf> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{
        SHBrowseForFolderW, SHGetPathFromIDListW, BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS,
        BROWSEINFOW,
    };
    let _com = ComScope::enter();
    let title = wide(title);
    let mut display = vec![0u16; 260];
    let info = BROWSEINFOW {
        hwndOwner: owner as HWND,
        pidlRoot: null_mut(),
        pszDisplayName: display.as_mut_ptr(),
        lpszTitle: title.as_ptr(),
        ulFlags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE,
        lpfn: None,
        lParam: 0,
        iImage: 0,
    };
    // SAFETY: `info` and its buffers stay alive for the modal dialog.
    let list = unsafe { SHBrowseForFolderW(&info) };
    if list.is_null() {
        return None;
    }
    let mut buffer = vec![0u16; 32768];
    // SAFETY: The ID list came from the dialog; the buffer is large enough for any path.
    let ok = unsafe { SHGetPathFromIDListW(list, buffer.as_mut_ptr()) };
    // SAFETY: Frees the ID list returned by SHBrowseForFolderW exactly once.
    unsafe { CoTaskMemFree(list as *const c_void) };
    if ok == 0 {
        return None;
    }
    let length = buffer.iter().position(|&unit| unit == 0).unwrap_or(0);
    Some(PathBuf::from(OsString::from_wide(&buffer[..length])))
}

// -------------------------------------------------------------------- files

/// Delete a file (or empty folder) when Windows next starts.
pub fn delete_on_reboot(path: &Path) {
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_DELAY_UNTIL_REBOOT};
    let path = wide(path);
    // SAFETY: The path is NUL-terminated; a null destination means delete.
    unsafe { MoveFileExW(path.as_ptr(), null(), MOVEFILE_DELAY_UNTIL_REBOOT) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(argument: &str) -> String {
        // The quoted form must parse back to the same single argument.
        let quoted = quote(OsStr::new(argument)).into_string().unwrap();
        let line = format!("setup.exe {quoted}");
        let wide_line = wide(&line);
        let mut count = 0;
        // SAFETY: Valid NUL-terminated command line; the result is freed below.
        let parsed = unsafe {
            windows_sys::Win32::UI::Shell::CommandLineToArgvW(wide_line.as_ptr(), &mut count)
        };
        assert!(!parsed.is_null());
        assert_eq!(count, 2, "{line}");
        // SAFETY: Index 1 exists; each entry is NUL-terminated; LocalFree releases the block.
        unsafe {
            let entry = *parsed.add(1);
            let length = (0..).take_while(|&i| *entry.add(i) != 0).count();
            let value = String::from_utf16_lossy(std::slice::from_raw_parts(entry, length));
            windows_sys::Win32::Foundation::LocalFree(parsed as _);
            value
        }
    }

    #[test]
    fn quoting_survives_the_windows_parser() {
        for argument in [
            "/VERYSILENT",
            r"/DIR=C:\Program Files\SightOCR",
            r"/DIR=C:\Program Files\SightOCR\",
            r#"/LOG=C:\a "b"\c.log"#,
            r"--result-file=C:\Users\测试 用户\AppData\Local\Temp\SightOCR-Setup-1.result",
            "",
        ] {
            assert_eq!(round_trip(argument), argument);
        }
    }

    #[test]
    fn paths_compare_case_insensitively_without_prefixes() {
        assert!(same_path(
            Path::new(r"\\?\C:\Program Files\SightOCR\"),
            Path::new(r"c:/program files/sightocr")
        ));
        assert!(!same_path(
            Path::new(r"C:\a\SightOCR"),
            Path::new(r"C:\b\SightOCR")
        ));
        assert_eq!(normalize(Path::new(r"D:\")), r"d:\");
    }
}
