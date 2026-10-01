mod activation;
mod creds;
mod fd_quota;
mod fd_server;
mod gateway;
mod inhibit;
mod notify;
mod sentry;
mod service;
mod varlink;

#[cfg(test)]
mod fd_tests;

use activation::{bind_standalone_unix, check_and_adopt_sockets, GatewayListener};
use clap::Parser;
use gateway::{build_gateway_router, serve_gateway, AppState};
use inferenced_core::{
    arbiter::Arbiter, preempt::PreemptCoordinator, topology::HardwareTopology,
};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
#[command(name = "inferenced")]
#[command(about = "Heterogeneous AI Hardware Arbiter and Model Lifecycle Broker")]
#[command(version)]
struct Cli {
    #[arg(short, long, default_value = "/etc/systemd/inferenced.conf")]
    config: PathBuf,
    #[arg(short, long)]
    bind: Option<SocketAddr>,
    #[arg(long, default_value = "/run/syntrop/gateway.sock")]
    gateway_socket: PathBuf,
    #[arg(long, default_value = "/run/syntrop/io.syntrop.Inference1")]
    varlink_socket: PathBuf,
    #[arg(long, default_value = "/run/syntrop/sentry.sock")]
    sentry_socket: PathBuf,
    #[arg(long, default_value = "/run/syntrop/fd.sock")]
    fd_socket: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "inferenced=info,systemd_inferenced=info,inferenced_core=info".into()),
        )
        .init();

    let cli = Cli::parse();
    info!("Starting inferenced daemon in pure Rust...");

    let mut activated = check_and_adopt_sockets()?;

    let topo = HardwareTopology::discover()?;
    info!(
        "Discovered {} compute planes ({} GB RAM, {} cores)",
        topo.planes.len(),
        topo.total_system_ram_bytes / (1024 * 1024 * 1024),
        topo.cpu_cores_total
    );

    let arbiter = Arc::new(Arbiter::new(topo));
    let preempt = Arc::new(PreemptCoordinator::new(arbiter.clone()));
    let inhibitor = Arc::new(inhibit::InhibitorManager::new());

    // 1. Varlink IPC Server (FD 3)
    let varlink_listener = match activated.varlink.take() {
        Some(l) => l,
        None => varlink::bind_or_create_listener(
            cli.varlink_socket.to_str().unwrap_or(varlink::DEFAULT_VARLINK_PATH),
        )?,
    };
    let varlink_arbiter = arbiter.clone();
    tokio::spawn(async move {
        if let Err(e) = varlink::run_varlink_listener(varlink_listener, varlink_arbiter).await {
            error!("Varlink IPC server encountered an error: {}", e);
        }
    });

    // 2. Sentry Emergency Enclave (FD 4)
    let sentry_listener = match activated.sentry.take() {
        Some(l) => l,
        None => sentry::bind_or_create_sentry_listener(&cli.sentry_socket)?,
    };
    let sentry_arbiter = arbiter.clone();
    tokio::spawn(async move {
        if let Err(e) = sentry::run_sentry_triage_listener(sentry_listener, sentry_arbiter).await {
            error!("Sentry emergency triage socket failed: {}", e);
        }
    });

    // 3. SCM_RIGHTS Zero-Copy FD Server (FD 6)
    let fd_listener = match activated.fd_server.take() {
        Some(l) => l,
        None => fd_server::bind_or_create_fd_listener(
            cli.fd_socket.to_str().unwrap_or(fd_server::DEFAULT_FD_SOCKET_PATH),
        )?,
    };
    tokio::spawn(async move {
        if let Err(e) = fd_server::run_fd_server(fd_listener).await {
            error!("SCM_RIGHTS FD server encountered an error: {}", e);
        }
    });

    // 4. HTTP Gateway Router & Server (FD 5 or standalone bind)
    let api_token = creds::resolve_secret("INFERENCED_API_TOKEN", "gateway_api_token")
        .or_else(|| creds::load_credential("api_token"));
    if api_token.is_some() {
        info!("Gateway API token configured via systemd-creds ($CREDENTIALS_DIRECTORY)");
    }
    let state = Arc::new(AppState {
        arbiter: arbiter.clone(),
        preempt: preempt.clone(),
        api_token,
    });
    let app = build_gateway_router(state);

    let gateway_listener = match activated.gateway.take() {
        Some(l) => l,
        None => {
            if let Some(bind_addr) = cli.bind {
                info!("Binding HTTP gateway to {}", bind_addr);
                GatewayListener::Tcp(TcpListener::bind(bind_addr).await?)
            } else {
                info!("Binding HTTP gateway to unix socket {:?}", cli.gateway_socket);
                let listener = bind_standalone_unix(&cli.gateway_socket)?;
                GatewayListener::Unix(listener)
            }
        }
    };

    notify::notify_systemd_ready();

    // 5. Systemd-udevd Netlink KOBJECT_UEVENT listener for hardware hotplug & driver resets
    let arbiter_hotplug = arbiter.clone();
    let _udev_task = tokio::spawn(async move {
        if let Ok(fd) = inferenced_core::open_uevent_socket() {
            info!("Subscribed to Linux Netlink KOBJECT_UEVENT for dynamic hardware discovery");
            let mut buf = [0u8; 8192];
            let stream = tokio::net::UdpSocket::from_std(std::net::UdpSocket::from(fd));
            if let Ok(socket) = stream {
                loop {
                    if let Ok((n, _)) = socket.recv_from(&mut buf).await {
                        if let Some(uevent) = inferenced_core::Uevent::parse(&buf[..n]) {
                            if uevent.is_compute_device() {
                                if uevent.is_reset_event() {
                                    warn!(
                                        "Kernel compute ASIC reset (action={}, devpath={}), refreshing topology",
                                        uevent.action, uevent.devpath
                                    );
                                } else {
                                    info!(
                                        "Kernel compute uevent (action={}, subsystem={}), refreshing topology",
                                        uevent.action, uevent.subsystem
                                    );
                                }
                                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                                let _ = arbiter_hotplug.refresh_topology().await;
                            }
                        }
                    }
                }
            }
        }
    });

    // 6. Systemd Watchdog keepalive loop (pings every 10s for WatchdogSec=30s if arbiter is responsive)
    let arbiter_clone = arbiter.clone();
    let inhibitor_watchdog = inhibitor.clone();
    let watchdog_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
        loop {
            interval.tick().await;
            if arbiter_clone.health_check().await {
                notify::notify_systemd_watchdog();
                if inhibitor_watchdog.is_inhibited() {
                    notify::notify_systemd_status(&format!(
                        "Active AI leases executing ({}, sleep/idle inhibited)",
                        inhibitor_watchdog.active_lease_count()
                    ));
                }
            }
        }
    });

    // 7. Systemd-logind sleep/idle inhibitor background monitor
    let arbiter_inhibit = arbiter.clone();
    let inhibitor_monitor = inhibitor.clone();
    let _inhibit_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(250));
        let mut prev_count = 0;
        loop {
            interval.tick().await;
            let count = arbiter_inhibit.list_leases().await.into_iter().filter(|l| l.is_active()).count();
            if count > prev_count {
                for _ in 0..(count - prev_count) { inhibitor_monitor.on_lease_acquired().await; }
            } else if count < prev_count {
                for _ in 0..(prev_count - count) { inhibitor_monitor.on_lease_released().await; }
            }
            prev_count = count;
        }
    });

    // 8. Schedule SlicePreemptCoordinator in inferenced-daemon service loop
    let (_slice_coord, slice_task) = service::schedule_slice_preemption(
        preempt.clone(),
        arbiter.clone(),
        service::DEFAULT_PREEMPT_INTERVAL,
    );

    serve_gateway(gateway_listener, app, shutdown_signal(inhibitor.clone(), arbiter.clone())).await?;

    watchdog_task.abort();
    slice_task.abort();
    inhibitor.quiesce_for_sleep(&arbiter).await;
    notify::notify_systemd_stopping();
    info!("inferenced daemon terminated cleanly.");
    Ok(())
}

async fn shutdown_signal(inhibitor: Arc<inhibit::InhibitorManager>, arbiter: Arc<Arbiter>) {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    let mut sigcont = tokio::signal::unix::signal(
        tokio::signal::unix::SignalKind::from_raw(rustix::process::Signal::Cont as i32),
    ).ok();

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("Termination signal SIGINT received, shutting down gracefully...");
                inhibitor.quiesce_for_sleep(&arbiter).await;
                break;
            }
            _ = async { match sigterm.as_mut() { Some(s) => { s.recv().await; }, None => std::future::pending().await } } => {
                info!("Termination signal SIGTERM from systemd received, shutting down gracefully...");
                inhibitor.quiesce_for_sleep(&arbiter).await;
                break;
            }
            _ = async { match sigcont.as_mut() { Some(s) => { s.recv().await; }, None => std::future::pending().await } } => {
                info!("SIGCONT received from systemd-logind/kernel; resuming model paging...");
                inhibitor.resume_from_sleep(&arbiter).await;
            }
        }
    }
}
