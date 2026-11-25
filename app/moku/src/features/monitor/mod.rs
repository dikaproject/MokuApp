//! Feature `monitor` — Task 3 (track B).
//!
//! Monitor panel: CPU, GPU, RAM, disk and network with sparklines (`pratinjau/02-Island-Panel.png`,
//! "Monitor"). Off by default (`monitor.enabled`).
//!
//! Sampling happens at 1 Hz **only while the Monitor tab is visible**: everything runs from
//! `TabSpec::tick` (called once when the tab is shown, then every second, stopped when it is
//! hidden), and the state is reset on `TabHidden`, so a closed panel costs nothing. CPU, RAM, disk
//! and network are a handful of cheap syscalls on the UI thread. GPU utilisation (PDH) can take
//! tens of milliseconds when many processes use the GPU, so it runs on a short-lived worker
//! thread that exists only while the tab is visible. The same worker computes the "Teratas" row:
//! the busiest programs by CPU (Toolhelp snapshot + `GetProcessTimes`; executable names only,
//! never window titles).

mod sys;

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::app::{App, Redraw};
use crate::i18n::{self, tr};
use crate::registry::*;
use crate::ui::*;

/// Samples kept for each sparkline.
const HISTORY: usize = 40;

pub fn register(r: &mut Registry) {
    r.add_state(Monitor { gpu_available: true, ..Monitor::default() });
    r.add_tab(TabSpec {
        id: "monitor",
        order: order::MONITOR,
        icon: icons::PULSE,
        label: ("Monitor", "Monitor"),
        enabled: |c| c.monitor.enabled,
        build: panel,
        tick: Some((Duration::from_secs(1), on_tick)),
    });
    r.on_event(on_event);
}

// ---- pure calculations (tested) -----------------------------------------------------------------

/// Cumulative CPU times from `GetSystemTimes` (100 ns units; `kernel` includes `idle`).
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct CpuTimes {
    pub idle: u64,
    pub kernel: u64,
    pub user: u64,
}

/// CPU busy percentage (0–100) between two `GetSystemTimes` readings. 0 when no time elapsed or
/// the counters went backwards.
pub fn cpu_percent_from_times(prev: CpuTimes, cur: CpuTimes) -> f32 {
    let idle = cur.idle.saturating_sub(prev.idle);
    let total = cur.kernel.saturating_sub(prev.kernel) + cur.user.saturating_sub(prev.user);
    if total == 0 {
        return 0.0;
    }
    let busy = total.saturating_sub(idle);
    (busy as f64 / total as f64 * 100.0).clamp(0.0, 100.0) as f32
}

/// Cumulative received / sent bytes over the counted interfaces.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct NetCounters {
    pub rx: u64,
    pub tx: u64,
}

/// `(down, up)` in bytes per second over `secs`. A counter that went backwards (adapter reset)
/// counts as 0, never as a huge wrapped value.
pub fn net_rate_from_counters(prev: NetCounters, cur: NetCounters, secs: f64) -> (f64, f64) {
    if !secs.is_finite() || secs <= 0.0 {
        return (0.0, 0.0);
    }
    let rate = |p: u64, c: u64| if c >= p { (c - p) as f64 / secs } else { 0.0 };
    (rate(prev.rx, cur.rx), rate(prev.tx, cur.tx))
}

/// One row of `GetIfTable2`, reduced to what decides whether it is counted.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IfRow {
    pub up: bool,
    /// A physical adapter (not a filter / virtual module stacked on one: those repeat its traffic).
    pub hardware: bool,
    pub loopback: bool,
    pub rx: u64,
    pub tx: u64,
}

/// Sum of the interfaces that carry real traffic: up, not loopback, physical. When there is no
/// physical adapter at all (a VM with only virtual ones) every up, non-loopback row counts.
pub fn net_totals(rows: &[IfRow]) -> NetCounters {
    let sum = |keep: &dyn Fn(&IfRow) -> bool| {
        rows.iter().filter(|r| keep(r)).fold(NetCounters::default(), |a, r| NetCounters { rx: a.rx.saturating_add(r.rx), tx: a.tx.saturating_add(r.tx) })
    };
    if rows.iter().any(|r| r.up && !r.loopback && r.hardware) { sum(&|r| r.up && !r.loopback && r.hardware) } else { sum(&|r| r.up && !r.loopback) }
}

/// 3D-engine utilisation from the PDH `\GPU Engine(*engtype_3D)\Utilization Percentage`
/// instances. Instance names look like `pid_1234_luid_0x00000000_0x0000D2A5_phys_0_eng_0_engtype_3D`:
/// the processes using one engine add up, and the busiest engine is the GPU's load (what Task
/// Manager shows). Clamped to 0–100.
pub fn gpu_percent_from_instances<'a>(items: impl IntoIterator<Item = (&'a str, f64)>) -> f32 {
    let mut engines: Vec<(&str, f64)> = Vec::new();
    for (name, v) in items {
        if !v.is_finite() || v < 0.0 {
            continue;
        }
        let key = name.find("luid_").map(|i| &name[i..]).unwrap_or("");
        match engines.iter_mut().find(|e| e.0 == key) {
            Some(e) => e.1 += v,
            None => engines.push((key, v)),
        }
    }
    engines.iter().map(|e| e.1).fold(0.0, f64::max).clamp(0.0, 100.0) as f32
}

/// The `n` busiest programs between two process samples: CPU % of the whole machine, processes
/// of the same name added up (a browser's many processes count as one). `prev` = pid → CPU time,
/// `cur` = (pid, name, CPU time); times and `elapsed` in 100 ns. New or restarted pids count from 0.
pub fn top_by_cpu(prev: &std::collections::HashMap<u32, u64>, cur: &[(u32, String, u64)], elapsed: u64, cores: u32, n: usize) -> Vec<(String, f32)> {
    if elapsed == 0 || cores == 0 {
        return vec![];
    }
    let mut by_name: Vec<(String, u64)> = Vec::new();
    for (pid, name, t) in cur {
        let Some(p) = prev.get(pid) else { continue };
        let d = t.saturating_sub(*p);
        if d == 0 {
            continue;
        }
        match by_name.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
            Some(e) => e.1 += d,
            None => by_name.push((name.clone(), d)),
        }
    }
    by_name.sort_by(|a, b| b.1.cmp(&a.1));
    by_name
        .into_iter()
        .take(n)
        .map(|(name, d)| (name, (d as f64 / (elapsed as f64 * cores as f64) * 100.0).clamp(0.0, 100.0) as f32))
        .filter(|(_, p)| *p >= 0.1)
        .collect()
}

fn num(v: f64, decimals: usize, comma: bool) -> String {
    let s = format!("{v:.decimals$}");
    if comma { s.replace('.', ",") } else { s }
}

/// `12,4 MB/s`, `820 KB/s`, `0 B/s` (decimal comma when `comma`).
pub fn fmt_rate_with(bytes_per_sec: f64, comma: bool) -> String {
    let b = if bytes_per_sec.is_finite() { bytes_per_sec.max(0.0) } else { 0.0 };
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    if b < KB {
        format!("{} B/s", b.round() as u64)
    } else if b < MB {
        let v = b / KB;
        format!("{} KB/s", num(v, if v < 100.0 { 1 } else { 0 }, comma))
    } else if b < GB {
        format!("{} MB/s", num(b / MB, 1, comma))
    } else {
        format!("{} GB/s", num(b / GB, 2, comma))
    }
}

/// `6,2 GB`, `168 GB`, `1,8 TB`.
pub fn fmt_size_with(bytes: u64, comma: bool) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let g = bytes as f64 / GB;
    if g >= 1000.0 {
        format!("{} TB", num(g / 1024.0, 1, comma))
    } else if g >= 100.0 {
        format!("{} GB", num(g, 0, comma))
    } else {
        format!("{} GB", num(g, 1, comma))
    }
}

fn fmt_rate(bps: f64) -> String {
    fmt_rate_with(bps, !i18n::is_english())
}
fn fmt_size(bytes: u64) -> String {
    fmt_size_with(bytes, !i18n::is_english())
}

fn push_history(h: &mut VecDeque<f32>, v: f32) {
    if h.len() == HISTORY {
        h.pop_front();
    }
    h.push_back(v);
}

// ---- state --------------------------------------------------------------------------------------

#[derive(Default)]
struct Monitor {
    prev_cpu: Option<CpuTimes>,
    prev_net: Option<(NetCounters, Instant)>,
    cpu: Option<f32>,
    /// Memory in use (%), used bytes, total bytes.
    ram: Option<(f32, u64, u64)>,
    gpu: Option<f32>,
    /// `false` once the PDH counter turned out to be unavailable on this machine.
    gpu_available: bool,
    /// `(down, up)` bytes per second.
    net: Option<(f64, f64)>,
    /// Drive letter label, free bytes, total bytes.
    disk: Option<(String, u64, u64)>,
    cpu_hist: VecDeque<f32>,
    gpu_hist: VecDeque<f32>,
    ram_hist: VecDeque<f32>,
    gpu_worker: Option<sys::GpuSampler>,
    /// Busiest programs by CPU (name, % of the machine), from the worker.
    top: Option<Vec<(String, f32)>>,
}

impl Monitor {
    /// Forget everything sampled: a hidden tab keeps no history and no worker thread.
    fn reset(&mut self) {
        *self = Monitor { gpu_available: true, ..Monitor::default() };
    }
}

fn on_event(app: &mut App, ev: &AppEvent) {
    if matches!(ev, AppEvent::TabHidden("monitor") | AppEvent::TabShown("monitor")) {
        app.state_mut::<Monitor>().reset();
    }
}

fn on_tick(app: &mut App) {
    if !app.is_tab_visible("monitor") {
        // Hidden island (fullscreen app / paused): sample nothing and drop stale deltas.
        app.state_mut::<Monitor>().reset();
        return;
    }
    let now = Instant::now();
    let cpu = sys::cpu_times();
    let mem = sys::memory();
    let disk = sys::system_disk();
    let net = sys::net_counters();
    let st = app.state_mut::<Monitor>();

    if let (Some(prev), Some(cur)) = (st.prev_cpu, cpu) {
        let p = cpu_percent_from_times(prev, cur);
        st.cpu = Some(p);
        push_history(&mut st.cpu_hist, p);
    }
    st.prev_cpu = cpu;

    if let Some((pct, used, total)) = mem {
        st.ram = Some((pct, used, total));
        push_history(&mut st.ram_hist, pct);
    }
    st.disk = disk;

    if let Some(cur) = net {
        if let Some((prev, at)) = st.prev_net {
            st.net = Some(net_rate_from_counters(prev, cur, now.duration_since(at).as_secs_f64()));
        }
        st.prev_net = Some((cur, now));
    }

    // GPU and top processes: ask the worker for a reading (it posts `set_gpu` / `set_top`
    // back); start it on the first tick. It lives only while the tab is visible.
    if st.gpu_worker.is_none() {
        st.gpu_worker = sys::GpuSampler::spawn();
        if st.gpu_worker.is_none() {
            st.gpu_available = false;
        }
    }
    if let Some(w) = &st.gpu_worker {
        w.request();
    }
    app.request_redraw(Redraw::Tab("monitor"));
}

/// Posted by the GPU worker: `Some(percent)`, or `None` when the counter does not exist.
fn set_gpu(app: &mut App, v: Option<f32>) {
    if !app.is_tab_visible("monitor") {
        return;
    }
    let st = app.state_mut::<Monitor>();
    match v {
        Some(p) => {
            st.gpu = Some(p);
            push_history(&mut st.gpu_hist, p);
        }
        // No GPU counter here; the worker keeps sampling processes.
        None => st.gpu_available = false,
    }
    app.request_redraw(Redraw::Tab("monitor"));
}

/// Posted by the worker: the busiest programs since its last sample.
fn set_top(app: &mut App, top: Vec<(String, f32)>) {
    if !app.is_tab_visible("monitor") {
        return;
    }
    app.state_mut::<Monitor>().top = Some(top);
    app.request_redraw(Redraw::Tab("monitor"));
}

// ---- panel --------------------------------------------------------------------------------------

/// Sparkline range: a fixed 0–100 would flatten a quiet CPU, so zoom to the busiest sample
/// (never below 25 %) like the mockup's lively lines.
fn spark_range(values: &[f32]) -> (f32, f32) {
    let peak = values.iter().copied().fold(0.0_f32, f32::max);
    (0.0, (peak * 1.25).clamp(25.0, 100.0))
}

fn metric_card(name: &str, value: String, hist: &VecDeque<f32>, accent: Rgba, fixed_range: bool) -> Node {
    let values: Vec<f32> = hist.iter().copied().collect();
    let range = if fixed_range { (0.0, 100.0) } else { spark_range(&values) };
    let line = if values.len() >= 2 { sparkline(values).color(accent).range(range.0, range.1).h(26.0) } else { empty().h(26.0) };
    col([label(name), text(value).font_size(26.0).bold().display(), line])
        .gap(4.0)
        .align(Align::Stretch)
        .pad(14.0)
        .w(196.66)
        .h(108.0)
        .bg(color::FILL)
        .radius(typo::RADIUS_CARD)
}

fn pct(v: Option<f32>) -> String {
    v.map(|p| format!("{}%", p.round() as u32)).unwrap_or_else(|| "–".into())
}

fn panel(app: &App) -> Node {
    let m = app.state::<Monitor>();

    let ram_text = m.ram.map(|(_, used, _)| fmt_size(used)).unwrap_or_else(|| "–".into());
    let gpu_text = if m.gpu_available { pct(m.gpu) } else { "–".into() };
    let top = row([
        metric_card("CPU", pct(m.cpu), &m.cpu_hist, color::GREEN, false),
        metric_card("GPU", gpu_text, &m.gpu_hist, color::TEAL, false),
        metric_card("RAM", ram_text, &m.ram_hist, color::PURPLE, true),
    ])
    .gap(10.0)
    .fill_w();

    let (down, up) = m.net.unwrap_or((0.0, 0.0));
    let net_card = col([
        label(tr("Jaringan", "Network")),
        row([
            icon(icons::DOWNLOAD).color(color::BLUE),
            text(fmt_rate(down)).semibold().font_size(15.0),
            gap_px(10.0),
            icon(icons::UPLOAD).color(color::RED),
            text(fmt_rate(up)).semibold().font_size(15.0),
        ])
        .gap(6.0),
    ])
    .gap(8.0)
    .pad(14.0)
    .w(290.0)
    .h(76.0)
    .bg(color::FILL)
    .radius(typo::RADIUS_CARD);

    let disk_card = match &m.disk {
        Some((letter, free, total)) if *total > 0 => {
            let used = 1.0 - (*free as f32 / *total as f32);
            col([
                label(format!("{} {letter}", tr("Disk", "Disk"))),
                progress(used).color(color::BLUE).track(color::FILL2).thickness(6.0),
                caption(i18n::tr_s(format!("{} kosong dari {}", fmt_size(*free), fmt_size(*total)), format!("{} free of {}", fmt_size(*free), fmt_size(*total)))),
            ])
            .gap(8.0)
            .align(Align::Stretch)
        }
        _ => col([label(tr("Disk", "Disk")), caption("–")]).gap(8.0),
    }
    .pad(14.0)
    .w(310.0)
    .h(76.0)
    .bg(color::FILL)
    .radius(typo::RADIUS_CARD);

    let busiest = match &m.top {
        None => "…".to_string(),
        Some(t) if t.is_empty() => tr("tidak ada yang sibuk", "nothing busy").to_string(),
        Some(t) => t.iter().map(|(n, p)| format!("{n} {}%", num(*p as f64, if *p < 10.0 { 1 } else { 0 }, !i18n::is_english()))).collect::<Vec<_>>().join("  ·  "),
    };
    let top_row = row([label(tr("Teratas (CPU)", "Top (CPU)")), caption(busiest).grow(1.0)]).gap(10.0).fill_w().pad_xy(14.0, 0.0).h(22.0);

    col([top, row([net_card, disk_card]).gap(10.0).fill_w(), top_row]).gap(10.0).fill_w()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_percent_from_times_basic() {
        // 150 ticks elapsed (kernel+user), 50 of them idle -> 100 busy of 150.
        let a = CpuTimes { idle: 100, kernel: 200, user: 100 };
        let b = CpuTimes { idle: 150, kernel: 300, user: 150 };
        let p = cpu_percent_from_times(a, b);
        assert!((p - 66.666).abs() < 0.1, "{p}");
    }

    #[test]
    fn cpu_percent_from_times_edges() {
        let a = CpuTimes { idle: 100, kernel: 200, user: 100 };
        // No time elapsed: 0, not NaN.
        assert_eq!(cpu_percent_from_times(a, a), 0.0);
        // Fully idle / fully busy.
        assert_eq!(cpu_percent_from_times(a, CpuTimes { idle: 200, kernel: 300, user: 100 }), 0.0);
        assert_eq!(cpu_percent_from_times(a, CpuTimes { idle: 100, kernel: 250, user: 150 }), 100.0);
        // Counters that went backwards (clock reset) or idle > total: clamped, never negative / > 100.
        assert_eq!(cpu_percent_from_times(CpuTimes { idle: 500, kernel: 900, user: 100 }, a), 0.0);
        let odd = cpu_percent_from_times(a, CpuTimes { idle: 400, kernel: 250, user: 120 });
        assert!((0.0..=100.0).contains(&odd));
    }

    #[test]
    fn net_rate_from_counters_basic() {
        let a = NetCounters { rx: 1_000, tx: 500 };
        let b = NetCounters { rx: 2_001_000, tx: 100_500 };
        let (down, up) = net_rate_from_counters(a, b, 2.0);
        assert_eq!(down, 1_000_000.0);
        assert_eq!(up, 50_000.0);
    }

    #[test]
    fn net_rate_from_counters_edges() {
        let a = NetCounters { rx: 5_000, tx: 5_000 };
        // Adapter reset (counters dropped): 0, never a huge wrapped value.
        assert_eq!(net_rate_from_counters(a, NetCounters { rx: 10, tx: 20 }, 1.0), (0.0, 0.0));
        // Zero / negative interval.
        assert_eq!(net_rate_from_counters(a, NetCounters { rx: 9_000, tx: 9_000 }, 0.0), (0.0, 0.0));
        assert_eq!(net_rate_from_counters(a, NetCounters { rx: 9_000, tx: 9_000 }, -1.0), (0.0, 0.0));
    }

    fn row_(up: bool, hardware: bool, loopback: bool, rx: u64, tx: u64) -> IfRow {
        IfRow { up, hardware, loopback, rx, tx }
    }

    #[test]
    fn net_totals_counts_physical_up_interfaces_only() {
        let rows = [
            row_(true, true, false, 100, 10),    // Wi-Fi
            row_(true, false, false, 100, 10),   // virtual / filter module repeating Wi-Fi traffic
            row_(true, false, true, 999, 999),   // loopback
            row_(false, true, false, 555, 555),  // cable unplugged
            row_(true, true, false, 5, 1),       // second physical adapter
        ];
        assert_eq!(net_totals(&rows), NetCounters { rx: 105, tx: 11 });
        // Only virtual adapters (VM): fall back to every up, non-loopback row.
        let vm = [row_(true, false, false, 7, 3), row_(true, false, true, 100, 100)];
        assert_eq!(net_totals(&vm), NetCounters { rx: 7, tx: 3 });
        assert_eq!(net_totals(&[]), NetCounters::default());
    }

    #[test]
    fn gpu_percent_sums_processes_per_engine_and_takes_the_busiest_engine() {
        let items = [
            ("pid_100_luid_0x00000000_0x0000D2A5_phys_0_eng_0_engtype_3D", 20.0),
            ("pid_200_luid_0x00000000_0x0000D2A5_phys_0_eng_0_engtype_3D", 15.5),
            ("pid_300_luid_0x00000000_0x0000D2A5_phys_0_eng_1_engtype_3D", 30.0),
        ];
        let p = gpu_percent_from_instances(items);
        assert!((p - 35.5).abs() < 0.01, "{p}");
        assert_eq!(gpu_percent_from_instances([]), 0.0);
        // Over 100 (rounding / sampling skew) is clamped; junk values are ignored.
        let over = [("pid_1_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 70.0), ("pid_2_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 60.0)];
        assert_eq!(gpu_percent_from_instances(over), 100.0);
        assert_eq!(gpu_percent_from_instances([("x", f64::NAN), ("y", -5.0)]), 0.0);
    }

    #[test]
    fn formatting_uses_decimal_comma_for_indonesian() {
        assert_eq!(fmt_rate_with(0.0, true), "0 B/s");
        assert_eq!(fmt_rate_with(850.0, true), "850 B/s");
        assert_eq!(fmt_rate_with(12.4 * 1024.0 * 1024.0, true), "12,4 MB/s");
        assert_eq!(fmt_rate_with(12.4 * 1024.0 * 1024.0, false), "12.4 MB/s");
        assert_eq!(fmt_rate_with(1.2 * 1024.0 * 1024.0, true), "1,2 MB/s");
        assert_eq!(fmt_rate_with(300.0 * 1024.0, true), "300 KB/s");
        assert_eq!(fmt_rate_with(f64::NAN, true), "0 B/s");
        let gb = 1024u64 * 1024 * 1024;
        assert_eq!(fmt_size_with(6 * gb + gb / 5, true), "6,2 GB");
        assert_eq!(fmt_size_with(168 * gb, true), "168 GB");
        assert_eq!(fmt_size_with(1800 * gb, false), "1.8 TB");
    }

    #[test]
    fn top_by_cpu_adds_up_programs() {
        let prev: std::collections::HashMap<u32, u64> = [(1, 1_000), (2, 5_000), (3, 0), (4, 100)].into_iter().collect();
        let cur = vec![
            (1, "chrome".to_string(), 1_000 + 300_000), // 30 % of one core
            (2, "Chrome".to_string(), 5_000 + 100_000), // same program, +10 %
            (3, "Code".to_string(), 200_000),
            (4, "idle-ish".to_string(), 101),           // below 0.1 %: dropped
            (9, "new".to_string(), 999_999),            // no baseline yet: not counted
        ];
        // 1 s elapsed, 2 cores → 2 core-seconds = 20_000_000 units.
        let t = top_by_cpu(&prev, &cur, 10_000_000, 2, 3);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].0, "chrome");
        assert!((t[0].1 - 2.0).abs() < 0.01, "{t:?}");
        assert!((t[1].1 - 1.0).abs() < 0.01);
        assert!(top_by_cpu(&prev, &cur, 0, 2, 3).is_empty());
    }

    #[test]
    fn spark_range_zooms_but_has_a_floor() {
        assert_eq!(spark_range(&[]), (0.0, 25.0));
        assert_eq!(spark_range(&[3.0, 5.0]), (0.0, 25.0));
        assert_eq!(spark_range(&[40.0, 10.0]), (0.0, 50.0));
        assert_eq!(spark_range(&[95.0]), (0.0, 100.0));
    }
}
