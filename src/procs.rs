//! Process Killer: list running processes, terminate by PID.

use std::collections::HashMap;
use windows::Win32::Foundation::*;
use windows::Win32::System::Diagnostics::ToolHelp::*;
use windows::Win32::System::ProcessStatus::*;
use windows::Win32::System::Threading::*;
use windows::core::PWSTR;

pub struct Proc {
    pub name: String,
    pub pids: Vec<u32>,
    pub path: Option<String>,
    /// Working set of all instances, bytes.
    pub mem: u64,
}

/// Running processes grouped by executable name (Chrome's 30 processes are one entry).
pub fn list() -> Vec<Proc> {
    let me = std::process::id();
    let mut by_name: HashMap<String, Proc> = HashMap::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return Vec::new() };
        let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let pid = e.th32ProcessID;
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            let name = String::from_utf16_lossy(&e.szExeFile[..len]);
            if pid > 4 && pid != me && !is_system(&name) {
                let (path, mem) = details(pid);
                let p = by_name.entry(name.to_lowercase()).or_insert_with(|| Proc { name, pids: Vec::new(), path: None, mem: 0 });
                p.pids.push(pid);
                p.mem += mem;
                if p.path.is_none() {
                    p.path = path;
                }
            }
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    let mut v: Vec<Proc> = by_name.into_values().collect();
    v.sort_by(|a, b| b.mem.cmp(&a.mem));
    v
}

/// Core OS processes that should never be offered for killing.
fn is_system(name: &str) -> bool {
    const SYSTEM: &[&str] = &[
        "system", "registry", "smss.exe", "csrss.exe", "wininit.exe", "winlogon.exe", "services.exe", "lsass.exe",
        "svchost.exe", "fontdrvhost.exe", "dwm.exe", "memory compression", "lsaiso.exe", "secure system",
    ];
    SYSTEM.contains(&name.to_lowercase().as_str())
}

fn details(pid: u32) -> (Option<String>, u64) {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return (None, 0) };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let path = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
            .ok()
            .map(|_| String::from_utf16_lossy(&buf[..len as usize]));
        let mut pmc = PROCESS_MEMORY_COUNTERS::default();
        let mem = if GetProcessMemoryInfo(h, &mut pmc, size_of::<PROCESS_MEMORY_COUNTERS>() as u32).is_ok() {
            pmc.WorkingSetSize as u64
        } else {
            0
        };
        let _ = CloseHandle(h);
        (path, mem)
    }
}

/// Terminates the processes; returns how many were killed.
pub fn kill(pids: &[u32]) -> usize {
    pids.iter()
        .filter(|&&pid| unsafe {
            match OpenProcess(PROCESS_TERMINATE, false, pid) {
                Ok(h) => {
                    let ok = TerminateProcess(h, 1).is_ok();
                    let _ = CloseHandle(h);
                    ok
                }
                Err(_) => false,
            }
        })
        .count()
}

pub fn format_mem(bytes: u64) -> String {
    let mb = bytes as f64 / 1048576.0;
    if mb >= 1024.0 { format!("{:.1} ГБ", mb / 1024.0) } else { format!("{:.0} МБ", mb) }
}
