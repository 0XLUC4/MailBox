//! Minimal i18n: English strings are the keys, French translations live in a static table.
//!
//! ```ignore
//! div().child(tr(cx, "New mail")) // "Nouveau message" when the language is French
//! ```

use gpui::App;
use mailbox_core::settings::Language;

use super::prefs;

/// English -> French. Missing entries fall back to English.
const FR: &[(&str, &str)] = &[
    // modules & tabs
    ("Mail", "Courrier"),
    ("Calendar", "Calendrier"),
    ("People", "Contacts"),
    ("Tasks", "Tâches"),
    ("Settings", "Paramètres"),
    ("Home", "Accueil"),
    ("Send / Receive", "Envoyer / Recevoir"),
    ("View", "Affichage"),
    ("Help", "Aide"),
    // ribbon groups
    ("New", "Nouveau"),
    ("Delete", "Supprimer"),
    ("Respond", "Répondre"),
    ("Move", "Déplacer"),
    ("Tags", "Indicateurs"),
    ("Undo", "Annuler"),
    ("Redo", "Rétablir"),
    ("Go to", "Atteindre"),
    ("Arrange", "Réorganiser"),
    ("Layout", "Disposition"),
    ("Theme", "Thème"),
    ("Ribbon", "Ruban"),
    ("Accounts", "Comptes"),
    ("Sync", "Synchronisation"),
    ("Support", "Assistance"),
    ("Manage", "Gérer"),
    // actions
    ("New item", "Nouvel élément"),
    ("New mail", "Nouveau message"),
    ("New event", "Nouvel événement"),
    ("New contact", "Nouveau contact"),
    ("New task", "Nouvelle tâche"),
    ("Reply", "Répondre"),
    ("Reply all", "Répondre à tous"),
    ("Forward", "Transférer"),
    ("Archive", "Archiver"),
    ("Move to folder", "Déplacer vers un dossier"),
    ("Categorize", "Catégoriser"),
    ("Flag / unflag", "Marquer d'un indicateur"),
    ("Mark as read", "Marquer comme lu"),
    ("Mark as unread", "Marquer comme non lu"),
    ("Mark all as read", "Tout marquer comme lu"),
    ("Rules", "Règles"),
    ("Run rules", "Exécuter les règles"),
    ("Send / receive all", "Tout envoyer / recevoir"),
    ("Today", "Aujourd'hui"),
    ("Day", "Jour"),
    ("Week", "Semaine"),
    ("Month", "Mois"),
    ("Mark complete", "Marquer comme terminée"),
    ("Command palette", "Palette de commandes"),
    ("Search", "Rechercher"),
    ("Classic ribbon", "Ruban classique"),
    ("Reading pane", "Volet de lecture"),
    ("Conversations", "Conversations"),
    ("Compact", "Compact"),
    ("Dark mode", "Mode sombre"),
    ("Add account", "Ajouter un compte"),
    ("Account settings", "Paramètres du compte"),
    ("Keyboard shortcuts", "Raccourcis clavier"),
    ("About Mailbox", "À propos de Mailbox"),
    ("Signatures", "Signatures"),
    ("Go to Mail", "Aller au courrier"),
    ("Go to Calendar", "Aller au calendrier"),
    ("Go to People", "Aller aux contacts"),
    ("Go to Tasks", "Aller aux tâches"),
    ("Open settings", "Ouvrir les paramètres"),
    // search
    (
        "Search mail, people, events",
        "Rechercher courrier, contacts, événements",
    ),
    ("This folder", "Ce dossier"),
    ("All mailboxes", "Toutes les boîtes"),
    ("Recent searches", "Recherches récentes"),
    ("Search operators", "Opérateurs de recherche"),
    // settings sections
    ("Appearance", "Apparence"),
    ("Notifications", "Notifications"),
    ("Language", "Langue"),
    ("About", "À propos"),
    ("System", "Système"),
    ("Light", "Clair"),
    ("Dark", "Sombre"),
    ("Accent colour", "Couleur d'accent"),
    ("Density", "Densité"),
    ("Comfortable", "Confortable"),
    ("Font size", "Taille du texte"),
    ("Ribbon layout", "Disposition du ruban"),
    ("Simplified", "Simplifié"),
    ("Classic", "Classique"),
    ("Right", "À droite"),
    ("Bottom", "En bas"),
    ("Off", "Masqué"),
    (
        "Group messages by conversation",
        "Regrouper les messages par conversation",
    ),
    ("Mark as read when opened", "Marquer comme lu à l'ouverture"),
    ("Immediately", "Immédiatement"),
    ("After 5 s", "Après 5 s"),
    ("Never", "Jamais"),
    ("Week starts on", "La semaine commence le"),
    ("Monday", "Lundi"),
    ("Sunday", "Dimanche"),
    ("Saturday", "Samedi"),
    ("Work hours", "Heures de travail"),
    ("New mail alerts", "Alertes de nouveaux messages"),
    ("Play a sound", "Jouer un son"),
    ("Event reminders", "Rappels d'événements"),
    ("Manage rules", "Gérer les règles"),
    ("Edit", "Modifier"),
    ("Cancel", "Annuler"),
    ("Save", "Enregistrer"),
    ("Close", "Fermer"),
];

/// Translates an English UI string for `language`.
pub fn translate(language: Language, english: &'static str) -> &'static str {
    match language {
        Language::En => english,
        Language::Fr => FR
            .iter()
            .find(|(en, _)| *en == english)
            .map_or(english, |(_, fr)| fr),
    }
}

/// Translates an English UI string for the current language.
pub fn tr(cx: &App, english: &'static str) -> &'static str {
    translate(prefs(cx).language, english)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn french_falls_back_to_english() {
        assert_eq!(translate(Language::Fr, "New mail"), "Nouveau message");
        assert_eq!(translate(Language::En, "New mail"), "New mail");
        assert_eq!(translate(Language::Fr, "Not translated"), "Not translated");
    }

    #[test]
    fn no_duplicate_keys() {
        for (index, (en, _)) in FR.iter().enumerate() {
            assert!(FR[index + 1..].iter().all(|(other, _)| other != en), "{en}");
        }
    }
}
