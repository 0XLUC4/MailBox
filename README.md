# Mailbox

Client de bureau mail + calendrier + contacts + tâches, écrit en Rust avec [GPUI](https://gpui-kit.com)
(le framework UI GPU de Zed). Stockage local SQLite, toutes les modifications passent par des commandes
annulables (undo/redo).

## Lancer

```sh
cargo run -p mailbox                      # vos données : %APPDATA%\mailbox\mailbox.db (Windows),
                                          # ~/.local/share/mailbox/mailbox.db ailleurs
cargo run -p mailbox -- --data ./my.db    # une autre base SQLite
cargo test                                # tests (store, commandes, IMAP/SMTP sur faux serveurs)
```

Au premier lancement (aucun compte), un écran d'accueil ouvre l'assistant « Ajouter un compte » :
adresse + mot de passe (ou mot de passe d'application), détection des serveurs (table intégrée,
puis base Mozilla ISPDB / autoconfig du domaine / MX — seul le nom de domaine est envoyé),
« Tester la connexion » (IMAP + SMTP), mot de passe rangé dans le trousseau du système, puis
synchronisation immédiate. Gmail / Outlook : connexion OAuth2 si un client id est configuré
(champ de l'assistant, ou `MAILBOX_GOOGLE_CLIENT_ID` / `MAILBOX_MS_CLIENT_ID`).

`MAILBOX_DATA_DIR` change le dossier de données. Les données d'exemple (`mailbox_core::sample`)
ne sont plus livrées : elles ne compilent qu'en test ou avec la feature `dev` de `mailbox-core`.

Raccourcis : `Ctrl+1..4` modules, `Ctrl+,` réglages, `Ctrl+Z` / `Ctrl+Shift+Z` annuler / rétablir,
`F9` envoyer/recevoir.

Réglages (`Ctrl+,`) : thème clair/sombre/système, couleur d'accent, densité, taille du texte, volet
de lecture, langue (français / anglais), notifications, signatures, messagerie par défaut
(`mailto:`), comptes de messagerie (ajouter, modifier, ressaisir le mot de passe, réordonner,
supprimer) et comptes calendrier / contacts (CalDAV / CardDAV).

## Version de production et paquet portable (Windows)

```powershell
cargo build --release -p mailbox                                  # target\release\mailbox.exe
powershell -ExecutionPolicy Bypass -File scripts\package.ps1      # dist\Mailbox-portable.zip
```

Le zip contient `Mailbox.exe` (icône intégrée), ce README et l'icône. Aucune installation :
décompresser et lancer. Les données restent dans `%APPDATA%\mailbox`.

## Icône

Source : `assets/icon.svg` (carré arrondi noir, « M » blanc). PNG (16 à 512 px) dans `assets/png/`
et `assets/icon.ico` multi-tailles, régénérés par `cargo run -p mailbox --example make_icon`.
`apps/mailbox/build.rs` intègre l'`.ico` dans l'exe (icône de l'Explorateur, de la fenêtre et de la
barre des tâches) ; la même image sert aux notifications et à l'enregistrement `mailto:`.

## Structure

- `crates/core` : modèle, store SQLite, commandes
- `crates/sync` : IMAP/SMTP, OAuth2, détection des serveurs, formats .eml/.mbox
- `crates/ui` : application GPUI
- `apps/mailbox` : binaire (+ `build.rs` : icône Windows, `examples/make_icon.rs`)
- `assets` : icône (SVG source, PNG, ICO)
- `scripts/package.ps1` : zip portable

Voir [ARCHITECTURE.md](ARCHITECTURE.md).
