//! The first start: install Rust, put the shipped source in a repository, fetch the crates
//! and build the first version, all in the setup window.

use anyhow::Result;
use gix::ObjectId;

use super::controller::{Controller, short};
use super::paths::Paths;
use super::state::{Kind, Status, Version, now};
use super::{assistant, checks, log, toolchain};
use crate::proc::{self, Limits};
use crate::repo::{MAIN, Signature, UPSTREAM};
use crate::ui::{self, Button, setup};

/// Who official releases are committed by. With the release's own time, the same release
/// makes the same commit on every Mac.
fn official_signature(app: &str, time: i64) -> Signature<'_> {
    Signature { name: app, email: "official@faust.invalid", time }
}

pub(super) fn official_title(version: &str) -> String {
    tr!(en: "Official version {version}", ja: "公式バージョン {version}", de: "Offizielle Version {version}", it: "Versione ufficiale {version}")
}

impl Controller {
    /// Shows the setup window and installs what is missing. False when the user quit.
    pub(super) fn setup(&mut self) -> bool {
        let rust = self.shipped_rust();
        let needs_install = !self.p.system_rust && !toolchain::installed(&self.p, &rust);
        ui::set_regular(true);
        setup::show(self.setup_text(&rust));
        if needs_install || self.state.current.is_none() {
            setup::set_mode(setup::Mode::Consent);
            if self.wait_button(&[Button::Install, Button::Quit]) == Button::Quit {
                return false;
            }
        }
        log("setup starts");
        loop {
            setup::set_mode(setup::Mode::Running);
            match self.setup_steps() {
                Ok(()) => break,
                Err(e) => {
                    log(&format!("setup failed: {e:#}"));
                    let e = short(&format!("{e:#}"), 60);
                    let failed = tr!(en: "Failed: {e}", ja: "失敗しました: {e}", de: "Fehlgeschlagen: {e}", it: "Non riuscito: {e}");
                    setup::progress(0.0, &failed, &short(&proc::tail(&self.p.log("setup.log"), 1), 90));
                    setup::set_mode(setup::Mode::Failed);
                    loop {
                        match self.wait_button(&[Button::Retry, Button::Quit, Button::ShowLog]) {
                            Button::ShowLog => ui::open_text(&self.p.log("setup.log")),
                            Button::Retry => break,
                            _ => return false,
                        }
                    }
                }
            }
        }
        setup::set_mode(setup::Mode::Done);
        if self.wait_button(&[Button::Start, Button::Quit]) == Button::Quit {
            return false;
        }
        setup::close();
        ui::set_regular(false);
        true
    }

    fn setup_text(&self, rust: &str) -> setup::Text {
        let app = self.app_name();
        let description = self.shipped.config.description();
        let intro = if description.is_empty() { String::new() } else { format!("{description}\n\n") };
        let welcome = tr!(
            en: "{intro}{app} can change itself. When you ask for a change, an assistant (Claude Code, or a model you choose) rewrites {app}'s source and a new version is built right on this Mac. It is used only if it builds, passes its tests and you agree, and you can always go back.\n\nThis installer puts what that needs on this Mac and builds the first version. It takes a few minutes.\n\nClick “Continue” to see what will be installed. Nothing is installed yet.",
            ja: "{intro}{app} は自分自身を変えられるアプリです。変更を頼むと、アシスタント（Claude Code か、あなたが選んだモデル）が {app} のソースを書き換え、この Mac の上で新しいバージョンをビルドします。ビルドとテストが通り、あなたが同意したときだけ使われ、いつでも前のバージョンに戻せます。\n\nこのインストーラは、そのために必要なものをこの Mac に入れて、最初のバージョンをビルドします。数分かかります。\n\n「続ける」をクリックすると、インストールするものを確認できます。まだ何もインストールしません。",
            de: "{intro}{app} kann sich selbst ändern. Wenn du um eine Änderung bittest, schreibt ein Assistent (Claude Code oder ein Modell deiner Wahl) den Quellcode von {app} um, und direkt auf diesem Mac wird eine neue Version gebaut. Sie wird nur verwendet, wenn sie sich bauen lässt, ihre Tests besteht und du zustimmst, und du kannst jederzeit zurück.\n\nDieses Installationsprogramm installiert, was dafür nötig ist, und baut die erste Version. Das dauert ein paar Minuten.\n\nKlicke auf „Fortfahren“, um zu sehen, was installiert wird. Noch wird nichts installiert.",
            it: "{intro}{app} può cambiare se stessa. Quando chiedi una modifica, un assistente (Claude Code o un modello a tua scelta) riscrive il codice sorgente di {app} e una nuova versione viene compilata direttamente su questo Mac. Viene usata solo se si compila, supera i test e tu sei d'accordo, e puoi sempre tornare indietro.\n\nQuesto programma di installazione mette su questo Mac ciò che serve e compila la prima versione. Ci vorranno alcuni minuti.\n\nFai clic su “Continua” per vedere cosa verrà installato. Per ora non viene installato nulla.",
        );
        let place = Paths::tilde(&self.p.faust);
        let source = &self.shipped.release.version;
        let rustup = toolchain::RUSTUP_VERSION;
        let rust_line = if self.p.system_rust {
            tr!(
                en: "• Rust {rust} from your own rustup (development mode)\n",
                ja: "• あなたの rustup にある Rust {rust}（開発モード）\n",
                de: "• Rust {rust} aus deinem eigenen rustup (Entwicklungsmodus)\n",
                it: "• Rust {rust} dal tuo rustup (modalità sviluppo)\n",
            )
        } else {
            tr!(
                en: "• rustup {rustup} and Rust {rust} (rustc, cargo, rust-std), about 540 MB\n",
                ja: "• rustup {rustup} と Rust {rust}（rustc、cargo、rust-std）、約 540 MB\n",
                de: "• rustup {rustup} und Rust {rust} (rustc, cargo, rust-std), etwa 540 MB\n",
                it: "• rustup {rustup} e Rust {rust} (rustc, cargo, rust-std), circa 540 MB\n",
            )
        };
        let items = tr!(
            en: "{rust_line}• The crates the build needs, from crates.io\n• {app}'s source {source} (the copy inside this app), as a git repository\n\nLocation: {place}\nAfter that, the first version is built and tested.",
            ja: "{rust_line}• ビルドに必要なクレート（crates.io から）\n• {app} のソース {source}（このアプリに入っているもの）を git リポジトリとして\n\n場所: {place}\nそのあと、最初のバージョンをビルドしてテストします。",
            de: "{rust_line}• Die Crates für den Build, von crates.io\n• Der Quellcode von {app} {source} (aus dieser App) als git-Repository\n\nOrt: {place}\nDanach wird die erste Version gebaut und getestet.",
            it: "{rust_line}• I crate necessari alla compilazione, da crates.io\n• Il codice sorgente di {app} {source} (quello incluso in questa app) come repository git\n\nPosizione: {place}\nDopo verrà compilata e testata la prima versione.",
        );
        let notes = tr!(
            en: "Xcode and the Command Line Tools aren't used, and no administrator password is needed. Your shell settings, ~/.cargo and ~/.rustup are left alone. To undo everything, delete the folder above.",
            ja: "Xcode と Command Line Tools は使わず、管理者のパスワードも要りません。シェルの設定、~/.cargo、~/.rustup には触れません。すべて元に戻すには、上のフォルダを削除してください。",
            de: "Xcode und die Command Line Tools werden nicht verwendet, und ein Administratorpasswort ist nicht nötig. Deine Shell-Einstellungen, ~/.cargo und ~/.rustup bleiben unberührt. Um alles rückgängig zu machen, lösche den Ordner oben.",
            it: "Xcode e i Command Line Tools non vengono usati e non serve la password di amministratore. Le impostazioni della shell, ~/.cargo e ~/.rustup restano intatti. Per annullare tutto, elimina la cartella qui sopra.",
        );
        let assistant = match assistant::find() {
            Some(a) => {
                let v = a.version;
                tr!(
                    en: "Claude Code, which makes the changes: found ({v})",
                    ja: "変更を行う Claude Code: 見つかりました（{v}）",
                    de: "Claude Code, das die Änderungen vornimmt: gefunden ({v})",
                    it: "Claude Code, che apporta le modifiche: trovato ({v})",
                )
            }
            None => tr!(
                en: "Claude Code, which makes the changes: not found (install it later, or choose another model when you ask for a change)",
                ja: "変更を行う Claude Code: 見つかりません（あとからインストールするか、変更を頼むときにほかのモデルを選べます）",
                de: "Claude Code, das die Änderungen vornimmt: nicht gefunden (installiere es später oder wähle ein anderes Modell, wenn du um eine Änderung bittest)",
                it: "Claude Code, che apporta le modifiche: non trovato (installalo più tardi o scegli un altro modello quando chiedi una modifica)",
            ),
        };
        let done = tr!(
            en: "Click “Start” to open {app}.",
            ja: "「開始」をクリックすると {app} が開きます。",
            de: "Klicke auf „Starten“, um {app} zu öffnen.",
            it: "Fai clic su “Avvia” per aprire {app}.",
        );
        setup::Text { welcome, items, notes, assistant, done }
    }

    fn setup_steps(&mut self) -> Result<()> {
        let limits = Limits::none();
        let rust = self.shipped_rust();
        // (start, weight) of each step in the whole bar, in percent.
        let at = |start: f64, weight: f64, f: f64| (start + weight * f.clamp(0.0, 1.0)) / 100.0;
        std::fs::create_dir_all(&self.p.faust)?;

        setup::progress(
            at(0.0, 1.0, 0.0),
            &tr!(en: "Checking this Mac", ja: "この Mac を確認しています", de: "Dieser Mac wird geprüft", it: "Verifica di questo Mac"),
            "",
        );
        toolchain::preflight(&self.p)?;
        if !self.p.system_rust && !toolchain::installed(&self.p, &rust) {
            let downloading = tr!(en: "Downloading rustup-init", ja: "rustup-init をダウンロードしています", de: "rustup-init wird geladen", it: "Download di rustup-init");
            let init = toolchain::download_rustup(&self.p, limits, &mut |f, l| {
                setup::progress(at(1.0, 4.0, f), &downloading, l)
            })?;
            let installing = tr!(en: "Installing Rust {rust}", ja: "Rust {rust} をインストールしています", de: "Rust {rust} wird installiert", it: "Installazione di Rust {rust}");
            toolchain::install(&self.p, &init, &rust, limits, &mut |f, l| {
                setup::progress(at(5.0, 45.0, f), &installing, l)
            })?;
        }
        self.state.rust.clone_from(&rust);
        self.save();

        setup::progress(
            at(50.0, 1.0, 0.0),
            &tr!(en: "Setting up the source", ja: "ソースを用意しています", de: "Quellcode wird eingerichtet", it: "Preparazione del codice sorgente"),
            "",
        );
        self.import_shipped()?;

        let fetching = tr!(en: "Fetching the crates for the build", ja: "ビルドに使うクレートを取得しています", de: "Crates für den Build werden geladen", it: "Download dei crate per la compilazione");
        toolchain::fetch(&self.p, &rust, &self.p.repo, true, &self.p.log("setup.log"), limits, &mut |f, l| {
            setup::progress(at(51.0, 9.0, f), &fetching, l)
        })?;

        if self.state.current().is_some_and(|v| self.executable(v.id).is_some()) {
            return Ok(());
        }
        let repo = self.repo()?;
        let main = repo.ref_id(MAIN).ok_or_else(|| anyhow::anyhow!("refs/heads/main is missing"))?;
        let id = self.state.take_id();
        let dir = self.p.version_dir(id);
        let _ = std::fs::remove_dir_all(&dir);
        repo.checkout(repo.tree_of(main)?, &dir)?;
        let build_log = self.p.log(&format!("{id:04}-build.log"));
        let _ = std::fs::remove_file(&build_log);
        let gate = checks::Gate { paths: &self.p, rust: &rust, check: &self.shipped.config.check };
        checks::run(gate, &dir, id, &build_log, limits, &mut |f, step, line| {
            let step = step.name();
            let text = tr!(en: "Building the first version ({step})", ja: "最初のバージョンをビルドしています（{step}）", de: "Die erste Version wird gebaut ({step})", it: "Compilazione della prima versione ({step})");
            setup::progress(at(60.0, 40.0, f), &text, line);
        })
        .inspect_err(|_| {
            let _ = std::fs::copy(&build_log, self.p.log("setup.log"));
        })?;
        checks::keep_target(&self.p, &dir);
        // Usually the official release; after a lost build folder, whatever main held.
        let (kind, title) = if repo.ref_id(UPSTREAM) == Some(main) {
            (Kind::Official, official_title(&self.shipped.release.version))
        } else {
            (Kind::Change, repo.message_of(main).lines().next().unwrap_or("").to_owned())
        };
        self.state.versions.push(Version {
            id,
            commit: main.to_string(),
            status: Status::Trial,
            kind,
            title,
            made: now(),
            used: now(),
        });
        self.state.current = Some(id);
        self.state.fallback = None;
        self.save();
        setup::progress(1.0, &tr!(en: "Done", ja: "完了", de: "Fertig", it: "Fatto"), "");
        Ok(())
    }

    /// Puts the shipped source on refs/heads/upstream (and on main the first time).
    /// Returns the new commit when this release had not been imported yet.
    pub(super) fn import_shipped(&mut self) -> Result<Option<ObjectId>> {
        let repo = self.repo()?;
        let release = self.shipped.release.clone();
        let parent = repo.ref_id(UPSTREAM);
        if parent.is_some() && self.state.upstream_release >= release.release {
            return Ok(None);
        }
        let tree = repo.tree_from_dir(&self.shipped.source)?;
        let commit = match parent {
            Some(c) if repo.tree_of(c)? == tree => c,
            _ => {
                let message = format!(
                    "{}\n\n{}\n\nFaust-Release: {}\nFaust-Source-Commit: {}\n",
                    official_title(&release.version),
                    release.notes,
                    release.release,
                    release.commit
                );
                let time = i64::try_from(release.release).unwrap_or(0);
                let c = repo.commit(tree, parent.as_slice(), &message, official_signature(self.app_name(), time))?;
                repo.set_ref(UPSTREAM, c, "official release")?;
                c
            }
        };
        if repo.ref_id(MAIN).is_none() {
            repo.set_ref(MAIN, commit, "first version")?;
            repo.checkout(tree, &self.p.repo)?;
            repo.write_index(tree)?;
        }
        self.state.upstream_release = release.release;
        self.save();
        log(&format!("put official version {} on upstream ({commit})", release.version));
        Ok(Some(commit))
    }
}
