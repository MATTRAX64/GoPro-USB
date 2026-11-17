use super::*;
use std::{ffi::c_void, path::PathBuf};

#[derive(serde::Serialize, serde::Deserialize)]
struct Owner {
    pid: u32,
    ip: Ipv4Addr,
    slot: Option<u32>,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
    fn WaitForSingleObject(handle: isize, timeout: u32) -> u32;
    fn CreateMutexW(attributes: *const c_void, owner: i32, name: *const u16) -> isize;
    fn ReleaseMutex(handle: isize) -> i32;
    fn CloseHandle(handle: isize) -> i32;
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> isize;
    fn Process32FirstW(snapshot: isize, entry: *mut ProcessEntry) -> i32;
    fn Process32NextW(snapshot: isize, entry: *mut ProcessEntry) -> i32;
    fn TerminateProcess(process: isize, code: u32) -> i32;
}
#[repr(C)]
struct ProcessEntry {
    size: u32,
    usage: u32,
    pid: u32,
    heap: usize,
    module: u32,
    threads: u32,
    parent: u32,
    priority: i32,
    flags: u32,
    name: [u16; 260],
}

pub struct Guard(isize);
impl Drop for Guard {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}
pub fn lock() -> Result<Guard, String> {
    let name: Vec<u16> = "Local\\GoPro USB.CameraOwnership\0"
        .encode_utf16()
        .collect();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle == 0 {
        return Err("Verrou de capture indisponible".into());
    }
    match unsafe { WaitForSingleObject(handle, 10000) } {
        0 | 128 => Ok(Guard(handle)),
        _ => {
            unsafe {
                CloseHandle(handle);
            }
            Err("La capture est occupée".into())
        }
    }
}
fn directory() -> Option<PathBuf> {
    Some(config_path()?.parent()?.join("Capture"))
}
pub fn claim(ip: Ipv4Addr, command: &str) -> Result<(), String> {
    let folder = directory().ok_or("AppData indisponible")?;
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let port = command
        .split('&')
        .find_map(|part| part.strip_prefix("port=")?.parse::<u32>().ok());
    let slot = port
        .and_then(|port| port.checked_sub(8554))
        .filter(|slot| *slot < 60);
    let owner = Owner {
        pid: std::process::id(),
        ip,
        slot,
    };
    let path = folder.join(format!("{ip}.json"));
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, serde_json::to_vec(&owner).unwrap())
        .and_then(|_| std::fs::rename(temp, path))
        .map_err(|e| e.to_string())
}
pub fn launch() -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    hidden(executable.to_str().ok_or("Chemin invalide")?)
        .args(["--watch-parent", &std::process::id().to_string()])
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}
fn stop_children(pid: u32) {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(2, 0);
        if snapshot == -1 {
            return;
        }
        let mut entry: ProcessEntry = std::mem::zeroed();
        entry.size = std::mem::size_of::<ProcessEntry>() as u32;
        let mut valid = Process32FirstW(snapshot, &mut entry) != 0;
        while valid {
            let name = String::from_utf16_lossy(&entry.name)
                .trim_end_matches('\0')
                .to_owned();
            if entry.parent == pid && name.eq_ignore_ascii_case("ffmpeg.exe") {
                let process = OpenProcess(1, 0, entry.pid);
                if process != 0 {
                    TerminateProcess(process, 0);
                    CloseHandle(process);
                }
            }
            valid = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
    }
}
pub fn helper() -> Option<i32> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some("--watch-parent") {
        return None;
    }
    let Some(pid) = args.get(2).and_then(|p| p.parse::<u32>().ok()) else {
        return Some(1);
    };
    let process = unsafe { OpenProcess(0x100000, 0, pid) };
    if process != 0 {
        unsafe {
            WaitForSingleObject(process, u32::MAX);
        }
    }
    stop_children(pid);
    if let Ok(_guard) = lock() {
        if let Some(folder) = directory() {
            if let Ok(files) = std::fs::read_dir(folder) {
                for file in files.flatten() {
                    let path = file.path();
                    let owner = std::fs::read(&path)
                        .ok()
                        .and_then(|bytes| serde_json::from_slice::<Owner>(&bytes).ok());
                    let Some(owner) = owner.filter(|o| o.pid == pid) else {
                        continue;
                    };
                    let _ = webcam_get(owner.ip, "stop");
                    if let Some(slot) = owner.slot {
                        let _ = vcam::Driver::register(slot, "", false);
                    }
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    }
    if process != 0 {
        unsafe {
            CloseHandle(process);
        }
    }
    Some(0)
}
