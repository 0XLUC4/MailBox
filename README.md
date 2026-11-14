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

## Structure

- `crates/core` : modèle, store SQLite, commandes
- `crates/sync` : IMAP/SMTP, OAuth2, détection des serveurs, formats .eml/.mbox
- `crates/ui` : application GPUI
- `apps/mailbox` : binaire

Voir [ARCHITECTURE.md](ARCHITECTURE.md).
