use std::cmp::Reverse;
use sysinfo::ProcessesToUpdate;
use crate::app::{AppState, SortMode};

pub struct ProcessRow {
    pub pid: u32,
    pub name: String,
    pub cpu_pct: f32,
    pub mem_mb: u64,
}

pub fn refresh(state: &mut AppState) {
    state.sys.refresh_processes(ProcessesToUpdate::All, true);
    state.sys.refresh_cpu_usage();
    state.sys.refresh_memory();
    state.networks.refresh(true);
    state.disks.refresh(true);
    state.components.refresh(false);

    let cpu = avg_cpu(&state.sys) as u64;
    push_capped(&mut state.cpu_history, cpu, 60);

    let rx: u64 = state.networks.iter().map(|(_, d)| d.received()).sum();
    let tx: u64 = state.networks.iter().map(|(_, d)| d.transmitted()).sum();
    push_capped(&mut state.net_rx_history, rx, 60);
    push_capped(&mut state.net_tx_history, tx, 60);
}

pub fn avg_cpu(sys: &sysinfo::System) -> f32 {
    let cpus = sys.cpus();
    if cpus.is_empty() {
        return 0.0;
    }
    cpus.iter().map(|c| c.cpu_usage()).sum::<f32>() / cpus.len() as f32
}

pub fn top_processes(state: &AppState, n: usize) -> Vec<ProcessRow> {
    let mut procs: Vec<ProcessRow> = state
        .sys
        .processes()
        .values()
        .map(|p| ProcessRow {
            pid: p.pid().as_u32(),
            name: p.name().to_string_lossy().into_owned(),
            cpu_pct: p.cpu_usage(),
            mem_mb: p.memory() / 1_048_576,
        })
        .collect();

    match state.sort_mode {
        SortMode::Cpu => {
            procs.sort_by(|a, b| b.cpu_pct.partial_cmp(&a.cpu_pct).unwrap_or(std::cmp::Ordering::Equal));
        }
        SortMode::Mem => {
            procs.sort_by_key(|p| Reverse(p.mem_mb));
        }
    }

    procs.truncate(n);
    procs
}

fn push_capped(buf: &mut std::collections::VecDeque<u64>, val: u64, cap: usize) {
    buf.push_back(val);
    if buf.len() > cap {
        buf.pop_front();
    }
}
