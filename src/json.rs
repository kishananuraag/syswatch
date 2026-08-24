use sysinfo::{Disks, Networks, System};

pub fn print_json(sys: &System, disks: &Disks, networks: &Networks) {
    let cpu_usage: f32 = sys.cpus().iter().map(|c| c.cpu_usage()).sum::<f32>()
        / sys.cpus().len().max(1) as f32;

    let total_ram = sys.total_memory();
    let used_ram = sys.used_memory();

    let disk_info: Vec<String> = disks
        .iter()
        .filter(|d| d.total_space() > 0)
        .map(|d| {
            let total = d.total_space();
            let free = d.available_space();
            let used_pct = (total - free) as f64 / total as f64 * 100.0;
            format!(
                r#"{{"free_gb":{},"used_pct":{:.1}}}"#,
                free / 1024 / 1024 / 1024,
                used_pct
            )
        })
        .collect();

    let net_info: Vec<String> = networks
        .iter()
        .filter(|(_, d)| d.received() > 0 || d.transmitted() > 0)
        .map(|(name, d)| {
            format!(
                r#"{{"name":"{}","rx_kb":{},"tx_kb":{}}}"#,
                name,
                d.received() / 1024,
                d.transmitted() / 1024
            )
        })
        .collect();

    println!(
        r#"{{"cpu_pct":{:.1},"ram_used_mb":{},"ram_total_mb":{},"disks":[{}],"networks":[{}]}}"#,
        cpu_usage,
        used_ram / 1024 / 1024,
        total_ram / 1024 / 1024,
        disk_info.join(","),
        net_info.join(","),
    );
}
