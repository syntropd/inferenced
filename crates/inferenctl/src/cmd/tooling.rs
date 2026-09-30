use crate::client::VarlinkClient;
use anyhow::Result;
use colored::*;
use inferenced_core::{arbiter::Arbiter, lease::LeasePriority, psi::PressureMetrics, topology::HardwareTopology};
use std::path::Path;
use std::time::Instant;

const BASH_COMPLETIONS: &str = r#"# bash completion for inferenctl
_inferenctl() {
    local cur prev words cword
    _init_completion || return
    local commands="status planes leases monitor exec freeze thaw register warm pin evict models cat-config check-config dump inspect test-triage benchmark completions man"
    if [[ $cword -eq 1 ]]; then
        COMPREPLY=( $(compgen -W "$commands" -- "$cur") )
        return 0
    fi
}
complete -F _inferenctl inferenctl
"#;

const ZSH_COMPLETIONS: &str = r#"#compdef inferenctl
_inferenctl() {
    local -a commands
    commands=(
        'status:Show AI compute status and active leases'
        'planes:List discovered compute planes'
        'leases:List active compute slice leases'
        'monitor:Live monitor of hardware pressure and bus saturation'
        'exec:Unix stream filter for model inference'
        'freeze:Freeze a compute lease'
        'thaw:Thaw a compute lease'
        'models:List registered models'
        'register:Register a model'
        'warm:Warm model weights in memory'
        'pin:Pin model for emergency triage'
        'evict:Evict model from active memory'
        'cat-config:Print configuration'
        'check-config:Validate configuration syntax'
        'dump:Dump internal state as JSON'
        'inspect:Inspect compute plane details'
        'test-triage:Synthetic Sentry diagnostic ping'
        'benchmark:Benchmark IPC and lease throughput'
        'completions:Generate shell completions'
        'man:Generate man page'
    )
    _describe 'command' commands
}
_inferenctl "$@"
"#;

const FISH_COMPLETIONS: &str = r#"# fish completion for inferenctl
complete -c inferenctl -f
complete -c inferenctl -n "__fish_use_subcommand" -a "status" -d "Display daemon operational health and compute planes"
complete -c inferenctl -n "__fish_use_subcommand" -a "planes" -d "List discovered compute planes and memory pools"
complete -c inferenctl -n "__fish_use_subcommand" -a "leases" -d "Display active resource leases"
complete -c inferenctl -n "__fish_use_subcommand" -a "monitor" -d "Stream real-time Linux PSI pressure"
complete -c inferenctl -n "__fish_use_subcommand" -a "exec" -d "Composable Unix stream filter"
complete -c inferenctl -n "__fish_use_subcommand" -a "freeze" -d "Freeze an active compute lease"
complete -c inferenctl -n "__fish_use_subcommand" -a "thaw" -d "Resume a frozen compute lease"
complete -c inferenctl -n "__fish_use_subcommand" -a "models" -d "List registered models"
complete -c inferenctl -n "__fish_use_subcommand" -a "register" -d "Register a local model file"
complete -c inferenctl -n "__fish_use_subcommand" -a "warm" -d "Pre-fault and warm model pages"
complete -c inferenctl -n "__fish_use_subcommand" -a "pin" -d "Pin a model for emergency triage"
complete -c inferenctl -n "__fish_use_subcommand" -a "evict" -d "Evict a model from accelerator memory"
complete -c inferenctl -n "__fish_use_subcommand" -a "cat-config" -d "Print parsed global configuration"
complete -c inferenctl -n "__fish_use_subcommand" -a "check-config" -d "Validate configuration syntax"
complete -c inferenctl -n "__fish_use_subcommand" -a "dump" -d "Dump complete internal state as JSON"
complete -c inferenctl -n "__fish_use_subcommand" -a "inspect" -d "Inspect compute plane attributes"
complete -c inferenctl -n "__fish_use_subcommand" -a "test-triage" -d "Send synthetic Sentry diagnostic ping"
complete -c inferenctl -n "__fish_use_subcommand" -a "benchmark" -d "Execute throughput and latency benchmarks"
complete -c inferenctl -n "__fish_use_subcommand" -a "completions" -d "Generate shell completion scripts"
complete -c inferenctl -n "__fish_use_subcommand" -a "man" -d "Output man page in troff format"
"#;

const MAN_PAGE: &str = r#".TH INFERENCTL 1 "September 2026" "systemd-inferenced 0.1.0" "User Commands"
.SH NAME
inferenctl \- Control and inspect systemd-inferenced hardware arbitration
.SH SYNOPSIS
.B inferenctl [\fIOPTIONS\fR] \fICOMMAND\fR [\fIARGS\fR]
.SH DESCRIPTION
\fBinferenctl\fR communicates with \fBsystemd-inferenced\fR over Varlink IPC
to manage compute slice leases, monitor memory bus saturation, and stream
zero-copy inference filters.
.SH COMMANDS
.TP
\fBstatus\fR, \fBplanes\fR, \fBleases\fR, \fBmonitor\fR
Hardware topology discovery, active compute leases, and live PSI monitoring.
.TP
\fBexec\fR \fIMODEL\fR [\fIPROMPT\fR]
Composable Unix stream filter: read prompt from stdin, stream tokens to stdout.
.TP
\fBfreeze\fR, \fBthaw\fR \fILEASE_ID\fR
Freeze and resume compute slice execution via cgroup.freeze or SIGSTOP.
.TP
\fBmodels\fR, \fBregister\fR, \fBwarm\fR, \fBpin\fR, \fBevict\fR
Model lifecycle: registration, pre-faulting (madvise), pinning, and memory eviction.
.TP
\fBcat-config\fR, \fBcheck-config\fR, \fBdump\fR, \fBinspect\fR
Configuration parsing, validation, full state dumping, and plane attribute inspection.
.TP
\fBtest-triage\fR, \fBbenchmark\fR
Synthetic Sentry emergency triage verification ping and IPC throughput benchmark.
.TP
\fBcompletions\fR, \fBman\fR
Shell tab-completion generation (bash, zsh) and troff man page output.
.SH AUTHORS
systemd-inferenced contributors.
"#;

pub fn run_completions(shell: &str) -> Result<()> {
    match shell.to_lowercase().as_str() {
        "bash" => print!("{BASH_COMPLETIONS}"),
        "zsh" => print!("{ZSH_COMPLETIONS}"),
        "fish" => print!("{FISH_COMPLETIONS}"),
        _ => {
            eprintln!("Unsupported shell: {}. Supported shells: bash, zsh, fish", shell);
        }
    }
    Ok(())
}

pub fn run_man() -> Result<()> {
    print!("{MAN_PAGE}");
    Ok(())
}

pub fn run_benchmark(socket_path: impl AsRef<Path>, iterations: usize) -> Result<()> {
    println!("{}", "Benchmarking systemd-inferenced IPC & Scheduler...".bold());

    if let Ok(mut client) = VarlinkClient::connect(socket_path.as_ref()) {
        let start = Instant::now();
        for _ in 0..iterations {
            let _ = client.get_pressure()?;
        }
        let elapsed = start.elapsed();
        let avg_us = elapsed.as_micros() as f64 / iterations as f64;
        println!("  Varlink IPC Ping ({iterations} calls): total {elapsed:?}, avg {avg_us:.2} µs/call");

        let start = Instant::now();
        for _ in 0..iterations {
            let lease = client.acquire_lease("Interactive", 1024 * 1024, None)?;
            if let Some(id) = lease.get("lease_id").and_then(|v| v.as_str()) {
                client.release_lease(id)?;
            }
        }
        let elapsed = start.elapsed();
        let ops = (iterations * 2) as f64 / elapsed.as_secs_f64();
        println!("  Lease Lifecycle Throughput: {ops:.0} ops/sec ({elapsed:?})");
        return Ok(());
    }

    // Offline in-process benchmark
    println!("  (Daemon offline; running in-process arbiter benchmark)");
    let topo = HardwareTopology::discover().unwrap_or_default();
    let arbiter = Arbiter::new(topo);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;

    let start = Instant::now();
    for _ in 0..iterations {
        let _ = PressureMetrics::read_current();
    }
    let elapsed = start.elapsed();
    let avg_us = elapsed.as_micros() as f64 / iterations as f64;
    println!("  Local PSI Sampling ({iterations} calls): total {elapsed:?}, avg {avg_us:.2} µs/call");

    let start = Instant::now();
    rt.block_on(async {
        for _ in 0..iterations {
            if let Ok(l) = arbiter.acquire_lease(LeasePriority::Interactive, 1024 * 1024, None, None, None).await {
                let _ = arbiter.release_lease(l.id).await;
            }
        }
    });
    let elapsed = start.elapsed();
    let ops = (iterations * 2) as f64 / elapsed.as_secs_f64();
    println!("  In-Memory Arbiter Throughput: {ops:.0} ops/sec ({elapsed:?})");
    Ok(())
}
