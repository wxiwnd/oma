use std::borrow::Cow;

use oma_pm::apt::{OmaApt, OmaAptArgsBuilder};

use crate::error::OutputError;

use super::utils::handle_no_result;

pub fn execute(pkgs: Vec<String>) -> Result<i32, OutputError> {
    let oma_apt_args = OmaAptArgsBuilder::default().build()?;
    let mut apt = OmaApt::new(vec![], oma_apt_args, false)?;

    let (pkgs, no_result) = apt.select_pkg(
        pkgs.iter().map(|x| x.as_str()).collect::<Vec<_>>(),
        false,
        true,
    )?;
    handle_no_result(no_result);

    for pkg in pkgs {
        println!("{}:", pkg.raw_pkg.name());
        let all_deps = pkg.deps;

        for (k, v) in all_deps {
            for dep in v.inner() {
                for b_dep in dep {
                    let s = if let Some(comp_ver) = b_dep.comp_ver {
                        Cow::Owned(format!("({comp_ver})"))
                    } else {
                        Cow::Borrowed("")
                    };

                    println!("  {k}: {} {}", b_dep.name, s);
                }
            }
        }
    }

    Ok(0)
}