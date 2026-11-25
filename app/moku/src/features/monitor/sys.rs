//! Win32 samplers for the Monitor panel. Each function is a few cheap syscalls; PDH (GPU) lives
//! in a worker thread ([`GpuSampler`]).

use std::sync::mpsc::{Receiver, Sender, channel};

use windows::Win32::Foundation::FILETIME;
use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA, PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW,
};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;
use windows::core::{HSTRING, PCWSTR};

use super::{CpuTimes, IfRow, NetCounters, gpu_percent_from_instances, net_totals};
use crate::app::Ui;

fn ft(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

/// Cumulative CPU times (`GetSystemTimes`).
pub fn cpu_times() -> Option<CpuTimes> {
    let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).ok()? };
    Some(CpuTimes { idle: ft(idle), kernel: ft(kernel), user: ft(user) })
}

/// `(memory load %, bytes in use, total bytes)` from `GlobalMemoryStatusEx`.
pub fn memory() -> Option<(f32, u64, u64)> {
    let mut m = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    unsafe { GlobalMemoryStatusEx(&mut m).ok()? };
    if m.ullTotalPhys == 0 {
        return None;
    }
    let used = m.ullTotalPhys.saturating_sub(m.ullAvailPhys);
    Some((used as f32 / m.ullTotalPhys as f32 * 100.0, used, m.ullTotalPhys))
}

/// The system drive: `("C:", free bytes for this user, total bytes)`.
pub fn system_disk() -> Option<(String, u64, u64)> {
    let drive = std::env::var("SystemDrive").ok().filter(|d| d.len() == 2 && d.ends_with(':')).unwrap_or_else(|| "C:".into());
    let root = HSTRING::from(format!("{drive}\\"));
    let (mut free, mut total) = (0u64, 0u64);
    unsafe { GetDiskFreeSpaceExW(&root, Some(&mut free), Some(&mut total), None).ok()? };
    Some((drive, free, total))
}

/// Cumulative bytes over the physical, connected adapters (`GetIfTable2`).
pub fn net_counters() -> Option<NetCounters> {
    const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;
    unsafe {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if GetIfTable2(&mut table).0 != 0 || table.is_null() {
            return None;
        }
        let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
        let rows: Vec<IfRow> = rows
            .iter()
            .map(|r| IfRow {
                up: r.OperStatus == IfOperStatusUp,
                // MIB_IF_ROW2::InterfaceAndOperStatusFlags bit 0 = HardwareInterface.
                hardware: r.InterfaceAndOperStatusFlags._bitfield & 1 != 0,
                loopback: r.Type == IF_TYPE_SOFTWARE_LOOPBACK,
                rx: r.InOctets,
                tx: r.OutOctets,
            })
            .collect();
        FreeMibTable(table as *const _);
        Some(net_totals(&rows))
    }
}

// ---- GPU (PDH) ----------------------------------------------------------------------------------

/// Handle to the GPU worker thread. Dropping it ends the thread (and closes the PDH query).
pub struct GpuSampler {
    tx: Sender<()>,
}

impl GpuSampler {
    pub fn spawn() -> Option<GpuSampler> {
        let (tx, rx) = channel();
        std::thread::Builder::new().name("monitor-gpu".into()).spawn(move || worker(rx)).ok()?;
        Some(GpuSampler { tx })
    }

    /// Ask for a reading; the result arrives through `Ui::post` → `set_gpu`.
    pub fn request(&self) {
        let _ = self.tx.send(());
    }
}

struct GpuQuery {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
}

impl GpuQuery {
    fn open() -> Option<GpuQuery> {
        unsafe {
            let mut query = PDH_HQUERY::default();
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 {
                return None;
            }
            let mut counter = PDH_HCOUNTER::default();
            let path = HSTRING::from(r"\GPU Engine(*engtype_3D)\Utilization Percentage");
            if PdhAddEnglishCounterW(query, &path, 0, &mut counter) != 0 {
                PdhCloseQuery(query);
                return None;
            }
            // Rate counters need two collections; this is the baseline.
            PdhCollectQueryData(query);
            Some(GpuQuery { query, counter })
        }
    }

    /// Collect and return the 3D utilisation; `None` when PDH has no valid data this time.
    fn read(&self) -> Option<f32> {
        unsafe {
            if PdhCollectQueryData(self.query) != 0 {
                return None;
            }
            let (mut size, mut count) = (0u32, 0u32);
            let r = PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut size, &mut count, None);
            if r == 0 && size == 0 {
                return Some(0.0); // no process uses the GPU right now
            }
            if r != PDH_MORE_DATA {
                return None;
            }
            // u64 elements: the item struct needs 8-byte alignment.
            let mut buf = vec![0u64; (size as usize).div_ceil(8) + 1];
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut size, &mut count, Some(items)) != 0 {
                return None;
            }
            let items = std::slice::from_raw_parts(items, count as usize);
            let named: Vec<(String, f64)> = items
                .iter()
                // CStatus 0 = valid, 1 = valid (new data).
                .filter(|it| it.FmtValue.CStatus <= 1)
                .map(|it| (it.szName.to_string().unwrap_or_default(), it.FmtValue.Anonymous.doubleValue))
                .collect();
            Some(gpu_percent_from_instances(named.iter().map(|(n, v)| (n.as_str(), *v))))
        }
    }
}

impl Drop for GpuQuery {
    fn drop(&mut self) {
        unsafe {
            PdhCloseQuery(self.query);
        }
    }
}

fn worker(rx: Receiver<()>) {
    let q = GpuQuery::open();
    if q.is_none() {
        Ui::post(|app| super::set_gpu(app, None));
    }
    let mut procs = ProcSampler::default();
    // The first request only has the baseline sample to compare with: skip it, answer the rest.
    let mut first = true;
    while rx.recv().is_ok() {
        while rx.try_recv().is_ok() {}
        let top = procs.sample();
        if std::mem::take(&mut first) {
            continue;
        }
        if let Some(p) = q.as_ref().and_then(|q| q.read()) {
            Ui::post(move |app| super::set_gpu(app, Some(p)));
        }
        if let Some(top) = top {
            Ui::post(move |app| super::set_top(app, top));
        }
    }
}

// ---- top processes ------------------------------------------------------------------------------

/// CPU time per process between two samples (Toolhelp snapshot + `GetProcessTimes`). Only the
/// executable names are read — no window titles, no command lines. Runs on the worker thread,
/// only while the Monitor tab is visible.
#[derive(Default)]
struct ProcSampler {
    prev: Option<(std::collections::HashMap<u32, u64>, std::time::Instant)>,
}

impl ProcSampler {
    /// Top processes by CPU since the previous call (`None` on the first call).
    fn sample(&mut self) -> Option<Vec<(String, f32)>> {
        let now = std::time::Instant::now();
        let cur = processes();
        let times: std::collections::HashMap<u32, u64> = cur.iter().map(|(pid, _, t)| (*pid, *t)).collect();
        let prev = self.prev.replace((times, now));
        let (prev_times, at) = prev?;
        let elapsed = now.duration_since(at).as_nanos() as u64 / 100;
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as u32;
        Some(super::top_by_cpu(&prev_times, &cur, elapsed, cores, 3))
    }
}

/// `(pid, exe name without ".exe", kernel + user time in 100 ns)` of every process we may query.
fn processes() -> Vec<(u32, String, u64)> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
    use windows::Win32::System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    let mut out = Vec::with_capacity(256);
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let pid = e.th32ProcessID;
            if pid != 0 && let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                let (mut c, mut x, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
                if GetProcessTimes(h, &mut c, &mut x, &mut k, &mut u).is_ok() {
                    let len = e.szExeFile.iter().position(|&ch| ch == 0).unwrap_or(e.szExeFile.len());
                    let name = String::from_utf16_lossy(&e.szExeFile[..len]);
                    let name = name.strip_suffix(".exe").or_else(|| name.strip_suffix(".EXE")).unwrap_or(&name).to_string();
                    out.push((pid, name, ft(k) + ft(u)));
                }
                let _ = CloseHandle(h);
            }
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}
