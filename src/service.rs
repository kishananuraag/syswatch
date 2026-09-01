//! Windows service support: install / uninstall / run for the 'syswatch' service.

#[cfg(windows)]
mod imp {
    use std::ffi::OsString;
    use std::time::Duration;

    use windows_service::{
        define_windows_service,
        service::{
            ServiceAccess, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
            ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
        },
        service_control_handler::{self, ServiceControlHandlerResult},
        service_manager::{ServiceManager, ServiceManagerAccess},
    };

    pub const SERVICE_NAME: &str = "syswatch";
    pub const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

    define_windows_service!(ffi_service_main, my_service_main);

    /// Entry point invoked by the SCM. Parses argv (unused) and starts the loop.
    fn my_service_main(args: Vec<OsString>) {
        let _ = args;
        // LocalSystem has no user LOCALAPPDATA; force PROGRAMDATA logs.
        // SAFETY: single-threaded at this point in service startup; no racing reads.
        unsafe { std::env::set_var("SYSWATCH_SERVICE", "1") };
        run_service_loop();
    }

    /// The sampling + logging loop driven by the config interval.
    fn run_service_loop() {
        let handler = match service_control_handler::register(SERVICE_NAME, move |event| match event {
            windows_service::service::ServiceControl::Stop
            | windows_service::service::ServiceControl::Shutdown => {
                SERVICE_STOPPED.store(true, Ordering::SeqCst);
                ServiceControlHandlerResult::NoError
            }
            windows_service::service::ServiceControl::Interrogate => {
                ServiceControlHandlerResult::NoError
            }
            _ => ServiceControlHandlerResult::NotImplemented,
        }) {
            Ok(h) => h,
            Err(_) => return,
        };

        handler.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::from_secs(5),
            process_id: None,
        })
        .ok();

        let cli = clap::Parser::parse_from([SERVICE_NAME, "--log"]);
        let mut cfg = crate::config::Config::load(&cli);
        if cfg.interval_ms == 0 {
            cfg.interval_ms = 1000;
        }
        crate::logging::prune(cfg.retention_days);

        let mut collector = crate::stats::Collector::new(cfg.top_n);
        collector.print_startup_self_test();
        while !SERVICE_STOPPED.load(Ordering::SeqCst) {
            collector.refresh();
            let snap = collector.snapshot(cfg.temp);
            crate::logging::append(&snap);
            std::thread::sleep(Duration::from_millis(cfg.interval_ms));
        }

        handler.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::from_secs(5),
            process_id: None,
        })
        .ok();
    }

    use std::sync::atomic::{AtomicBool, Ordering};
    static SERVICE_STOPPED: AtomicBool = AtomicBool::new(false);

    /// Register the 'syswatch' service pointing at this executable with auto-start.
    pub fn install() {
        let manager_access = ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE;
        let manager = match ServiceManager::local_computer(None::<&str>, manager_access) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("syswatch: failed to open service manager: {}", e);
                std::process::exit(1);
            }
        };

        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("syswatch: cannot determine executable path: {}", e);
                std::process::exit(1);
            }
        };
        let info = ServiceInfo {
            name: OsString::from(SERVICE_NAME),
            display_name: OsString::from("syswatch"),
            service_type: SERVICE_TYPE,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: exe,
            launch_arguments: vec![OsString::from("service"), OsString::from("run")],
            dependencies: vec![],
            account_name: None, // run as LocalSystem
            account_password: None,
        };

        let service = match manager.create_service(&info, ServiceAccess::QUERY_STATUS) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("syswatch: failed to create service '{}': {}", SERVICE_NAME, e);
                std::process::exit(1);
            }
        };
        let _ = service;
        println!("syswatch: service '{}' installed (auto-start)", SERVICE_NAME);
    }

    /// Remove the 'syswatch' service.
    pub fn uninstall() {
        let manager =
            match ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("syswatch: failed to open service manager: {}", e);
                    std::process::exit(1);
                }
            };
        let service = match manager.open_service(
            SERVICE_NAME,
            ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
        ) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("syswatch: failed to open service '{}': {}", SERVICE_NAME, e);
                std::process::exit(1);
            }
        };
        // Stop the service first if it is running.
        if let Ok(status) = service.query_status() {
            if status.current_state == ServiceState::Running {
                let _ = service.stop();
            }
        }
        if let Err(e) = service.delete() {
            eprintln!("syswatch: failed to delete service '{}': {}", SERVICE_NAME, e);
            std::process::exit(1);
        }
        println!("syswatch: service '{}' uninstalled", SERVICE_NAME);
    }

    /// Entrypoint for `syswatch service run`: hand control to the SCM.
    pub fn run_service() {
        windows_service::service_dispatcher::start(SERVICE_NAME, ffi_service_main)
            .expect("start service dispatcher");
    }
}

#[cfg(windows)]
pub use imp::*;

#[cfg(not(windows))]
mod imp {
    pub fn install() {
        eprintln!("syswatch: service management is only supported on Windows");
        std::process::exit(1);
    }

    pub fn uninstall() {
        eprintln!("syswatch: service management is only supported on Windows");
        std::process::exit(1);
    }

    pub fn run_service() {
        eprintln!("syswatch: service management is only supported on Windows");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
pub use imp::*;
