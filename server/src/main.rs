mod alarm;
mod api;
mod diagnostics;
mod time_hardware_stub;
mod time_provider;
mod time_software;
mod ui;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::{env, ffi::OsString};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};

use crate::alarm::model::load_alarm_config;
use crate::alarm::scheduler::AlarmScheduler;
use crate::api::{ApiServer, ApiServerConfig, ApiSharedState};
use crate::time_provider::{TimingSourceKind, select_provider};

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum CliTimingSource {
    Auto,
    Software,
    Hardware,
}

impl From<CliTimingSource> for TimingSourceKind {
    fn from(value: CliTimingSource) -> Self {
        match value {
            CliTimingSource::Auto => TimingSourceKind::Auto,
            CliTimingSource::Software => TimingSourceKind::Software,
            CliTimingSource::Hardware => TimingSourceKind::Hardware,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "betterclock",
    version,
    about = "High-FPS CLI clock with alarm scheduling"
)]
struct Cli {
    #[arg(long, default_value = "alarms.json")]
    alarms: PathBuf,

    #[arg(
        long = "simFPS",
        visible_alias = "sim-fps",
        alias = "simfps",
        default_value_t = 10_000
    )]
    sim_fps: u16,

    #[arg(
        long = "FPS",
        visible_alias = "fps",
        alias = "render-fps",
        default_value_t = 240
    )]
    render_fps: u16,

    #[arg(long, value_enum, default_value_t = CliTimingSource::Auto)]
    timing_source: CliTimingSource,

    #[arg(long)]
    diagnostics: bool,

    #[arg(long, default_value = "0.0.0.0")]
    api_bind: String,

    #[arg(long, default_value_t = 8099)]
    api_port: u16,

    #[arg(long, default_value_t = true)]
    discovery_enabled: bool,

    #[arg(long, default_value_t = 8099)]
    discovery_udp_port: u16,

    #[arg(long, default_value_t = true)]
    mdns_enabled: bool,

    #[arg(long, default_value = "betterclock")]
    mdns_instance: String,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse_from(normalize_legacy_flags());
    if cli.sim_fps == 0 {
        bail!("--simFPS must be greater than zero");
    }
    if cli.render_fps == 0 {
        bail!("--FPS must be greater than zero");
    }

    let alarm_config = load_alarm_config(&cli.alarms)
        .with_context(|| format!("failed to load {}", cli.alarms.display()))?;
    let scheduler = AlarmScheduler::new(alarm_config.alarms);
    let selected = select_provider(cli.timing_source.into())?;

    if cli.diagnostics {
        diagnostics::run_diagnostics(&selected, cli.sim_fps)?;
        return Ok(());
    }

    let api_server = ApiServer::start(ApiServerConfig {
        bind_addr: cli.api_bind.clone(),
        port: cli.api_port,
        discovery_enabled: cli.discovery_enabled,
        discovery_udp_port: cli.discovery_udp_port,
        mdns_enabled: cli.mdns_enabled,
        mdns_instance: cli.mdns_instance,
    })
    .with_context(|| {
        format!(
            "failed to start local API at {}:{}",
            cli.api_bind, cli.api_port
        )
    })?;
    let api_state: Arc<Mutex<ApiSharedState>> = Arc::clone(&api_server.state);

    let ui_result = ui::app::run_gui(
        selected,
        scheduler,
        cli.sim_fps,
        cli.render_fps,
        cli.alarms,
        alarm_config.settings,
        Some(api_state),
        cli.api_bind,
        cli.api_port,
    );

    drop(api_server);
    ui_result
}

fn normalize_legacy_flags() -> Vec<OsString> {
    env::args_os()
        .map(|arg| {
            let Some(text) = arg.to_str() else {
                return arg;
            };
            if text.eq_ignore_ascii_case("-simFPS") {
                OsString::from("--simFPS")
            } else if text.eq_ignore_ascii_case("-FPS") {
                OsString::from("--FPS")
            } else {
                arg
            }
        })
        .collect()
}
