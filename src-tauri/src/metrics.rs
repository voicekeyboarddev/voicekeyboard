use crate::types::SystemMetrics;
use std::process::Command;

pub fn collect(server_pid: Option<u32>) -> SystemMetrics {
    let app_ram_mb = process_working_set_mb(std::process::id());
    let server_ram_mb = server_pid.and_then(process_working_set_mb);
    let gpu = gpu_metrics();

    SystemMetrics {
        app_ram_mb,
        server_ram_mb,
        total_ram_mb: match (app_ram_mb, server_ram_mb) {
            (Some(app), Some(server)) => Some(app + server),
            (Some(app), None) => Some(app),
            (None, Some(server)) => Some(server),
            (None, None) => None,
        },
        gpu_util_percent: gpu.as_ref().and_then(|g| g.util_percent),
        gpu_mem_used_mb: gpu.as_ref().and_then(|g| g.mem_used_mb),
        gpu_mem_total_mb: gpu.as_ref().and_then(|g| g.mem_total_mb),
    }
}

#[derive(Debug)]
struct GpuMetrics {
    util_percent: Option<f64>,
    mem_used_mb: Option<f64>,
    mem_total_mb: Option<f64>,
}

fn gpu_metrics() -> Option<GpuMetrics> {
    let mut command = hidden_command("nvidia-smi");
    let output = command
        .args([
            "--query-gpu=utilization.gpu,memory.used,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().next()?;
    let mut parts = line.split(',').map(|part| part.trim().parse::<f64>().ok());

    Some(GpuMetrics {
        util_percent: parts.next().flatten(),
        mem_used_mb: parts.next().flatten(),
        mem_total_mb: parts.next().flatten(),
    })
}

#[cfg(windows)]
fn hidden_command(program: &str) -> Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let mut command = Command::new(program);
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

#[cfg(not(windows))]
fn hidden_command(program: &str) -> Command {
    Command::new(program)
}

#[cfg(windows)]
fn process_working_set_mb(pid: u32) -> Option<f64> {
    use windows::Win32::{
        Foundation::CloseHandle,
        System::{
            ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ},
        },
    };

    unsafe {
        let handle = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ,
            false,
            pid,
        )
        .ok()?;
        let mut counters = PROCESS_MEMORY_COUNTERS::default();
        let ok = GetProcessMemoryInfo(
            handle,
            &mut counters,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        )
        .is_ok();
        let _ = CloseHandle(handle);

        ok.then_some(counters.WorkingSetSize as f64 / 1024.0 / 1024.0)
    }
}

#[cfg(not(windows))]
fn process_working_set_mb(_pid: u32) -> Option<f64> {
    None
}
