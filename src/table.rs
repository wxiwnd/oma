use std::fmt::Display;
use std::io::Write;
use std::sync::atomic::Ordering;

use crate::console::style;
use crate::error::OutputError;
use crate::{color_formatter, fl, ALLOWCTRLC};
use oma_console::indicatif::HumanBytes;
use oma_console::pager::{Pager, PagerExit, PagerUIText};
use oma_console::print::Action;
use oma_console::WRITER;
use oma_pm::apt::{InstallEntry, InstallOperation, RemoveEntry, RemoveTag};
use tabled::settings::object::Columns;
use tabled::settings::peaker::PriorityMax;
use tabled::settings::{Alignment, Padding, Style, Width};
use tabled::{Table, Tabled};

#[derive(Debug, Tabled)]
struct InstallEntryDisplay {
    name: String,
    version_delta: String,
    size_delta: String,
}

#[derive(Debug, Tabled)]
struct RemoveEntryDisplay {
    name: String,
    size: String,
    detail: String,
}

impl From<&RemoveEntry> for RemoveEntryDisplay {
    fn from(value: &RemoveEntry) -> Self {
        let name = style(value.name()).red().bold().to_string();
        let mut detail = vec![];
        for i in value.details() {
            match i {
                RemoveTag::Purge => detail.push(fl!("purge-file")),
                RemoveTag::AutoRemove => detail.push(fl!("removed-as-unneed-dep")),
                RemoveTag::Resolver => detail.push(fl!("removed-as-unmet-dep")),
            }
        }

        let mut detail = detail.join(&fl!("semicolon"));

        // 首字大小写转换
        detail.get_mut(0..1).map(|s| {
            s.make_ascii_uppercase();
            &*s
        });

        let size = format!("-{}", HumanBytes(value.size()));

        Self { name, detail, size }
    }
}

impl From<&InstallEntry> for InstallEntryDisplay {
    fn from(value: &InstallEntry) -> Self {
        let name = match value.op() {
            InstallOperation::Install => style(value.name()).green().to_string(),
            InstallOperation::ReInstall => style(value.name()).blue().to_string(),
            InstallOperation::Upgrade => color_formatter()
                .color_str(value.name(), Action::UpgradeTips)
                .to_string(),
            InstallOperation::Downgrade => style(value.name()).yellow().to_string(),
            InstallOperation::Download => value.name().to_string(),
            InstallOperation::Default => unreachable!(),
        };

        let version_delta = if let Some(old_version) = value.old_version() {
            format!("{old_version} -> {}", value.new_version())
        } else {
            value.new_version().to_string()
        };

        let size_delta = if let Some(old_size) = value.old_size() {
            value.new_size() as i64 - old_size as i64
        } else {
            value.new_size() as i64
        };

        let size_delta = if size_delta >= 0 {
            format!("+{}", HumanBytes(size_delta.unsigned_abs()))
        } else {
            format!("-{}", HumanBytes(size_delta.unsigned_abs()))
        };

        Self {
            name,
            version_delta,
            size_delta,
        }
    }
}

pub fn oma_display_with_normal_output(
    is_question: bool,
    len: usize,
) -> Result<Pager<'static>, OutputError> {
    if !is_question {
        ALLOWCTRLC.store(true, Ordering::Relaxed);
    }

    let pager = if len < WRITER.get_height().into() {
        Pager::plain()
    } else {
        Pager::external(
            &OmaPagerUIText { is_question: false },
            None,
            color_formatter(),
        )
        .map_err(|e| OutputError {
            description: "Failed to get pager".to_string(),
            source: Some(Box::new(e)),
        })?
    };

    Ok(pager)
}

struct OmaPagerUIText {
    is_question: bool,
}

impl PagerUIText for OmaPagerUIText {
    fn normal_tips(&self) -> String {
        tips(self.is_question)
    }

    fn search_tips_with_result(&self) -> String {
        fl!("search-tips-with-result")
    }

    fn searct_tips_with_query(&self, query: &str) -> String {
        fl!("search-tips-with-query", query = query)
    }

    fn search_tips_with_empty(&self) -> String {
        fl!("search-tips-with-empty")
    }

    fn search_tips_not_found(&self) -> String {
        fl!("search-tips-not-found")
    }
}

fn tips(is_question: bool) -> String {
    let has_x11 = std::env::var("DISPLAY");
    let has_wayland = std::env::var("WAYLAND_DISPLAY");
    let has_gui = has_x11.is_ok() || has_wayland.is_ok();

    if is_question {
        if has_gui {
            fl!("question-tips-with-gui")
        } else {
            fl!("question-tips")
        }
    } else if has_gui {
        fl!("normal-tips-with-gui")
    } else {
        fl!("normal-tips")
    }
}

pub struct PagerPrinter<W> {
    writer: W,
}

impl<W: Write> PagerPrinter<W> {
    pub fn new(writer: W) -> PagerPrinter<W> {
        PagerPrinter { writer }
    }

    pub fn print<D: Display>(&mut self, d: D) -> std::io::Result<()> {
        writeln!(self.writer, "{d}")
    }

    pub fn print_table<T, I>(&mut self, table: I, header: Vec<&str>) -> std::io::Result<()>
    where
        I: IntoIterator<Item = T>,
        T: Tabled,
    {
        let mut table = {
            let mut t = Table::builder(table);
            t.remove_record(0);
            t.insert_record(0, header);
            t.build()
        };

        table
            .with(Padding::new(2, 2, 0, 0))
            .with(Alignment::left())
            .modify(Columns::new(2..3), Alignment::left())
            .with(Style::psql())
            .with(
                Width::wrap(WRITER.get_length() as usize)
                    .priority::<PriorityMax>()
                    .keep_words(),
            );

        writeln!(self.writer, "{table}")
    }
}

pub fn table_for_install_pending(
    install: &[InstallEntry],
    remove: &[RemoveEntry],
    disk_size: &(Box<str>, u64),
    is_pager: bool,
    dry_run: bool,
) -> Result<PagerExit, OutputError> {
    if dry_run {
        return Ok(PagerExit::NormalExit);
    }

    let mut pager = if is_pager {
        Pager::external(
            &OmaPagerUIText { is_question: true },
            Some(fl!("pending-op")),
            color_formatter(),
        )
        .map_err(|e| OutputError {
            description: "Failed to get pager".to_string(),
            source: Some(Box::new(e)),
        })?
    } else {
        Pager::plain()
    };

    let out = pager.get_writer().map_err(|e| OutputError {
        description: "Failed to get writer".to_string(),
        source: Some(Box::new(e)),
    })?;
    let mut printer = PagerPrinter::new(out);

    if is_pager {
        review_msg(&mut printer);
    }

    print_pending_inner(printer, remove, install, disk_size);
    let exit = pager.wait_for_exit().map_err(|e| OutputError {
        description: "Failed to wait exit".to_string(),
        source: Some(Box::new(e)),
    })?;

    match exit {
        PagerExit::NormalExit if is_pager => {
            let mut pager = Pager::plain();
            let out = pager.get_writer().map_err(|e| OutputError {
                description: "Failed to wait exit".to_string(),
                source: Some(Box::new(e)),
            })?;
            let mut printer = PagerPrinter::new(out);
            printer.print("").ok();
            print_pending_inner(printer, remove, install, disk_size);
            Ok(exit)
        }
        _ => Ok(exit),
    }
}

pub fn table_for_history_pending(
    install: &[InstallEntry],
    remove: &[RemoveEntry],
    disk_size: &(Box<str>, u64),
) -> Result<(), OutputError> {
    let mut pager = Pager::external(
        &OmaPagerUIText { is_question: false },
        Some(fl!("pending-op")),
        color_formatter(),
    )
    .map_err(|e| OutputError {
        description: "Failed to get pager".to_string(),
        source: Some(Box::new(e)),
    })?;

    let out = pager.get_writer().map_err(|e| OutputError {
        description: "Failed to get writer".to_string(),
        source: Some(Box::new(e)),
    })?;
    let mut printer = PagerPrinter::new(out);

    printer.print("\n\n").ok();

    print_pending_inner(printer, remove, install, disk_size);
    pager.wait_for_exit().map_err(|e| OutputError {
        description: "Failed to wait exit".to_string(),
        source: Some(Box::new(e)),
    })?;

    Ok(())
}

fn print_pending_inner<W: Write>(
    mut printer: PagerPrinter<W>,
    remove: &[RemoveEntry],
    install: &[InstallEntry],
    disk_size: &(Box<str>, u64),
) {
    if !remove.is_empty() {
        printer
            .print(format!(
                "{} {}{}\n",
                fl!("count-pkg-has-desc", count = remove.len()),
                style(fl!("removed")).red().bold(),
                fl!("colon")
            ))
            .ok();

        let remove_display = remove
            .iter()
            .map(RemoveEntryDisplay::from)
            .collect::<Vec<_>>();

        printer
            .print_table(
                remove_display,
                vec![
                    fl!("table-name").as_str(),
                    fl!("table-size").as_str(),
                    fl!("table-detail").as_str(),
                ],
            )
            .ok();
        printer.print("\n").ok();
    }

    let total_download_size: u64 = install
        .iter()
        .filter(|x| x.op() == &InstallOperation::Install || x.op() == &InstallOperation::Upgrade)
        .map(|x| x.download_size())
        .sum();

    if !install.is_empty() {
        let install_e = install
            .iter()
            .filter(|x| x.op() == &InstallOperation::Install);

        let install_e_display = install_e.map(InstallEntryDisplay::from).collect::<Vec<_>>();

        if !install_e_display.is_empty() {
            printer
                .print(format!(
                    "{} {}{}\n",
                    fl!("count-pkg-has-desc", count = install_e_display.len()),
                    style(fl!("installed")).green().bold(),
                    fl!("colon")
                ))
                .ok();

            printer
                .print_table(
                    install_e_display,
                    vec![
                        fl!("table-name").as_str(),
                        fl!("table-version").as_str(),
                        fl!("table-size").as_str(),
                    ],
                )
                .ok();
            printer.print("\n").ok();
        }

        let update = install
            .iter()
            .filter(|x| x.op() == &InstallOperation::Upgrade);

        let update_display = update.map(InstallEntryDisplay::from).collect::<Vec<_>>();

        if !update_display.is_empty() {
            printer
                .print(format!(
                    "{} {}{}\n",
                    fl!("count-pkg-has-desc", count = update_display.len()),
                    color_formatter().color_str(fl!("upgraded"), Action::UpgradeTips),
                    fl!("colon")
                ))
                .ok();

            printer
                .print_table(
                    update_display,
                    vec![
                        fl!("table-name").as_str(),
                        fl!("table-version").as_str(),
                        fl!("table-size").as_str(),
                    ],
                )
                .ok();
            printer.print("\n").ok();
        }

        let downgrade = install
            .iter()
            .filter(|x| x.op() == &InstallOperation::Downgrade);

        let downgrade_display = downgrade.map(InstallEntryDisplay::from).collect::<Vec<_>>();

        if !downgrade_display.is_empty() {
            printer
                .print(format!(
                    "{} {}{}\n",
                    fl!("count-pkg-has-desc", count = downgrade_display.len()),
                    style(fl!("downgraded")).yellow().bold(),
                    fl!("colon")
                ))
                .ok();

            printer
                .print_table(
                    downgrade_display,
                    vec![
                        fl!("table-name").as_str(),
                        fl!("table-version").as_str(),
                        fl!("table-size").as_str(),
                    ],
                )
                .ok();
            printer.print("\n").ok();
        }

        let reinstall = install
            .iter()
            .filter(|x| x.op() == &InstallOperation::ReInstall);

        let reinstall_display = reinstall.map(InstallEntryDisplay::from).collect::<Vec<_>>();

        if !reinstall_display.is_empty() {
            printer
                .print(format!(
                    "{} {}{}\n",
                    fl!("count-pkg-has-desc", count = reinstall_display.len()),
                    style(fl!("reinstalled")).blue().bold(),
                    fl!("colon"),
                ))
                .ok();

            printer
                .print_table(
                    reinstall_display,
                    vec![
                        fl!("table-name").as_str(),
                        fl!("table-version").as_str(),
                        fl!("table-size").as_str(),
                    ],
                )
                .ok();
            printer.print("\n").ok();
        }
    }

    printer
        .print(format!(
            "{}{}",
            style(fl!("total-download-size")).bold(),
            HumanBytes(total_download_size)
        ))
        .ok();

    let (symbol, abs_install_size_change) = disk_size;

    printer
        .print(format!(
            "{}{}{}",
            style(fl!("change-storage-usage")).bold(),
            symbol,
            HumanBytes(*abs_install_size_change)
        ))
        .ok();
    printer.print("").ok();
}

fn review_msg<W: Write>(printer: &mut PagerPrinter<W>) {
    printer.print("").ok();
    printer.print(format!("{}\n", fl!("review-msg"))).ok();
    printer
        .print(format!(
            "{}\n",
            fl!(
                "oma-may",
                a = style(fl!("install")).green().to_string(),
                b = style(fl!("remove")).red().to_string(),
                c = color_formatter()
                    .color_str(fl!("upgrade"), Action::UpgradeTips)
                    .to_string(),
                d = style(fl!("downgrade")).yellow().to_string(),
                e = style(fl!("reinstall")).blue().to_string()
            )
        ))
        .ok();

    let has_x11 = std::env::var("DISPLAY");

    let line1 = format!("    {}", fl!("end-review"));
    let line2 = format!("    {}", fl!("cc-to-abort"));

    if has_x11.is_ok() {
        let line3 = format!("    {}\n\n", fl!("how-to-op-with-x"));

        printer.print(format!("{}", style(line1).bold())).ok();
        printer.print(format!("{}", style(line2).bold())).ok();
        printer.print(format!("{}", style(line3).bold())).ok();
    } else {
        let line3 = format!("    {}\n\n", fl!("how-to-op"));

        printer.print(format!("{}", style(line1).bold())).ok();
        printer.print(format!("{}", style(line2).bold())).ok();
        printer.print(format!("{}", style(line3).bold())).ok();
    }
}
