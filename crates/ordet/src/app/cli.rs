use super::*;

pub(crate) fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match parse_cli_args(&args) {
        CliAction::Run {
            files,
            config,
            latency_log,
        } => {
            crate::latency::enable_from_flag(latency_log);
            // Startup is single-threaded; no task has spawned yet.
            apply_config_override(&config);
            run_with_files(files)
        }
        CliAction::Help => {
            print!("{CLI_HELP}");
            Ok(())
        }
        CliAction::Version => {
            println!("ordet {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        CliAction::PrintConfig { config } => print_config(config),
        CliAction::ResetConfig { config } => reset_config(config),
        CliAction::UsageError(message) => {
            eprintln!("{message}\n\n{CLI_HELP}");
            std::process::exit(2);
        }
    }
}

const CLI_HELP: &str = "\
Ordet - a terminal-first word processor.

Usage:
  ordet [options] [--] [file ...]

Options:
  --config <path>     Use this settings file instead of the default.
  --latency-log       Print keystroke latency percentiles (stderr) on exit.
  --print-config      Print the effective settings as JSON and exit.
  --reset-config      Write default settings (to --config, or the default
                      path) and exit. Overwrites without asking.
  -h, --help          Show this help and exit.
  -V, --version       Show the version and exit.

Anything else opens as a draft. Prefix keys with `--` to open a file whose
name starts with a dash. Inside the app, F1 opens the tabbed shortcut
guide, and Ctrl-Shift-W tracks a folder in the workspace.
";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CliAction {
    Run {
        files: Vec<String>,
        config: Option<String>,
        latency_log: bool,
    },
    Help,
    Version,
    PrintConfig {
        config: Option<String>,
    },
    ResetConfig {
        config: Option<String>,
    },
    UsageError(String),
}

pub(super) fn parse_cli_args(args: &[String]) -> CliAction {
    let mut files = Vec::new();
    let mut config = None;
    let mut latency_log = false;
    let mut only_files = false;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if only_files {
            files.push(arg.clone());
        } else if arg == "--" {
            only_files = true;
        } else if arg == "--help" || arg == "-h" {
            return CliAction::Help;
        } else if arg == "--version" || arg == "-V" {
            return CliAction::Version;
        } else if arg == "--latency-log" {
            latency_log = true;
        } else if arg == "--print-config" {
            return CliAction::PrintConfig { config };
        } else if arg == "--reset-config" {
            return CliAction::ResetConfig { config };
        } else if arg == "--config" {
            index += 1;
            match args.get(index) {
                Some(path) => config = Some(path.clone()),
                None => return CliAction::UsageError("--config needs a path.".to_string()),
            }
        } else if let Some(path) = arg.strip_prefix("--config=") {
            if path.is_empty() {
                return CliAction::UsageError("--config needs a path.".to_string());
            }
            config = Some(path.to_string());
        } else if arg.starts_with("--") {
            return CliAction::UsageError(format!("Unknown option {arg}."));
        } else {
            files.push(arg.clone());
        }
        index += 1;
    }
    CliAction::Run {
        files,
        config,
        latency_log,
    }
}

/// Point the settings lookup at `config` for this process, so `--config`
/// applies to the management commands too.
fn apply_config_override(config: &Option<String>) {
    if let Some(path) = config {
        unsafe {
            std::env::set_var("ORDET_TUI_SETTINGS_PATH", path);
        }
    }
}

fn print_config(config: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    apply_config_override(&config);
    let path = settings_path().map_err(std::io::Error::other)?;
    let settings = load_settings().unwrap_or_default();
    println!("{}", serde_json::to_string_pretty(&settings)?);
    println!("\nConfig file: {}", path.display());
    Ok(())
}

fn reset_config(config: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    apply_config_override(&config);
    let path = settings_path().map_err(std::io::Error::other)?;
    save_settings(&AppSettings::default()).map_err(std::io::Error::other)?;
    println!("Wrote default settings to {}.", path.display());
    Ok(())
}

fn run_with_files(files: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    install_signal_restore_hook();
    install_terminal_panic_hook();
    let mut app = OrdetApp::from_args(files)?;
    let mut terminal = Terminal::enter()?;
    let result = app.run(&mut terminal);
    let clear_result = terminal.restore();
    clear_result?;
    result
}

/// Restore the terminal before a panic unwinds. Background task panics are
/// already caught and reported by the task pool, so their hook stays silent
/// instead of painting a panic over the TUI.
fn install_terminal_panic_hook() {
    let main_thread = std::thread::current().id();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        if thread
            .name()
            .map(|name| name.starts_with("ordet-task-"))
            .unwrap_or(false)
        {
            return;
        }
        if thread.id() == main_thread {
            Terminal::emergency_restore();
        }
        default_hook(info);
    }));
}

/// Raw mode turns Ctrl-C into Copy, but an external SIGINT/SIGTERM/SIGHUP
/// still terminates the process without unwinding. Restore the screen and
/// mouse mode from the signal thread before exiting.
#[cfg(unix)]
fn install_signal_restore_hook() {
    use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;

    let Ok(mut signals) = Signals::new([SIGINT, SIGTERM, SIGHUP]) else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("ordet-signal".to_string())
        .spawn(move || {
            if let Some(signal) = signals.forever().next() {
                Terminal::emergency_restore();
                std::process::exit(128 + signal);
            }
        });
}

#[cfg(target_os = "windows")]
fn install_signal_restore_hook() {
    let _ = ctrlc::set_handler(|| {
        Terminal::emergency_restore();
        std::process::exit(130);
    });
}

#[cfg(not(any(unix, target_os = "windows")))]
fn install_signal_restore_hook() {}
