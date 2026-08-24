use std::collections::VecDeque;
use std::time::Duration;
use sysinfo::{Components, Disks, Networks, System};

#[derive(Clone, Copy, PartialEq)]
pub enum SortMode {
    Cpu,
    Mem,
}

pub struct AppState {
    pub sys: System,
    pub disks: Disks,
    pub networks: Networks,
    pub components: Components,
    pub interval: Duration,
    pub sort_mode: SortMode,
    pub cpu_history: VecDeque<u64>,
    pub net_rx_history: VecDeque<u64>,
    pub net_tx_history: VecDeque<u64>,
}

impl AppState {
    pub fn new(interval: Duration) -> Self {
        Self {
            sys: System::new_all(),
            disks: Disks::new_with_refreshed_list(),
            networks: Networks::new_with_refreshed_list(),
            components: Components::new_with_refreshed_list(),
            interval,
            sort_mode: SortMode::Cpu,
            cpu_history: VecDeque::new(),
            net_rx_history: VecDeque::new(),
            net_tx_history: VecDeque::new(),
        }
    }

    pub fn toggle_sort(&mut self) {
        self.sort_mode = match self.sort_mode {
            SortMode::Cpu => SortMode::Mem,
            SortMode::Mem => SortMode::Cpu,
        };
    }
}
