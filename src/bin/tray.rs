use std::thread;
use std::time::Duration;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::TrayIconBuilder;
use tray_icon::icon::Icon;
use reqwest::blocking::Client;

#[derive(serde::Deserialize, serde::Serialize, Debug)]
struct CpuInfo {
    total_pct: f64,
}

#[derive(serde::Deserialize, serde::Serialize, Debug)]
struct RamInfo {
    pct: f64,
}

#[derive(serde::Deserialize, serde::Serialize, Debug)]
struct SyswatchData {
    cpu: CpuInfo,
    ram: RamInfo,
}

fn fetch_syswatch_data() -> Result<SyswatchData, reqwest::Error> {
    let client = Client::new();
    let resp = client
        .get("http://127.0.0.1:8123/api/current")
        .send()?
        .json::<SyswatchData>()?;
    Ok(resp)
}

fn main() {
    // 1) Get the menu event channel
    let menu_channel = MenuEvent::receiver();

    // 2) Build the menu items and keep references so we can compare IDs later
    let dashboard_i = MenuItem::new("Open Dashboard", true, None);
    let separator = PredefinedMenuItem::separator();
    let quit_i = MenuItem::new("Quit", true, None);

    let menu = Menu::new();
    menu.append_items(&[&dashboard_i, &separator, &quit_i]);

    // 3) Create the tray icon
    let tray_icon = TrayIconBuilder::new()
        .with_tooltip("syswatch")
        .with_menu(Box::new(menu))
        .with_icon(create_default_icon())
        .with_menu_on_left_click(false)
        .build()
        .expect("Failed to build tray icon");

    // 4) Background thread: poll the dashboard every 2 seconds and log the snapshot
    let tray_handle = tray_icon; // not used for updates yet, but keep alive
    thread::spawn(move || {
        loop {
            match fetch_syswatch_data() {
                Ok(data) => {
                    let tip = format!("syswatch\nCPU: {:.1}%\nRAM: {:.1}%", data.cpu.total_pct, data.ram.pct);
                    println!("{}", tip);
                }
                Err(e) => {
                    eprintln!("Failed to fetch syswatch data: {}", e);
                }
            }
            thread::sleep(Duration::from_secs(2));
        }
    });

    // 5) Event loop – react to menu clicks
    let mut should_quit = false;
    while !should_quit {
        if let Ok(event) = menu_channel.try_recv() {
            if event.id == quit_i.id() {
                println!("Quit requested");
                should_quit = true;
            } else if event.id == dashboard_i.id() {
                println!("Dashboard requested");
                if let Err(e) = open::that("http://127.0.0.1:8123") {
                    eprintln!("Failed to open dashboard: {}", e);
                }
            } else {
                println!("Menu event: {:?}", event);
            }
        }
        thread::sleep(Duration::from_millis(100));
    }

    // Drop the tray icon to remove the icon from the system tray
    drop(tray_handle);
    println!("Exiting syswatch-tray");
}

fn create_default_icon() -> Icon {
    // 16x16 RGBA – simple checkered pattern (transparent corners, opaque squares)
    let w = 16u32;
    let h = 16u32;
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let on = (x + y) % 2 == 0;
            if on {
                rgba.extend_from_slice(&[0, 0, 0, 255]); // black
            } else {
                rgba.extend_from_slice(&[0, 0, 0, 0]); // transparent
            }
        }
    }
    Icon::from_rgba(rgba, w, h).expect("Failed to create icon")
}