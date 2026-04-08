use sysinfo::System;

fn main() {
    let mut sys = System::new_all();
    sys.refresh_all();

    // CPU usage — average across all cores
    let cpu_usage: f32 = sys.cpus().iter().map(|c| c.cpu_usage()).sum::<f32>()
        / sys.cpus().len() as f32;

    // RAM
    let total_ram = sys.total_memory();   // bytes
    let used_ram  = sys.used_memory();
    let ram_pct   = used_ram as f64 / total_ram as f64 * 100.0;

    println!("=== syswatch ===");
    println!("CPU:  {:.1}%", cpu_usage);
    println!("RAM:  {:.1}% ({} MB / {} MB)",
        ram_pct,
        used_ram  / 1024 / 1024,
        total_ram / 1024 / 1024,
    );
}
