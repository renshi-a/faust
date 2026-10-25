//! Faust: ship macOS apps your users can repair.
//!
//! One executable, two jobs. Inside a packaged app it is the launcher that builds, starts,
//! changes and rolls back the app on the user's Mac ([`launcher`]). On the command line it
//! is the `faust` command app developers use to create, build, package and release apps
//! ([`cli`]).

// First, so that `tr!` is in scope in the modules below.
#[macro_use]
mod i18n;

mod build;
mod cli;
mod config;
mod dev;
mod launcher;
mod objc;
mod package;
mod proc;
mod release;
mod repo;
mod template;
mod ui;

use std::process::ExitCode;

fn main() -> ExitCode {
    if launcher::is_bundled_launch() { launcher::main() } else { cli::main() }
}
