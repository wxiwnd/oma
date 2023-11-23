use oma_console::info;
use oma_pm::{
    apt::{OmaApt, OmaAptArgsBuilder},
    pkginfo::PkgInfo,
};

use crate::error::OutputError;

use super::utils::handle_no_result;
use crate::fl;

pub fn execute(all: bool, pkgs_unparse: Vec<&str>) -> Result<i32, OutputError> {
    let oma_apt_args = OmaAptArgsBuilder::default().build()?;
    let mut apt = OmaApt::new(vec![], oma_apt_args, false)?;
    let (pkgs, no_result) = apt.select_pkg(&pkgs_unparse, false, false, false)?;
    handle_no_result(no_result);

    if !all {
        let mut filter_pkgs: Vec<PkgInfo> = vec![];
        let pkgs_len = pkgs.len();
        for pkg in pkgs {
            if filter_pkgs
                .iter()
                .find(|x| pkg.raw_pkg.name() == x.raw_pkg.name())
                .is_none()
            {
                filter_pkgs.push(pkg);
            }
        }

        if filter_pkgs.is_empty() {
            return Ok(1);
        }

        for (i, pkg) in filter_pkgs.iter().enumerate() {
            pkg.print_info(&apt.cache);
            if i != filter_pkgs.len() - 1 {
                println!()
            }
        }

        if filter_pkgs.len() == 1 {
            let other_version = pkgs_len - 1;

            if other_version > 0 {
                info!("{}", fl!("additional-version", len = other_version));
            }
        }
    } else {
        for (i, c) in pkgs.iter().enumerate() {
            if i != pkgs.len() - 1 {
                c.print_info(&apt.cache);
                println!();
            } else {
                c.print_info(&apt.cache);
            }
        }
    }

    Ok(0)
}
