//! `rust-img-vhdx`: the command-line tool for VHDX images, one multi-call
//! binary.
//!
//! Installed as `rust-img-vhdx` and linked as `img.vhdx`. The dispatch and the
//! output contract every tool shares are `fs_core::cli` (rust-fs-core's `cli`
//! feature); `vhdx` is the tool itself.

mod vhdx;

use fs_core::cli;
use std::process::ExitCode;

static FAMILY: cli::Family = cli::Family {
    repo: "rust-img-vhdx",
    crate_name: env!("CARGO_PKG_NAME"),
    version: env!("CARGO_PKG_VERSION"),
    about: "VHDX tools: report, read and write a VHDX disk image without a hypervisor",
    install_hints: &[
        "`chore cli:install` from a checkout of this repository",
        "`brew install antimatter-studios/tap/rust-img-vhdx`",
    ],
    tools: &[vhdx::img::TOOL],
};

fn main() -> ExitCode {
    cli::main(&FAMILY)
}
