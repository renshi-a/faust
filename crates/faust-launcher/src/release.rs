//! `faust release`: packages HEAD, signs it with a Developer ID, notarizes and staples it, and
//! makes the signed, notarized disk image to hand out. Runs on the developer's Mac, which
//! needs the Command Line Tools for `notarytool` and `stapler`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use clap::Args;

use crate::config::Config;
use crate::package::{self, Source};

#[derive(Debug, Args)]
pub(crate) struct ReleaseArgs {
    #[arg(long, default_value = ".")]
    path: PathBuf,
    /// The signing identity; defaults to the first "Developer ID Application" in the keychain.
    #[arg(long, env = "FAUST_SIGN_IDENTITY")]
    identity: Option<String>,
    /// A notarytool keychain profile (see `xcrun notarytool store-credentials`).
    #[arg(long, env = "FAUST_NOTARY_PROFILE", default_value = "faust-notary")]
    notary_profile: String,
    /// Instead of a profile: an App Store Connect API key (.p8), for CI.
    #[arg(long, env = "FAUST_NOTARY_KEY", requires_all = ["notary_key_id", "notary_issuer"])]
    notary_key: Option<PathBuf>,
    #[arg(long, env = "FAUST_NOTARY_KEY_ID")]
    notary_key_id: Option<String>,
    #[arg(long, env = "FAUST_NOTARY_ISSUER")]
    notary_issuer: Option<String>,
}

impl ReleaseArgs {
    fn notary_auth(&self) -> Vec<String> {
        match (&self.notary_key, &self.notary_key_id, &self.notary_issuer) {
            (Some(key), Some(id), Some(issuer)) => vec![
                "--key".into(),
                key.display().to_string(),
                "--key-id".into(),
                id.clone(),
                "--issuer".into(),
                issuer.clone(),
            ],
            _ => vec!["--keychain-profile".into(), self.notary_profile.clone()],
        }
    }
}

pub(crate) fn run(args: &ReleaseArgs) -> Result<()> {
    let root = args.path.canonicalize().with_context(|| format!("{} not found", args.path.display()))?;
    let config = Config::load(&root)?;
    let out = root.join("dist/release");
    let work = root.join("target/faust/release");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let identity = match &args.identity {
        Some(i) => i.clone(),
        None => find_identity()?,
    };
    let auth = args.notary_auth();
    let name = &config.app.name;

    say("package HEAD");
    let app = package::package(&package::Options {
        root: root.clone(),
        source: Source::Head,
        out_dir: out.clone(),
        dmg: false,
        development: false,
    })?;

    say(&format!("sign the app as {identity}"));
    // The launcher is the bundle's only executable; the source is sealed as resources.
    tool("/usr/bin/codesign", &["--force", "--options", "runtime", "--timestamp", "--sign", &identity, &path(&app)])?;
    tool("/usr/bin/codesign", &["--verify", "--strict", "--verbose=2", &path(&app)])?;

    let zip = work.join(format!("{name}.zip"));
    tool("/usr/bin/ditto", &["-c", "-k", "--keepParent", &path(&app), &path(&zip)])?;
    notarize(&zip, &auth)?;
    staple(&app)?;

    say("disk image");
    let dmg = out.join(format!("{name}.dmg"));
    let stage = work.join("dmg");
    std::fs::create_dir_all(&stage)?;
    tool("/bin/cp", &["-R", &path(&app), &path(&stage)])?;
    std::os::unix::fs::symlink("/Applications", stage.join("Applications"))?;
    let _ = std::fs::remove_file(&dmg);
    tool(
        "/usr/bin/hdiutil",
        &["create", "-quiet", "-volname", name, "-srcfolder", &path(&stage), "-ov", "-format", "UDZO", &path(&dmg)],
    )?;
    tool("/usr/bin/codesign", &["--force", "--timestamp", "--sign", &identity, &path(&dmg)])?;
    notarize(&dmg, &auth)?;
    staple(&dmg)?;

    say("what Gatekeeper says");
    tool("/usr/bin/xcrun", &["stapler", "validate", &path(&app)])?;
    tool("/usr/bin/xcrun", &["stapler", "validate", &path(&dmg)])?;
    tool("/usr/sbin/spctl", &["--assess", "--verbose=2", "--type", "execute", &path(&app)])?;
    tool(
        "/usr/sbin/spctl",
        &["--assess", "--verbose=2", "--type", "open", "--context", "context:primary-signature", &path(&dmg)],
    )?;
    let sha = crate::proc::sha256_file(&dmg)?;
    std::fs::write(out.join(format!("{name}.dmg.sha256")), format!("{sha}  {name}.dmg\n"))?;
    println!("{} ({sha})", dmg.display());
    Ok(())
}

fn say(step: &str) {
    eprintln!("== {step}");
}

fn path(p: &Path) -> String {
    p.display().to_string()
}

fn tool(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program).args(args).status().with_context(|| format!("running {program}"))?;
    ensure!(status.success(), "{program} {} failed ({status})", args.join(" "));
    Ok(())
}

fn find_identity() -> Result<String> {
    let out = Command::new("/usr/bin/security").args(["find-identity", "-v", "-p", "codesigning"]).output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find_map(|l| {
            let start = l.find("\"Developer ID Application: ")?;
            let rest = &l[start + 1..];
            Some(rest[..rest.find('"')?].to_owned())
        })
        .context("no \"Developer ID Application\" identity in the keychain; pass --identity")
}

/// Submits to Apple's notary service and waits; prints the log when it is refused.
fn notarize(file: &Path, auth: &[String]) -> Result<()> {
    say(&format!("notarize {} (this waits for Apple, usually a few minutes)", file.display()));
    let mut cmd = Command::new("/usr/bin/xcrun");
    cmd.args(["notarytool", "submit"]).arg(file).args(auth).args([
        "--wait",
        "--timeout",
        "1h",
        "--output-format",
        "json",
    ]);
    let out = cmd.output().context("running notarytool")?;
    let result: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let (id, status) = (result["id"].as_str().unwrap_or("?"), result["status"].as_str().unwrap_or("no answer"));
    eprintln!("   submission {id}: {status}");
    if status != "Accepted" {
        eprintln!("{}", String::from_utf8_lossy(&out.stdout));
        if id != "?" {
            let _ = Command::new("/usr/bin/xcrun").args(["notarytool", "log", id]).args(auth).status();
        }
        bail!("notarization of {} was not accepted", file.display());
    }
    Ok(())
}

/// Right after notarizing, or when the connection to Apple drops, stapling can fail for a while.
fn staple(file: &Path) -> Result<()> {
    for attempt in 1..=5u64 {
        if Command::new("/usr/bin/xcrun").args(["stapler", "staple"]).arg(file).status()?.success() {
            return Ok(());
        }
        eprintln!("   stapling failed, trying again in {} s", attempt * 15);
        std::thread::sleep(Duration::from_secs(attempt * 15));
    }
    bail!("could not staple {}", file.display())
}
