//! `rust-img-vhdx`: the command-line tool for VHDX images, one multi-call
//! binary.
//!
//! Installed as `rust-img-vhdx` and linked as `img.vhdx`; see `common` for
//! the dispatch and the output contract every tool shares, and `vhdx` for
//! the tool itself.

// The shared plumbing is a library in waiting (see its module docs): its
// API is whole, and a piece this repository does not call yet is not dead,
// it is the part another format's tools will.
#[allow(dead_code)]
mod common;
mod vhdx;

use std::process::ExitCode;

static FAMILY: common::Family = common::Family {
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
    common::main(&FAMILY)
}
