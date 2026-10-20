use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UiLanguage {
    #[default]
    English,
    Indonesian,
    Spanish,
    French,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// A few welcome-screen keys are parked for the deferred language row; the
// translations stay complete so enabling it later is not a data migration.
#[allow(dead_code)]
pub enum TextKey {
    WelcomeMenu,
    WelcomeLastPage,
    WelcomeSetup,
    WelcomeSetupHint,
    WelcomeResumeLabel,
    WelcomeNewDraftLabel,
    WelcomeOpenFileLabel,
    WelcomeHelpLabel,
    WelcomeExitLabel,
    WelcomeExitDetail,
    WelcomeNewDraftDetail,
    WelcomeOpenFileDetail,
    WelcomeHelpDetail,
    WelcomeHelpActiveDetail,
    WelcomeOpenToBegin,
    WelcomeLanguage,
    FileBrowserTitle,
    FileBrowserSearch,
    FileBrowserNoMatches,
}

impl UiLanguage {
    pub const ALL: [Self; 4] = [Self::English, Self::Indonesian, Self::Spanish, Self::French];

    pub fn display_name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Indonesian => "Indonesian",
            Self::Spanish => "Spanish",
            Self::French => "French",
        }
    }

    pub fn text(self, key: TextKey) -> &'static str {
        match self {
            Self::English => match key {
                TextKey::WelcomeMenu => "opening menu",
                TextKey::WelcomeLastPage => "last page",
                TextKey::WelcomeSetup => "first start",
                TextKey::WelcomeSetupHint => "Press L to change language at any time.",
                TextKey::WelcomeResumeLabel => "resume last",
                TextKey::WelcomeNewDraftLabel => "new draft",
                TextKey::WelcomeOpenFileLabel => "open file",
                TextKey::WelcomeHelpLabel => "help",
                TextKey::WelcomeExitLabel => "exit",
                TextKey::WelcomeExitDetail => "leave Ordet.",
                TextKey::WelcomeNewDraftDetail => "blank page, no template.",
                TextKey::WelcomeOpenFileDetail => "browse, rename, copy, or type a path.",
                TextKey::WelcomeHelpDetail => "press enter or right to browse shortcuts.",
                TextKey::WelcomeHelpActiveDetail => "up or down scrolls. left returns to the menu.",
                TextKey::WelcomeOpenToBegin => "open a file to begin.",
                TextKey::WelcomeLanguage => "language",
                TextKey::FileBrowserTitle => "Open",
                TextKey::FileBrowserSearch => "Search",
                TextKey::FileBrowserNoMatches => "No files match this search.",
            },
            Self::Indonesian => match key {
                TextKey::WelcomeMenu => "menu pembuka",
                TextKey::WelcomeLastPage => "halaman terakhir",
                TextKey::WelcomeSetup => "awal pertama",
                TextKey::WelcomeSetupHint => "Tekan L untuk ganti bahasa kapan saja.",
                TextKey::WelcomeResumeLabel => "lanjut terakhir",
                TextKey::WelcomeNewDraftLabel => "draf baru",
                TextKey::WelcomeOpenFileLabel => "buka file",
                TextKey::WelcomeHelpLabel => "bantuan",
                TextKey::WelcomeExitLabel => "keluar",
                TextKey::WelcomeExitDetail => "keluar dari Ordet.",
                TextKey::WelcomeNewDraftDetail => "halaman kosong, tanpa template.",
                TextKey::WelcomeOpenFileDetail => "jelajah, ganti nama, salin, atau ketik path.",
                TextKey::WelcomeHelpDetail => "tekan enter atau kanan untuk lihat pintasan.",
                TextKey::WelcomeHelpActiveDetail => {
                    "atas atau bawah menggulir. kiri kembali ke menu."
                }
                TextKey::WelcomeOpenToBegin => "buka file untuk mulai.",
                TextKey::WelcomeLanguage => "bahasa",
                TextKey::FileBrowserTitle => "Buka",
                TextKey::FileBrowserSearch => "Cari",
                TextKey::FileBrowserNoMatches => "Tidak ada file yang cocok.",
            },
            Self::Spanish => match key {
                TextKey::WelcomeMenu => "menu inicial",
                TextKey::WelcomeLastPage => "ultima pagina",
                TextKey::WelcomeSetup => "primer inicio",
                TextKey::WelcomeSetupHint => "Pulsa L para cambiar el idioma en cualquier momento.",
                TextKey::WelcomeResumeLabel => "reanudar ultimo",
                TextKey::WelcomeNewDraftLabel => "nuevo borrador",
                TextKey::WelcomeOpenFileLabel => "abrir archivo",
                TextKey::WelcomeHelpLabel => "ayuda",
                TextKey::WelcomeExitLabel => "salir",
                TextKey::WelcomeExitDetail => "salir de Ordet.",
                TextKey::WelcomeNewDraftDetail => "pagina vacia, sin plantilla.",
                TextKey::WelcomeOpenFileDetail => {
                    "explora, cambia nombre, copia o escribe una ruta."
                }
                TextKey::WelcomeHelpDetail => "pulsa enter o derecha para ver atajos.",
                TextKey::WelcomeHelpActiveDetail => {
                    "arriba o abajo desplaza. izquierda vuelve al menu."
                }
                TextKey::WelcomeOpenToBegin => "abre un archivo para empezar.",
                TextKey::WelcomeLanguage => "idioma",
                TextKey::FileBrowserTitle => "Abrir",
                TextKey::FileBrowserSearch => "Buscar",
                TextKey::FileBrowserNoMatches => "Ningun archivo coincide.",
            },
            Self::French => match key {
                TextKey::WelcomeMenu => "menu d'ouverture",
                TextKey::WelcomeLastPage => "derniere page",
                TextKey::WelcomeSetup => "premier demarrage",
                TextKey::WelcomeSetupHint => "Appuyez sur L pour changer la langue a tout moment.",
                TextKey::WelcomeResumeLabel => "reprendre",
                TextKey::WelcomeNewDraftLabel => "nouveau brouillon",
                TextKey::WelcomeOpenFileLabel => "ouvrir un fichier",
                TextKey::WelcomeHelpLabel => "aide",
                TextKey::WelcomeExitLabel => "quitter",
                TextKey::WelcomeExitDetail => "quitter Ordet.",
                TextKey::WelcomeNewDraftDetail => "page vide, sans modele.",
                TextKey::WelcomeOpenFileDetail => {
                    "parcourir, renommer, copier, ou saisir un chemin."
                }
                TextKey::WelcomeHelpDetail => {
                    "appuyez sur entree ou droite pour voir les raccourcis."
                }
                TextKey::WelcomeHelpActiveDetail => {
                    "haut ou bas fait defiler. gauche revient au menu."
                }
                TextKey::WelcomeOpenToBegin => "ouvrez un fichier pour commencer.",
                TextKey::WelcomeLanguage => "langue",
                TextKey::FileBrowserTitle => "Ouvrir",
                TextKey::FileBrowserSearch => "Recherche",
                TextKey::FileBrowserNoMatches => "Aucun fichier ne correspond.",
            },
        }
    }

    pub fn welcome_resume_detail(self, path_label: Option<&str>) -> String {
        match (self, path_label) {
            (Self::English, Some(path)) => format!("pick up {path}."),
            (Self::English, None) => "return to the latest page.".to_string(),
            (Self::Indonesian, Some(path)) => format!("lanjutkan {path}."),
            (Self::Indonesian, None) => "kembali ke halaman terakhir.".to_string(),
            (Self::Spanish, Some(path)) => format!("sigue en {path}."),
            (Self::Spanish, None) => "volver a la ultima pagina.".to_string(),
            (Self::French, Some(path)) => format!("reprendre {path}."),
            (Self::French, None) => "revenir a la derniere page.".to_string(),
        }
    }

    pub fn language_changed_status(self, new_language: UiLanguage) -> String {
        match self {
            Self::English => format!("Startup language set to {}.", new_language.display_name()),
            Self::Indonesian => format!("Bahasa awal diubah ke {}.", new_language.display_name()),
            Self::Spanish => format!("Idioma inicial cambiado a {}.", new_language.display_name()),
            Self::French => format!(
                "Langue de demarrage changee vers {}.",
                new_language.display_name()
            ),
        }
    }
}
