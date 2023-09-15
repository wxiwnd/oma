use std::path::PathBuf;

use std::process::{exit, Command};

mod args;
mod config;
mod error;
mod history;
mod lang;
mod subcommand;
mod table;
mod utils;

use anyhow::{anyhow, Result};

use clap::ArgMatches;
use error::OutputError;
use nix::sys::signal;
use oma_console::{console::style, info};
use oma_console::{debug, due_to, error, DEBUG, WRITER};
use oma_utils::oma::{terminal_ring, unlock_oma};
use oma_utils::OsRelease;

use std::sync::atomic::{AtomicBool, Ordering};

use oma_console::console;
use oma_console::pager::SUBPROCESS;

use crate::config::{Config, GeneralConfig};
use crate::subcommand::*;

static ALLOWCTRLC: AtomicBool = AtomicBool::new(false);
static LOCKED: AtomicBool = AtomicBool::new(false);
static AILURUS: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Default)]
pub struct InstallArgs {
    no_refresh: bool,
    install_dbg: bool,
    reinstall: bool,
    no_fixbroken: bool,
    yes: bool,
    force_yes: bool,
    force_confnew: bool,
    dpkg_force_all: bool,
    install_recommends: bool,
    install_suggests: bool,
    no_install_recommends: bool,
    no_install_suggests: bool,
}

#[derive(Debug, Default)]
pub struct UpgradeArgs {
    yes: bool,
    force_yes: bool,
    force_confnew: bool,
    dpkg_force_all: bool,
}

#[derive(Debug, Default)]
pub struct RemoveArgs {
    yes: bool,
    remove_config: bool,
    no_autoremove: bool,
    force_yes: bool,
}

fn main() {
    ctrlc::set_handler(single_handler).expect(
        "Oma could not initialize SIGINT handler.\n\nPlease restart your installation environment.",
    );

    let code = match try_main() {
        Ok(exit_code) => exit_code,
        Err(e) => {
            let (err, dueto) = e.inner();
            if !err.is_empty() {
                error!("{err}");
            }
            let dueto = dueto.unwrap_or(fl!("debug"));
            due_to!("{dueto}");
            1
        }
    };

    terminal_ring();
    unlock_oma().ok();

    exit(code);
}

fn try_main() -> Result<i32, OutputError> {
    let cmd = args::command_builder();
    let matches = cmd.get_matches();

    // Egg
    if matches.get_count("ailurus") == 3 {
        AILURUS.store(true, Ordering::Relaxed);
    } else if matches.get_count("ailurus") != 0 {
        println!(
            "{} unexpected argument '{}' found\n",
            style("error:").red().bold(),
            style("\x1b[33m--ailurus\x1b[0m").bold()
        );
        println!("{}: oma <COMMAND>\n", style("Usage").bold().underlined());
        println!("For more information, try '{}'.", style("--help").bold());

        return Ok(3);
    }

    let dry_run = matches!(
        matches
            .subcommand()
            .map(|(_, x)| x.try_get_one::<bool>("dry_run")),
        Some(Ok(Some(true)))
    );

    // --no-color option
    if matches.get_flag("no_color")
        || matches!(
            matches.subcommand().map(|(_, x)| x.try_get_one("no_color")),
            Some(Ok(Some(true)))
        )
    {
        std::env::set_var("NO_COLOR", "");
    }

    // Init debug flag
    if matches.get_flag("debug")
        || matches!(
            matches.subcommand().map(|(_, x)| x.try_get_one("debug")),
            Some(Ok(Some(true)))
        )
        || dry_run
    {
        DEBUG.store(true, Ordering::Relaxed);
    }

    // --no-progress
    let no_progress = matches.get_flag("no_progress")
        || matches!(
            matches
                .subcommand()
                .map(|(_, x)| x.try_get_one("no_progress")),
            Some(Ok(Some(true)))
        );

    debug!("oma version: {}", env!("CARGO_PKG_VERSION"));
    debug!("OS: {:?}", OsRelease::new());

    // Init config file
    let config = Config::read()?;

    let pkgs_getter = |args: &ArgMatches| {
        args.get_many::<String>("packages")
            .map(|x| x.map(|x| x.to_owned()).collect::<Vec<_>>())
    };

    let exit_code = match matches.subcommand() {
        Some(("install", args)) => {
            let pkgs_unparse = pkgs_getter(args).unwrap_or_default();

            let args = InstallArgs {
                no_refresh: args.get_flag("no_refresh"),
                install_dbg: args.get_flag("install_dbg"),
                reinstall: args.get_flag("reinstall"),
                no_fixbroken: args.get_flag("no_fix_broken"),
                yes: args.get_flag("yes"),
                force_yes: args.get_flag("force_yes"),
                force_confnew: args.get_flag("force_confnew"),
                dpkg_force_all: args.get_flag("dpkg_force_all"),
                install_recommends: args.get_flag("install_recommends"),
                install_suggests: args.get_flag("install_suggests"),
                no_install_recommends: args.get_flag("no_install_recommends"),
                no_install_suggests: args.get_flag("no_install_recommends"),
            };

            let network_thread = config.network_thread();

            install::execute(pkgs_unparse, args, dry_run, network_thread, no_progress, config.pure_db())?
        }
        Some(("upgrade", args)) => {
            let pkgs_unparse = pkgs_getter(args).unwrap_or_default();

            let args = UpgradeArgs {
                yes: args.get_flag("yes"),
                force_yes: args.get_flag("force_yes"),
                force_confnew: args.get_flag("force_confnew"),
                dpkg_force_all: args.get_flag("dpkg_force_all"),
            };

            upgrade::execute(pkgs_unparse, args, dry_run, no_progress, config.pure_db())?
        }
        Some(("download", args)) => {
            let keyword = pkgs_getter(args).unwrap_or_default();
            let keyword = keyword.iter().map(|x| x.as_str()).collect::<Vec<_>>();

            let path = args
                .get_one::<String>("path")
                .cloned()
                .map(|x| PathBuf::from(&x));

            download::execute(keyword, path, dry_run, no_progress)?
        }
        Some(("remove", args)) => {
            let pkgs_unparse = pkgs_getter(args).unwrap();
            let pkgs_unparse = pkgs_unparse.iter().map(|x| x.as_str()).collect::<Vec<_>>();

            let args = RemoveArgs {
                yes: args.get_flag("yes"),
                remove_config: args.get_flag("remove_config"),
                no_autoremove: args.get_flag("no_autoremove"),
                force_yes: args.get_flag("force_yes"),
            };

            let protect_essentials = config
                .general
                .as_ref()
                .map(|x| x.protect_essentials)
                .unwrap_or_else(GeneralConfig::default_protect_essentials);

            remove::execute(
                pkgs_unparse,
                args,
                dry_run,
                protect_essentials,
                config.network_thread(),
                no_progress,
            )?
        }
        Some(("refresh", _)) => refresh::execute(no_progress, config.pure_db())?,
        Some(("show", args)) => {
            let pkgs_unparse = pkgs_getter(args).unwrap_or_default();
            let pkgs_unparse = pkgs_unparse.iter().map(|x| x.as_str()).collect::<Vec<_>>();
            let all = args.get_flag("all");

            show::execute(all, pkgs_unparse)?
        }
        Some(("search", args)) => {
            let args = args
                .get_many::<String>("pattern")
                .map(|x| x.map(|x| x.to_owned()).collect::<Vec<_>>())
                .unwrap();

            search::execute(&args, no_progress)?
        }
        Some((x, args)) if x == "files" || x == "provides" => {
            let arg = if x == "files" { "package" } else { "pattern" };
            let pkg = args.get_one::<String>(arg).unwrap();
            let is_bin = args.get_flag("bin");

            contents_find::execute(x, is_bin, pkg, no_progress)?
        }
        Some(("fix-broken", _)) => {
            let network_thread = config.network_thread();
            fix_broken::execute(dry_run, network_thread, no_progress)?
        }
        Some(("pick", args)) => {
            let pkg_str = args.get_one::<String>("package").unwrap();
            let network_thread = config.network_thread();

            pick::execute(
                pkg_str,
                args.get_flag("no_refresh"),
                dry_run,
                network_thread,
                no_progress,
                config.pure_db()
            )?
        }
        Some(("mark", args)) => {
            let op = args.get_one::<String>("action").unwrap();

            let pkgs = pkgs_getter(args).unwrap();
            let dry_run = args.get_flag("dry_run");

            mark::execute(op, pkgs, dry_run)?
        }
        Some(("command-not-found", args)) => {
            command_not_found::execute(args.get_one::<String>("package").unwrap())?
        }
        Some(("list", args)) => {
            let pkgs = pkgs_getter(args).unwrap_or_default();
            let all = args.get_flag("all");
            let installed = args.get_flag("installed");
            let upgradable = args.get_flag("upgradable");

            list::execute(all, installed, upgradable, pkgs)?
        }
        Some(("depends", args)) => {
            let pkgs = pkgs_getter(args).unwrap();

            depends::execute(pkgs)?
        }
        Some(("rdepends", args)) => {
            let pkgs = pkgs_getter(args).unwrap();

            rdepends::execute(pkgs)?
        }
        Some(("clean", _)) => clean::execute(no_progress)?,
        Some(("history", _)) => subcommand::history::execute()?,
        Some(("undo", _)) => {
            let network_thread = config.network_thread();
            undo::execute(network_thread, no_progress)?
        }
        #[cfg(feature = "aosc")]
        Some(("topics", args)) => {
            let opt_in = args
                .get_many::<String>("opt_in")
                .map(|x| x.map(|x| x.to_owned()).collect::<Vec<_>>())
                .unwrap_or_default();

            let opt_out = args
                .get_many::<String>("opt_out")
                .map(|x| x.map(|x| x.to_owned()).collect::<Vec<_>>())
                .unwrap_or_default();

            let network_thread = config.network_thread();

            topics::execute(opt_in, opt_out, dry_run, network_thread, no_progress, config.pure_db())?
        }
        Some(("pkgnames", args)) => {
            let keyword = args.get_one::<String>("keyword").map(|x| x.as_str());

            pkgnames::execute(keyword)?
        }
        Some((cmd, args)) => {
            let exe_dir = std::env::current_exe()?;
            let exe_dir = exe_dir.parent().expect("Where am I?");
            let plugin = exe_dir.join(format!("oma-{}", cmd));
            if !plugin.is_file() {
                return Err(OutputError::from(anyhow!("Unknown command: `{cmd}'.")));
            }
            info!("Executing applet oma-{cmd}");
            let mut process = &mut Command::new(plugin);
            if let Some(args) = args.get_many::<String>("COMMANDS") {
                process = process.args(args);
            }
            let status = process.status().unwrap().code().unwrap();
            if status != 0 {
                error!("Applet exited with error {status}");
            }

            return Ok(status);
        }
        None => unreachable!(),
    };

    Ok(exit_code)
}

fn single_handler() {
    // Kill subprocess
    let subprocess_pid = SUBPROCESS.load(Ordering::Relaxed);
    let allow_ctrlc = ALLOWCTRLC.load(Ordering::Relaxed);
    if subprocess_pid > 0 {
        let pid = nix::unistd::Pid::from_raw(subprocess_pid);
        signal::kill(pid, signal::SIGTERM).expect("Failed to kill child process.");
        if !allow_ctrlc {
            info!("{}", fl!("user-aborted-op"));
        } else {
            std::process::exit(0);
        }
    }

    // Dealing with lock
    if LOCKED.load(Ordering::Relaxed) {
        unlock_oma().expect("Failed to unlock instance.");
    }

    // Show cursor before exiting.
    // This is not a big deal so we won't panic on this.
    let _ = WRITER.show_cursor();

    std::process::exit(2);
}
