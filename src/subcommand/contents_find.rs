use std::path::Path;

use dialoguer::console;
use oma_console::WRITER;
use oma_console::{indicatif::ProgressBar, pb::oma_spinner};
use oma_contents::{ContentsEvent, QueryMode};
use oma_utils::dpkg::dpkg_arch;

use crate::error::OutputError;
use crate::fl;
use crate::table::oma_display;

pub fn execute(x: &str, is_bin: bool, pkg: &str) -> Result<i32, OutputError> {
    let pb = ProgressBar::new_spinner();
    let (style, inv) = oma_spinner(false).unwrap();
    pb.set_style(style);
    pb.enable_steady_tick(inv);
    pb.set_message(fl!("searching"));

    let query_mode = match x {
        "files" => QueryMode::ListFiles(is_bin),
        "provides" => QueryMode::Provides(is_bin),
        _ => unreachable!(),
    };

    let arch = dpkg_arch()?;

    let res = oma_contents::find(
        pkg,
        query_mode,
        Path::new("/var/lib/apt/lists"),
        &arch,
        move |c| match c {
            ContentsEvent::Progress(c) => {
                pb.set_message(fl!("search-with-result-count", count = c))
            }
            ContentsEvent::ContentsMayNotBeAccurate => {
                WRITER
                    .writeln_with_pb(
                        &pb,
                        &console::style("WARNING").yellow().bold().to_string(),
                        &fl!("contents-may-not-be-accurate-1"),
                    )
                    .unwrap();
                WRITER
                    .writeln_with_pb(
                        &pb,
                        &console::style("INFO").blue().bold().to_string(),
                        &fl!("contents-may-not-be-accurate-2"),
                    )
                    .unwrap();
            }
            ContentsEvent::Done => pb.finish_and_clear(),
        },
        arch != "mips64r6el",
    )?;

    let mut pager = oma_display(false, res.len())?;
    let mut out = pager.get_writer()?;

    for (_, v) in res {
        writeln!(out, "{v}").ok();
    }

    drop(out);
    pager.wait_for_exit()?;

    Ok(0)
}