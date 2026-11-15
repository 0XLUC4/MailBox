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
    // updates (updater.rs, mailbox-updater)
    ("Updates", "Mises à jour"),
    ("Update available", "Mise à jour disponible"),
    ("You're up to date", "Vous êtes à jour"),
    ("Checking for updates…", "Recherche de mises à jour…"),
    (
        "Could not check for updates",
        "Impossible de vérifier les mises à jour",
    ),
    ("Check for updates", "Rechercher des mises à jour"),
    ("What's new", "Nouveautés"),
    ("Update and restart", "Mettre à jour et redémarrer"),
    ("Updating…", "Mise à jour…"),
    ("Download", "Télécharger"),
    // send checks (compose/view.rs)
    ("Send anyway?", "Envoyer quand même ?"),
    ("Send", "Envoyer"),
    (
        "This message has no subject.",
        "Ce message n'a pas d'objet.",
    ),
    (
        "The message mentions an attachment, but nothing is attached.",
        "Le message parle d'une pièce jointe, mais aucune n'est jointe.",
    ),
    // contact card (mail/sender_card.rs)
    ("New email", "Écrire"),
    ("Add to contacts", "Ajouter aux contacts"),
    ("In your contacts", "Dans vos contacts"),
    ("Copy address", "Copier l'adresse"),
    ("Mailbox {} is available", "Mailbox {} est disponible"),
    ("You have {}.", "Vous avez la {}."),
    (
        "Mailbox restarts to finish the update.",
        "Mailbox redémarre pour terminer la mise à jour.",
    ),
    (
        "Opens the release page to download it.",
        "Ouvre la page de la version pour la télécharger.",
    ),
    (
        "Offline: could not reach GitHub",
        "Hors connexion : GitHub est injoignable",
    ),
    (
        "No release published yet",
        "Aucune version publiée pour l'instant",
    ),
    (
        "Unexpected answer from GitHub",
        "Réponse inattendue de GitHub",
    ),
    (
        "This release has no package for this system",
        "Cette version n'a pas de paquet pour ce système",
    ),
    (
        "The download is damaged (checksum mismatch)",
        "Le téléchargement est endommagé (somme de contrôle incorrecte)",
    ),
    (
        "Development build: updates are installed only in a release build",
        "Version de développement : les mises à jour s'installent seulement dans une version publiée",
    ),
    (
        "Updates are installed automatically only on Windows and macOS",
        "Les mises à jour automatiques ne sont disponibles que sous Windows et macOS",
    ),
    (
        "Could not install the update",
        "Impossible d'installer la mise à jour",
    ),
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
    // ribbon & palette actions
    ("Snooze", "Reporter"),
    ("Mute conversation", "Ignorer la conversation"),
    ("Print", "Imprimer"),
    ("Save as .eml", "Enregistrer en .eml"),
    (
        "Save all attachments",
        "Enregistrer toutes les pièces jointes",
    ),
    ("Export mailbox (.mbox)", "Exporter la boîte (.mbox)"),
    ("Import .mbox / .eml", "Importer .mbox / .eml"),
    ("Automatic replies", "Réponses automatiques"),
    ("Read receipts", "Accusés de lecture"),
    (
        "Set as default mail app",
        "Définir comme messagerie par défaut",
    ),
    ("Simplified ribbon", "Ruban simplifié"),
    ("More", "Plus"),
    ("More commands", "Plus de commandes"),
    ("Later", "Plus tard"),
    ("Message", "Message"),
    ("Folder", "Dossier"),
    ("Select a message first", "Sélectionnez d'abord un message"),
    ("Everything is already read", "Tout est déjà lu"),
    (
        "Select a folder to run rules on",
        "Sélectionnez un dossier où exécuter les règles",
    ),
    (
        "No message matched your rules",
        "Aucun message ne correspond à vos règles",
    ),
    ("Delete permanently?", "Supprimer définitivement ?"),
    // settings
    (
        "Row height in lists.",
        "Hauteur des lignes dans les listes.",
    ),
    (
        "One compact row, or large buttons in labelled groups.",
        "Une seule ligne compacte, ou de grands boutons en groupes nommés.",
    ),
    (
        "Added to new messages and replies.",
        "Ajoutée aux nouveaux messages et aux réponses.",
    ),
    (
        "Highlighted in day and week views.",
        "Mises en évidence dans les vues jour et semaine.",
    ),
    (
        "Desktop notification when mail arrives.",
        "Notification sur le bureau à l'arrivée d'un message.",
    ),
    ("Menus, ribbon and settings.", "Menus, ruban et paramètres."),
    ("Open rules dialog", "Ouvrir la fenêtre des règles"),
    (
        "{} rule(s). Rules run on new mail, in order, and can be run on a folder at any time.",
        "{} règle(s). Les règles s'appliquent aux nouveaux messages, dans l'ordre, et peuvent \
         être exécutées sur un dossier à tout moment.",
    ),
    ("Version {}", "Version {}"),
    (
        "Out of office: answer each sender once during a date range.",
        "Absence : répondre une fois à chaque expéditeur pendant une période.",
    ),
    ("Set up", "Configurer"),
    (
        "What to do when a message asks to know you read it.",
        "Que faire quand un message demande un accusé de lecture.",
    ),
    ("Choose", "Choisir"),
    ("Default mail app", "Messagerie par défaut"),
    (
        "Open mailto: links with Mailbox (Windows asks you to confirm).",
        "Ouvrir les liens mailto: avec Mailbox (Windows demande de confirmer).",
    ),
    // accounts
    ("MAIL ACCOUNTS", "COMPTES DE MESSAGERIE"),
    (
        "CALENDAR & CONTACTS ACCOUNTS (CALDAV / CARDDAV)",
        "COMPTES CALENDRIER ET CONTACTS (CALDAV / CARDDAV)",
    ),
    ("Move up", "Monter"),
    ("Move down", "Descendre"),
    ("Password…", "Mot de passe…"),
    (
        "No mail account yet.",
        "Aucun compte de messagerie pour l'instant.",
    ),
    (
        "Sync calendars and contacts with Google, iCloud, Fastmail, Nextcloud or any \
         CalDAV/CardDAV server.",
        "Synchronisez calendriers et contacts avec Google, iCloud, Fastmail, Nextcloud ou tout \
         serveur CalDAV/CardDAV.",
    ),
    (
        "Add calendar & contacts account",
        "Ajouter un compte calendrier et contacts",
    ),
    ("Remove {}?", "Supprimer {} ?"),
    ("Remove account", "Supprimer le compte"),
    ("Remove account…", "Supprimer le compte…"),
    (
        "Its mail, folders and calendars are deleted from this computer and its password is \
         removed from the system keychain. Nothing is deleted on the server.",
        "Ses messages, dossiers et calendriers sont effacés de cet ordinateur et son mot de \
         passe est retiré du trousseau du système. Rien n'est supprimé sur le serveur.",
    ),
    (
        "Calendars and contacts stop syncing with this server. Nothing is deleted on the server.",
        "Les calendriers et contacts ne sont plus synchronisés avec ce serveur. Rien n'est \
         supprimé sur le serveur.",
    ),
    (
        "Enter the password again",
        "Saisir à nouveau le mot de passe",
    ),
    (
        "Password updated: syncing…",
        "Mot de passe mis à jour : synchronisation…",
    ),
    (
        "This account has no server settings",
        "Ce compte n'a pas de paramètres de serveur",
    ),
    // add account wizard
    ("Add an account", "Ajouter un compte"),
    (
        "Enter your email address and password: Mailbox finds the server settings and checks \
         that everything works.",
        "Saisissez votre adresse e-mail et votre mot de passe : Mailbox trouve les paramètres \
         du serveur et vérifie que tout fonctionne.",
    ),
    ("Server settings", "Paramètres du serveur"),
    ("Step 2 of 2", "Étape 2 sur 2"),
    (
        "Name, colour, servers and password.",
        "Nom, couleur, serveurs et mot de passe.",
    ),
    ("Email address", "Adresse e-mail"),
    ("Password", "Mot de passe"),
    ("New password", "Nouveau mot de passe"),
    ("Account name", "Nom du compte"),
    ("Colour", "Couleur"),
    ("Server", "Serveur"),
    ("Port", "Port"),
    ("Security", "Sécurité"),
    ("Username", "Nom d'utilisateur"),
    ("OAuth client id", "Identifiant client OAuth"),
    ("INCOMING MAIL (IMAP)", "COURRIER ENTRANT (IMAP)"),
    ("OUTGOING MAIL (SMTP)", "COURRIER SORTANT (SMTP)"),
    ("Work, Personal...", "Travail, Personnel..."),
    (
        "Leave empty to keep the saved password",
        "Laisser vide pour garder le mot de passe enregistré",
    ),
    (
        "Password or app password",
        "Mot de passe ou mot de passe d'application",
    ),
    (
        "Usually your email address",
        "En général votre adresse e-mail",
    ),
    (
        "Optional: enables “Sign in with Google / Microsoft”",
        "Facultatif : active « Se connecter avec Google / Microsoft »",
    ),
    (
        "Enter a valid email address",
        "Saisissez une adresse e-mail valide",
    ),
    (
        "Enter your password (or an app password)",
        "Saisissez votre mot de passe (ou un mot de passe d'application)",
    ),
    ("{} settings detected.", "Paramètres {} détectés."),
    (
        "{} detected: settings will be filled in",
        "{} détecté : les paramètres seront remplis",
    ),
    (
        "Looking up the server settings…",
        "Recherche des paramètres du serveur…",
    ),
    ("Settings found online.", "Paramètres trouvés en ligne."),
    (
        "{} settings found online.",
        "Paramètres {} trouvés en ligne.",
    ),
    (
        "Provider not recognised: we guessed the usual settings. Check them with your provider.",
        "Fournisseur non reconnu : nous avons deviné les paramètres habituels. Vérifiez-les \
         auprès de votre fournisseur.",
    ),
    (
        "Finish signing in in your browser…",
        "Terminez la connexion dans votre navigateur…",
    ),
    ("Testing the connection…", "Test de la connexion…"),
    ("Test connection", "Tester la connexion"),
    ("Continue", "Continuer"),
    ("Back", "Retour"),
    (
        "Connected: sign-in works for receiving and sending ({} folders).",
        "Connecté : la connexion fonctionne pour recevoir et envoyer ({} dossiers).",
    ),
    ("Sign in with {}", "Se connecter avec {}"),
    (
        "Signing in with {} in your browser.",
        "Connexion avec {} dans votre navigateur.",
    ),
    (
        "Signing in with a password or app password.",
        "Connexion avec un mot de passe ou un mot de passe d'application.",
    ),
    ("Use a password instead", "Utiliser plutôt un mot de passe"),
    (
        "Sign in with the browser instead",
        "Se connecter plutôt avec le navigateur",
    ),
    (
        "To sign in with {} in your browser instead, set an OAuth client id in the server \
         settings or the {} environment variable.",
        "Pour vous connecter avec {} dans le navigateur, indiquez un identifiant client OAuth \
         dans les paramètres du serveur ou la variable d'environnement {}.",
    ),
    (
        "Passwords are kept in the system keychain, never in the Mailbox database.",
        "Les mots de passe sont rangés dans le trousseau du système, jamais dans la base de \
         Mailbox.",
    ),
    (
        "Account {} added: syncing…",
        "Compte {} ajouté : synchronisation…",
    ),
    (
        "Create an app password",
        "Créer un mot de passe d'application",
    ),
    (
        "With two-step verification on, Gmail needs an app password instead of your Google \
         password.",
        "Avec la validation en deux étapes, Gmail demande un mot de passe d'application à la \
         place de votre mot de passe Google.",
    ),
    (
        "With two-step verification on, Outlook needs an app password instead of your \
         Microsoft password.",
        "Avec la validation en deux étapes, Outlook demande un mot de passe d'application à la \
         place de votre mot de passe Microsoft.",
    ),
    (
        "iCloud Mail needs an app-specific password, created in your Apple Account.",
        "iCloud Mail demande un mot de passe pour app, créé dans votre compte Apple.",
    ),
    (
        "Yahoo Mail needs an app password, created in your account security settings.",
        "Yahoo Mail demande un mot de passe d'application, créé dans les paramètres de \
         sécurité de votre compte.",
    ),
    (
        "AOL Mail needs an app password, created in your account security settings.",
        "AOL Mail demande un mot de passe d'application, créé dans les paramètres de sécurité \
         de votre compte.",
    ),
    // onboarding & about
    ("Connect your email", "Connectez votre messagerie"),
    (
        "Add an account to start. Enter your address and password: server settings are \
         detected for you.",
        "Ajoutez un compte pour commencer. Saisissez votre adresse et votre mot de passe : les \
         paramètres du serveur sont détectés pour vous.",
    ),
    (
        "Mail syncs over IMAP and SMTP with any provider: Gmail, Outlook, iCloud, your own \
         domain.",
        "Le courrier se synchronise en IMAP et SMTP avec tout fournisseur : Gmail, Outlook, \
         iCloud, votre propre domaine.",
    ),
    (
        "Your password stays in the system keychain; mail is stored on this computer only.",
        "Votre mot de passe reste dans le trousseau du système ; le courrier n'est stocké que \
         sur cet ordinateur.",
    ),
    (
        "Read, sort and write offline: changes are sent when you are back online.",
        "Lisez, triez et écrivez hors connexion : les changements partent dès le retour en \
         ligne.",
    ),
    (
        "Calendar & contacts only",
        "Calendrier et contacts seulement",
    ),
    (
        "Mail, calendar, contacts and tasks in one fast native app. Built in Rust with GPUI.",
        "Courrier, calendrier, contacts et tâches dans une seule application native et rapide. \
         Écrite en Rust avec GPUI.",
    ),
    ("Icons by Lucide (ISC)", "Icônes Lucide (ISC)"),
    // rules dialog
    ("Rule name", "Nom de la règle"),
    ("Sender name or address", "Nom ou adresse de l'expéditeur"),
    ("Recipient address", "Adresse du destinataire"),
    ("Words in the subject", "Mots de l'objet"),
    ("Words in the message", "Mots du message"),
    ("Name", "Nom"),
    (
        "WHEN A MESSAGE ARRIVES THAT MATCHES ALL OF",
        "QUAND UN MESSAGE ARRIVE ET REMPLIT TOUTES CES CONDITIONS",
    ),
    ("From contains", "L'expéditeur contient"),
    ("To contains", "Le destinataire contient"),
    ("Subject contains", "L'objet contient"),
    ("Body contains", "Le message contient"),
    ("Has an attachment", "A une pièce jointe"),
    ("Importance is", "L'importance est"),
    ("Any", "Toutes"),
    ("Low", "Faible"),
    ("Normal", "Normale"),
    ("High", "Haute"),
    ("Unchanged", "Inchangée"),
    ("DO THE FOLLOWING", "ALORS"),
    ("Flag", "Marquer d'un indicateur"),
    ("Set importance", "Définir l'importance"),
    ("Delete (move to Trash)", "Supprimer (vers la corbeille)"),
    (
        "Stop processing more rules",
        "Ne pas appliquer d'autres règles",
    ),
    ("Don't move", "Ne pas déplacer"),
    ("None", "Aucune"),
    ("Enabled", "Activée"),
    ("New rule", "Nouvelle règle"),
    ("Run on current folder", "Exécuter sur le dossier actuel"),
    ("Run earlier", "Exécuter plus tôt"),
    ("Run later", "Exécuter plus tard"),
    ("Select a rule", "Sélectionnez une règle"),
    (
        "Pick a rule to edit it, or create a new one.",
        "Choisissez une règle pour la modifier, ou créez-en une.",
    ),
    (
        "No rules yet. Rules file, flag or categorize mail automatically.",
        "Aucune règle pour l'instant. Les règles classent, marquent ou catégorisent le \
         courrier automatiquement.",
    ),
    ("Rule saved", "Règle enregistrée"),
    ("Delete rule?", "Supprimer la règle ?"),
    ("“{}” will be removed.", "« {} » sera supprimée."),
    ("Add an account first", "Ajoutez d'abord un compte"),
    (
        "Add at least one condition",
        "Ajoutez au moins une condition",
    ),
    ("Add at least one action", "Ajoutez au moins une action"),
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
