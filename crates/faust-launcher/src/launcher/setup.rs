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
            en: "{intro}{app} changes itself when you ask it to.\n\nAn assistant rewrites its source on this Mac.\nA new version is used only if it builds, passes its tests and you agree.\nYou can always go back.\n\nThis installer sets up what that needs and builds the first version, which takes a few minutes.\nNothing is installed until you click “Install”.",
            ja: "{intro}{app} は、頼むと自分自身を変えられるアプリです。\n\nアシスタントがこの Mac の上でソースを書き換えます。\nビルドとテストが通り、あなたが同意したときだけ、\n新しいバージョンに切り替わります。いつでも前に戻せます。\n\nこのインストーラは、必要なものを入れて\n最初のバージョンをビルドします。数分かかります。\n「インストール」を押すまで、何もインストールしません。",
            de: "{intro}{app} ändert sich selbst, wenn du darum bittest.\n\nEin Assistent schreibt dazu den Quellcode auf diesem Mac um.\nEine neue Version wird nur verwendet, wenn sie sich bauen lässt, ihre Tests besteht und du zustimmst.\nDu kannst jederzeit zurück.\n\nDieses Installationsprogramm richtet ein, was dafür nötig ist, und baut die erste Version (ein paar Minuten).\nInstalliert wird erst, wenn du auf „Installieren“ klickst.",
            it: "{intro}{app} cambia se stessa quando glielo chiedi.\n\nUn assistente ne riscrive il codice sorgente su questo Mac.\nUna nuova versione viene usata solo se si compila, supera i test e tu sei d'accordo.\nPuoi sempre tornare indietro.\n\nQuesto programma di installazione prepara ciò che serve e compila la prima versione (alcuni minuti).\nNon viene installato nulla finché non fai clic su “Installa”.",
        );
        let place = Paths::tilde(&self.p.faust);
        let rust_line = if self.p.system_rust {
            tr!(
                en: "• Rust {rust} from your own rustup (development mode)\n",
                ja: "• あなたの rustup にある Rust {rust}（開発モード）\n",
                de: "• Rust {rust} aus deinem eigenen rustup (Entwicklungsmodus)\n",
                it: "• Rust {rust} dal tuo rustup (modalità sviluppo)\n",
            )
        } else {
            tr!(
                en: "• Rust {rust}, about 540 MB\n",
                ja: "• Rust {rust}（約 540 MB）\n",
                de: "• Rust {rust}, etwa 540 MB\n",
                it: "• Rust {rust}, circa 540 MB\n",
            )
        };
        let items = tr!(
            en: "{rust_line}• The crates the build needs, from crates.io\n• {app}'s source, as a git repository\n\nLocation:\n{place}",
            ja: "{rust_line}• ビルドに使うクレート（crates.io から）\n• {app} のソース（git リポジトリとして）\n\n場所:\n{place}",
            de: "{rust_line}• Die Crates für den Build, von crates.io\n• Der Quellcode von {app}, als git-Repository\n\nOrt:\n{place}",
            it: "{rust_line}• I crate necessari alla compilazione, da crates.io\n• Il codice sorgente di {app}, come repository git\n\nPosizione:\n{place}",
        );
        let notes = tr!(
            en: "No Xcode or administrator password is needed.\n~/.cargo, ~/.rustup and your shell settings stay as they are.\nTo remove everything, delete the folder above.",
            ja: "Xcode と管理者のパスワードは要りません。\n~/.cargo、~/.rustup、シェルの設定はそのままです。\nすべて消すには、上のフォルダを削除してください。",
            de: "Xcode und ein Administratorpasswort sind nicht nötig.\n~/.cargo, ~/.rustup und deine Shell-Einstellungen bleiben, wie sie sind.\nUm alles zu entfernen, lösche den Ordner oben.",
            it: "Non servono Xcode né la password di amministratore.\n~/.cargo, ~/.rustup e le impostazioni della shell restano come sono.\nPer rimuovere tutto, elimina la cartella qui sopra.",
        );
        let assistant = match assistant::find() {
            Some(a) => {
                let v = a.short_version();
                tr!(
                    en: "Assistant: Claude Code {v}",
                    ja: "アシスタント: Claude Code {v}",
                    de: "Assistent: Claude Code {v}",
                    it: "Assistente: Claude Code {v}",
                )
            }
            None => tr!(
                en: "Assistant: Claude Code wasn't found.\nYou can choose another model when you ask for a change.",
                ja: "アシスタント: Claude Code は見つかりませんでした。\n変更を頼むときに、ほかのモデルを選べます。",
                de: "Assistent: Claude Code wurde nicht gefunden.\nDu kannst ein anderes Modell wählen, wenn du um eine Änderung bittest.",
                it: "Assistente: Claude Code non è stato trovato.\nPuoi scegliere un altro modello quando chiedi una modifica.",
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
