# Mailbox

Client de bureau mail + calendrier + contacts + tâches, écrit en Rust avec [GPUI](https://gpui-kit.com)
(le framework UI GPU de Zed). Stockage local SQLite, toutes les modifications passent par des commandes
annulables (undo/redo).

## Lancer

```sh
cargo run -p mailbox -- --sample          # données d'exemple en mémoire
cargo run -p mailbox -- --data ./my.db    # base SQLite persistante
cargo test                                # tests (store + commandes)
```

Raccourcis : `Ctrl+1..4` modules, `Ctrl+,` réglages, `Ctrl+Z` / `Ctrl+Shift+Z` annuler / rétablir.

## Structure

- `crates/core` : modèle, store SQLite, commandes, données d'exemple
- `crates/sync` : backends de protocoles (IMAP/SMTP… à venir)
- `crates/ui` : application GPUI
- `apps/mailbox` : binaire

Voir [ARCHITECTURE.md](ARCHITECTURE.md).
