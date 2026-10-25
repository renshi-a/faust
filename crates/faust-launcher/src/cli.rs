//! The `faust` command for app developers.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

use crate::build::{Project, Sink, Toolchain};
use crate::config::Config;
use crate::package::{self, Source};
use crate::proc::Limits;

/// Ship macOS apps your users can repair.
#[derive(Debug, Parser)]
#[command(name = "faust", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a new repairable app.
    New(NewArgs),
    /// Build the app into dist/<name>.app, without Xcode, like the user's Mac will.
    Build(BuildArgs),
    /// Run the app's tests, linked like the user's Mac will link them.
    Test(ProjectArgs),
    /// Make the .app users download: the launcher and the app's source.
    Package(PackageArgs),
    /// Package the working tree and open it, as a user would.
    Dev(DevArgs),
    /// Sign, notarize and staple a packaged app, and make the .dmg to hand out.
    Release(crate::release::ReleaseArgs),
}

#[derive(Debug, Args)]
struct ProjectArgs {
    /// The app's repository (where faust.toml is).
    #[arg(long, default_value = ".")]
    path: PathBuf,
    /// Cargo's output folder.
    #[arg(long, hide = true)]
    target_dir: Option<PathBuf>,
    /// Append the output here instead of target/faust/build.log.
    #[arg(long, hide = true)]
    log: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct BuildArgs {
    #[command(flatten)]
    project: ProjectArgs,
    /// CFBundleVersion of the built app.
    #[arg(long, hide = true, default_value_t = 0)]
    build_number: u64,
}

#[derive(Debug, Args)]
struct PackageArgs {
    #[arg(long, default_value = ".")]
    path: PathBuf,
    /// Package the working tree (uncommitted changes too) instead of HEAD.
    #[arg(long)]
    worktree: bool,
    /// Where to put the .app.
    #[arg(long, default_value = "dist/release")]
    out: PathBuf,
    /// Also make a .dmg.
    #[arg(long)]
    dmg: bool,
}

#[derive(Debug, Args)]
struct DevArgs {
    #[arg(long, default_value = ".")]
    path: PathBuf,
    /// Install Rust into the app's own folder, as on a user's Mac, instead of using yours.
    #[arg(long)]
    fresh: bool,
}

#[derive(Debug, Args)]
struct NewArgs {
    /// The folder to create; its name becomes the app's name unless --name says otherwise.
    path: PathBuf,
    /// The app's name, shown to users.
    #[arg(long)]
    name: Option<String>,
    /// The bundle identifier.
    #[arg(long)]
    id: Option<String>,
}

pub(crate) fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::New(a) => crate::template::create(&a.path, a.name.as_deref(), a.id.as_deref()),
        Command::Build(a) => build(&a),
        Command::Test(a) => test(&a),
        Command::Package(a) => package_cmd(&a).map(|_| ()),
        Command::Dev(a) => crate::dev::run(&a.path, a.fresh),
        Command::Release(a) => crate::release::run(&a),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Loads `faust.toml` and works out where things go.
fn open(args: &ProjectArgs) -> Result<(PathBuf, Config, PathBuf, PathBuf)> {
    let root = args.path.canonicalize().with_context(|| format!("{} not found", args.path.display()))?;
    let config = Config::load(&root)?;
    let target = match &args.target_dir {
        Some(t) => t.clone(),
        None => std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from),
    };
    let log = match &args.log {
        Some(log) => log.clone(),
        None => {
            // The last command's output only.
            let log = target.join("faust/build.log");
            std::fs::create_dir_all(log.parent().expect("a log file inside a folder"))?;
            std::fs::write(&log, "")?;
            log
        }
    };
    Ok((root, config, target, log))
}

fn print_line(line: &str) {
    eprintln!("{line}");
}

fn build(args: &BuildArgs) -> Result<()> {
    let (root, config, target_dir, log) = open(&args.project)?;
    let toolchain = Toolchain::from_env(&config).resolve()?;
    let project = Project { root: &root, config: &config, toolchain: &toolchain, target_dir };
    project.build(args.build_number, &mut Sink { log: &log, limits: Limits::none(), on_line: &mut print_line })?;
    Ok(())
}

fn test(args: &ProjectArgs) -> Result<()> {
    let (root, config, target_dir, log) = open(args)?;
    let toolchain = Toolchain::from_env(&config).resolve()?;
    let project = Project { root: &root, config: &config, toolchain: &toolchain, target_dir };
    project.test(&mut Sink { log: &log, limits: Limits::none(), on_line: &mut print_line })
}

fn package_cmd(args: &PackageArgs) -> Result<PathBuf> {
    let root = args.path.canonicalize().with_context(|| format!("{} not found", args.path.display()))?;
    let out_dir = if args.out.is_absolute() { args.out.clone() } else { root.join(&args.out) };
    let source = if args.worktree { Source::WorkingTree } else { Source::Head };
    let bundle = package::package(&package::Options { root, source, out_dir, dmg: args.dmg, development: false })?;
    println!("{}", bundle.display());
    Ok(bundle)
}

/// For `faust dev`: packages the working tree into `out`.
pub(crate) fn package_worktree(root: &Path, out: &Path) -> Result<PathBuf> {
    package::package(&package::Options {
        root: root.to_owned(),
        source: Source::WorkingTree,
        out_dir: out.to_owned(),
        dmg: false,
        development: true,
    })
}
