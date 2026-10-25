//! Uninstalling from the app's menu: the app goes to the Trash, and the support folder
//! (Rust, the versions, the history of changes, and the app's own data there) is deleted.

use super::controller::{Controller, cancel_title};
use super::log;
use super::paths::Paths;
use super::shipped;
use crate::proc;
use crate::ui;

impl Controller {
    pub(super) fn ask_uninstall(&mut self) {
        if self.busy() {
            return;
        }
        let app = self.app_name().to_owned();
        let Some(support) = self.p.deletable_support().map(std::path::Path::to_path_buf) else {
            let dir = self.p.faust.display();
            ui::alert_critical(
                &tr!(en: "{app} can't be uninstalled", ja: "{app} をアンインストールできません", de: "{app} kann nicht deinstalliert werden", it: "Impossibile disinstallare {app}"),
                &tr!(
                    en: "Its folder is in an unexpected place, so nothing was deleted ({dir}).",
                    ja: "フォルダの場所が想定と違うので、何も削除しませんでした（{dir}）。",
                    de: "Der Ordner liegt an einem unerwarteten Ort, daher wurde nichts gelöscht ({dir}).",
                    it: "La cartella si trova in una posizione imprevista, quindi non è stato eliminato nulla ({dir}).",
                ),
                &["OK"],
            );
            return;
        };
        let bundle = shipped::bundle_path().map_or_else(
            || tr!(en: "(not found)", ja: "（見つかりません）", de: "(nicht gefunden)", it: "(non trovata)"),
            |a| Paths::tilde(&a),
        );
        let mb = proc::dir_size(&support) >> 20;
        let folder = Paths::tilde(&support);
        let text = tr!(
            en: "These will be deleted. This can't be undone.\n\n• The app: {bundle} (moved to the Trash)\n• Its folder: {folder} (about {mb} MB), with Rust, the versions that were built, the history of the changes you asked for, and the app's own data\n\nClaude Code and any other Rust on this Mac are left alone.",
            ja: "次のものを削除します。元に戻せません。\n\n• アプリ: {bundle}（ゴミ箱に移動）\n• フォルダ: {folder}（約 {mb} MB）。Rust、ビルドしたバージョン、頼んだ変更の履歴、アプリ自身のデータが入っています\n\nClaude Code と、この Mac にあるほかの Rust には触れません。",
            de: "Folgendes wird gelöscht. Das kann nicht rückgängig gemacht werden.\n\n• Die App: {bundle} (kommt in den Papierkorb)\n• Ihr Ordner: {folder} (etwa {mb} MB) mit Rust, den gebauten Versionen, dem Verlauf deiner Änderungswünsche und den eigenen Daten der App\n\nClaude Code und andere Rust-Installationen auf diesem Mac bleiben unberührt.",
            it: "Verranno eliminati questi elementi. L'operazione non si può annullare.\n\n• L'app: {bundle} (spostata nel Cestino)\n• La sua cartella: {folder} (circa {mb} MB), con Rust, le versioni compilate, la cronologia delle modifiche richieste e i dati dell'app\n\nClaude Code e altre installazioni di Rust su questo Mac restano intatti.",
        );
        let title = tr!(en: "Uninstall {app}?", ja: "{app} をアンインストールしますか？", de: "{app} deinstallieren?", it: "Disinstallare {app}?");
        let delete = tr!(en: "Uninstall", ja: "アンインストール", de: "Deinstallieren", it: "Disinstalla");
        if ui::alert_critical(&title, &text, &[&cancel_title(), &delete]) != 1 {
            return;
        }
        log("uninstalling");
        self.uninstalling = true;
        // The app gets to quit cleanly first; exited() then calls uninstall().
        if !self.stop_app() {
            self.uninstall();
        }
    }

    pub(super) fn uninstall(&mut self) {
        let app = self.app_name().to_owned();
        let mut failed = Vec::new();
        match self.p.deletable_support() {
            Some(support) => {
                if let Err(e) = std::fs::remove_dir_all(support) {
                    failed.push(format!("{}: {e}", support.display()));
                }
            }
            None => failed.push(self.p.faust.display().to_string()),
        }
        if let Some(bundle) = shipped::bundle_path()
            && let Err(e) = ui::trash(&bundle)
        {
            failed.push(format!("{}: {e}", bundle.display()));
        }
        if failed.is_empty() {
            ui::alert(
                &tr!(en: "{app} was uninstalled", ja: "{app} をアンインストールしました", de: "{app} wurde deinstalliert", it: "{app} è stato disinstallato"),
                &tr!(en: "The app and its folder were deleted.", ja: "アプリとフォルダを削除しました。", de: "Die App und ihr Ordner wurden gelöscht.", it: "L'app e la sua cartella sono state eliminate."),
                &["OK"],
            );
        } else {
            let title = tr!(en: "Some items couldn't be deleted", ja: "削除できなかったものがあります", de: "Einige Objekte konnten nicht gelöscht werden", it: "Alcuni elementi non sono stati eliminati");
            ui::alert_critical(&title, &failed.join("\n"), &["OK"]);
        }
        ui::quit();
    }
}
