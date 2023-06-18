use std::{
    io::{Read, Seek, SeekFrom, Write},
    sync::atomic::Ordering,
};

use crate::{
    cli::InstallOptions,
    error, handle_install_error, info,
    oma::{apt_handler, Action, InstallError, InstallResult, Oma},
    pkg::{mark_delete, mark_install},
    utils::needs_root,
    ARGS, DRYRUN, TIME_OFFSET,
};
use crate::{fl, success};
use anyhow::{anyhow, Context, Result};
use dialoguer::{theme::ColorfulTheme, Select};
use rust_apt::{cache::Cache, config::Config as AptConfig, new_cache};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

const HISTORY_DB_FILE: &str = "/var/log/oma/history.json";

#[derive(Serialize, Deserialize, Debug)]
struct History {
    start_date: String,
    end_date: String,
    args: String,
    action: Action,
    op: Operation,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub enum Operation {
    Undo,
    Redo,
    Other,
}

pub fn log_to_file(
    action: &Action,
    start_time: &str,
    end_time: &str,
    op: Operation,
    success: bool,
) -> Result<()> {
    if DRYRUN.load(Ordering::Relaxed) {
        return Ok(());
    }

    std::fs::create_dir_all("/var/log/oma")
        .map_err(|e| anyhow!(fl!("can-not-create-oma-log-dir", e = e.to_string())))?;

    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open("/var/log/oma/history")
        .map_err(|e| anyhow!(fl!("can-not-create-oma-log", e = e.to_string())))?;

    f.write_all(format!("Start-Date: {start_time}\n").as_bytes())?;
    f.write_all(format!("Action: {}\n{action:#?}", *ARGS).as_bytes())?;
    f.write_all(format!("Status: {}", if success { "Success" } else { "Failed" }).as_bytes())?;
    f.write_all(format!("End-Date: {end_time}\n\n").as_bytes())?;

    drop(f);

    if !action.is_empty() {
        let json = History {
            start_date: start_time.to_string(),
            end_date: end_time.to_string(),
            args: (*ARGS.clone()).to_string(),
            action: action.clone(),
            op: op.clone(),
        };

        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(HISTORY_DB_FILE)
            .map_err(|e| anyhow!(fl!("can-not-create-oma-log-database", e = e.to_string())))?;

        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;

        let mut history_db: Vec<History> = if !buf.is_empty() {
            serde_json::from_reader(&*buf)
                .map_err(|e| anyhow!(fl!("can-not-read-oma-log-database", e = e.to_string())))?
        } else {
            vec![]
        };

        history_db.insert(0, json);

        let buf = serde_json::to_vec(&history_db)
            .map_err(|e| anyhow!(fl!("can-not-ser-oma-log-database", e = e.to_string())))?;

        f.seek(SeekFrom::Start(0))?;
        f.write_all(&buf)?;

        success!("{}", fl!("history-tips-1"));
        let op = match op {
            Operation::Undo => "redo",
            Operation::Redo | Operation::Other => "undo",
        };
        info!("{}", fl!("history-tips-2", undo_or_redo = op));
    }

    Ok(())
}

pub fn run(index: Option<usize>, is_undo: bool) -> Result<i32> {
    needs_root()?;

    let buf = std::fs::read(HISTORY_DB_FILE)
        .map_err(|e| anyhow!(fl!("can-not-read-oma-log-database", e = e.to_string())))?;

    let db: Vec<History> = serde_json::from_reader(&*buf)
        .map_err(|e| anyhow!(fl!("can-not-deser-oma-log-database", e = e.to_string())))?;

    let db: Vec<_> = if is_undo {
        db.iter()
            .filter(|x| x.op == Operation::Other || x.op == Operation::Redo)
            .collect()
    } else {
        db.iter().filter(|x| x.op == Operation::Undo).collect()
    };

    let action = if let Some(index) = index {
        let history = db.get(index).context(fl!("invaild-index", index = index))?;

        let action = &history.action;
        if action.is_empty() {
            info!("{}", fl!("index-is-nothing", index = index));
            return Ok(0);
        }

        action
    } else {
        let theme = ColorfulTheme::default();
        let mut dialoguer = Select::with_theme(&theme);

        let db_with_args = db
            .iter()
            .enumerate()
            .map(|(i, x)| {
                let desc = x.action.get_description();
                format!(
                    "[{}]: {} ({}){}",
                    i + 1,
                    x.args,
                    if desc.len() <= 3 {
                        desc.join(",")
                    } else {
                        desc[..3].join(",")
                    },
                    if desc.len() <= 3 { "" } else { "..." }
                )
            })
            .collect::<Vec<_>>();

        dialoguer.items(&db_with_args);
        dialoguer.with_prompt(if is_undo {
            fl!("select-op-undo")
        } else {
            fl!("select-op-redo")
        });

        dialoguer.default(0);
        let index = dialoguer.interact()?;

        let history = db.get(index).unwrap();

        let action = &history.action;
        if action.is_empty() {
            info!("{}", fl!("index-is-nothing", index = index));
            return Ok(0);
        }

        action
    };

    let mut count = 1;

    let start_time = OffsetDateTime::now_utc()
        .to_offset(*TIME_OFFSET)
        .to_string();

    let op = if is_undo {
        Operation::Undo
    } else {
        Operation::Redo
    };

    handle_install_error!(do_inner(action, count), count, start_time, op)
}

fn do_inner(action: &Action, count: usize) -> InstallResult<Action> {
    let cache = new_cache!().map_err(|e| anyhow!("{e}"))?;
    let (action, len) = undo_inner(action, &cache)?;

    Oma::build_async_runtime()?.action_to_install(
        AptConfig::new_clear(),
        action.clone(),
        count,
        cache,
        len,
        &InstallOptions::default(),
    )?;

    Ok(action)
}

fn undo_inner(action: &Action, cache: &Cache) -> Result<(Action, usize), InstallError> {
    for i in &action.update {
        let pkg = cache.get(&i.name_no_color);
        if let Some(pkg) = pkg {
            if let Some(v) = pkg.get_version(i.old_version.as_ref().unwrap()) {
                mark_install(cache, pkg.name(), v.unique(), false, false, None)?;
                continue;
            }
        }

        error!(
            "{}",
            fl!(
                "can-not-get-pkg-version-from-database",
                name = i.name_no_color.to_string(),
                version = i.old_version.as_ref().unwrap().to_string()
            )
        );
    }
    for i in &action.downgrade {
        let pkg = cache.get(&i.name_no_color);
        if let Some(pkg) = pkg {
            if let Some(v) = pkg.get_version(i.old_version.as_ref().unwrap()) {
                mark_install(cache, pkg.name(), v.unique(), false, false, None)?;
                continue;
            }
        }

        error!(
            "{}",
            fl!(
                "can-not-get-pkg-version-from-database",
                name = i.name_no_color.to_string(),
                version = i.old_version.as_ref().unwrap().to_string()
            )
        );
    }
    for i in &action.del {
        let pkg = cache.get(&i.name_no_color);
        if let Some(pkg) = pkg {
            if let Some(v) = pkg.get_version(&i.version) {
                mark_install(cache, pkg.name(), v.unique(), false, false, None)?;
                continue;
            }
        }

        error!(
            "{}",
            fl!(
                "can-not-get-pkg-version-from-database",
                name = i.name_no_color.to_string(),
                version = i.version.to_string()
            )
        );
    }
    for i in &action.install {
        let pkg = cache.get(&i.name_no_color);
        if let Some(pkg) = pkg {
            if let Some(v) = pkg.installed() {
                if v.version() == i.new_version {
                    mark_delete(&pkg, false)?;
                    continue;
                }
            }
        }

        error!(
            "{}",
            fl!(
                "can-not-get-pkg-version-from-database",
                name = i.name_no_color.to_string(),
                version = i.version.to_string()
            )
        );
    }

    let (action, len, _) = apt_handler(cache, false, false, true)?;

    Ok((action, len))
}