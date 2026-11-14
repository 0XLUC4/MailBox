//! Deterministic, realistic sample dataset (`--sample`): 2 accounts, nested folders,
//! ~200 messages over 6 weeks with threads, recurring events, contacts, tasks, rules.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, Utc};

use crate::error::Result;
use crate::model::*;
use crate::store::Store;

/// Number of days of mail history generated.
const HISTORY_DAYS: i64 = 42;
/// Target message count.
const MESSAGE_TARGET: usize = 200;

/// Small deterministic PRNG (xorshift64*), so the dataset is stable across runs.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, max: u64) -> u64 {
        if max == 0 { 0 } else { self.next_u64() % max }
    }

    fn percent(&mut self, chance: u64) -> bool {
        self.below(100) < chance
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

struct Person {
    name: &'static str,
    email: &'static str,
    company: &'static str,
    title: &'static str,
    phone: &'static str,
}

const WORK_PEOPLE: &[Person] = &[
    Person {
        name: "Sofia Ramirez",
        email: "sofia.ramirez@northwind.example",
        company: "Northwind",
        title: "Engineering Manager",
        phone: "+33 6 12 34 56 01",
    },
    Person {
        name: "Daniel Okafor",
        email: "daniel.okafor@northwind.example",
        company: "Northwind",
        title: "Staff Engineer",
        phone: "+33 6 12 34 56 02",
    },
    Person {
        name: "Mei Tanaka",
        email: "mei.tanaka@northwind.example",
        company: "Northwind",
        title: "Product Designer",
        phone: "+33 6 12 34 56 03",
    },
    Person {
        name: "Lucas Bernard",
        email: "lucas.bernard@northwind.example",
        company: "Northwind",
        title: "Product Manager",
        phone: "+33 6 12 34 56 04",
    },
    Person {
        name: "Priya Nair",
        email: "priya.nair@northwind.example",
        company: "Northwind",
        title: "Data Scientist",
        phone: "+33 6 12 34 56 05",
    },
    Person {
        name: "Tom Becker",
        email: "tom.becker@northwind.example",
        company: "Northwind",
        title: "Finance Lead",
        phone: "+33 6 12 34 56 06",
    },
    Person {
        name: "Hannah Schmidt",
        email: "hannah@acme.example",
        company: "Acme Corp",
        title: "Head of Procurement",
        phone: "+49 30 1234 5601",
    },
    Person {
        name: "Marco Rossi",
        email: "marco.rossi@acme.example",
        company: "Acme Corp",
        title: "IT Director",
        phone: "+39 02 1234 5602",
    },
    Person {
        name: "Elena Petrova",
        email: "elena@talentbridge.example",
        company: "TalentBridge",
        title: "Recruiter",
        phone: "+44 20 1234 5603",
    },
];

const PERSONAL_PEOPLE: &[Person] = &[
    Person {
        name: "Sam Lefebvre",
        email: "sam.lefebvre@mail.example",
        company: "",
        title: "",
        phone: "+33 6 98 76 54 01",
    },
    Person {
        name: "Julia Costa",
        email: "julia.costa@mail.example",
        company: "",
        title: "",
        phone: "+33 6 98 76 54 02",
    },
    Person {
        name: "Noah Williams",
        email: "noah.w@mail.example",
        company: "",
        title: "",
        phone: "+33 6 98 76 54 03",
    },
    Person {
        name: "Claire Martin",
        email: "claire.martin@mail.example",
        company: "",
        title: "Sister",
        phone: "+33 6 98 76 54 04",
    },
    Person {
        name: "Green Lease Agency",
        email: "contact@greenlease.example",
        company: "Green Lease",
        title: "Property manager",
        phone: "+33 1 23 45 67 89",
    },
];

struct ThreadTemplate {
    subject: &'static str,
    /// Destination folder name ("Inbox" or a custom folder name).
    folder: &'static str,
    /// Indices into the account's people list.
    people: &'static [usize],
    lines: &'static [&'static str],
}

const WORK_THREADS: &[ThreadTemplate] = &[
    ThreadTemplate {
        subject: "Atlas sprint 14 planning",
        folder: "Atlas",
        people: &[0, 1, 3],
        lines: &[
            "I've drafted the sprint 14 scope in the planning doc. The big items are the sync engine refactor and the new search indexer.",
            "Looks reasonable. I'd move the indexer to next sprint, we still have open questions on the storage format.",
            "Agreed. Let's keep the refactor and add the two flaky-test fixes instead.",
            "Updated the board. Planning is Monday 10:00, see you there.",
        ],
    },
    ThreadTemplate {
        subject: "Q4 budget review",
        folder: "Inbox",
        people: &[5, 0],
        lines: &[
            "Attached is the Q4 budget forecast. We're about 4% over on cloud spend, mostly from the analytics cluster.",
            "Thanks Tom. Can we break the analytics cost down per team before Thursday?",
            "Sure, I'll send the breakdown tomorrow morning.",
        ],
    },
    ThreadTemplate {
        subject: "Helios launch checklist",
        folder: "Helios",
        people: &[3, 2, 1],
        lines: &[
            "Here's the launch checklist for Helios. Please tick your items by Friday.",
            "Design assets are final and uploaded to the shared drive.",
            "Backend is feature-frozen. Load tests passed at 3x expected traffic.",
            "Great work everyone, we're on track for the launch date.",
        ],
    },
    ThreadTemplate {
        subject: "Acme renewal - contract draft",
        folder: "Acme Corp",
        people: &[6, 7],
        lines: &[
            "Please find the renewal draft attached. We'd like to discuss the volume tiers in section 4.",
            "Thanks Hannah, we'll review with legal and come back to you early next week.",
            "Quick follow-up: could we also add SSO to the scope of the renewal?",
        ],
    },
    ThreadTemplate {
        subject: "Design review: new onboarding flow",
        folder: "Inbox",
        people: &[2, 3],
        lines: &[
            "The new onboarding prototype is ready for review. Main change: we ask for the account type before the email.",
            "Love the simplified first step. The progress indicator feels a bit heavy on mobile though.",
            "Good point, I'll try a lighter variant and share it before the review.",
        ],
    },
    ThreadTemplate {
        subject: "Incident postmortem: API latency spike",
        folder: "Inbox",
        people: &[1, 0, 4],
        lines: &[
            "Postmortem draft for Tuesday's latency spike. Root cause: a missing index after the orders migration.",
            "Thanks Daniel. Let's add a migration check to CI so this can't happen silently again.",
            "I can add query-plan regression tests to the nightly job.",
        ],
    },
    ThreadTemplate {
        subject: "Hiring: senior backend engineer",
        folder: "Inbox",
        people: &[8, 0],
        lines: &[
            "I have three strong candidates for the senior backend role. Can we schedule the technical interviews next week?",
            "Yes, please send me their profiles and I'll set up the loop.",
        ],
    },
    ThreadTemplate {
        subject: "Team offsite agenda",
        folder: "Inbox",
        people: &[0, 3, 2],
        lines: &[
            "First draft of the offsite agenda: strategy in the morning, workshops in the afternoon, dinner at 19:30.",
            "Can we keep one slot for the roadmap retro?",
            "Added it after lunch. Final agenda next week.",
        ],
    },
    ThreadTemplate {
        subject: "Weekly metrics digest",
        folder: "Inbox",
        people: &[4],
        lines: &[
            "Weekly active users are up 6%, retention stable. Search usage doubled since the new ranking shipped.",
        ],
    },
    ThreadTemplate {
        subject: "Atlas storage format RFC",
        folder: "Atlas",
        people: &[1, 4],
        lines: &[
            "I wrote an RFC comparing three storage formats for the indexer. Feedback welcome by Wednesday.",
            "Option B looks best for our read patterns. Did you benchmark compaction?",
            "Not yet, I'll add numbers in the next revision.",
        ],
    },
];

const PERSONAL_THREADS: &[ThreadTemplate] = &[
    ThreadTemplate {
        subject: "Weekend hike?",
        folder: "Inbox",
        people: &[0, 2],
        lines: &[
            "Anyone up for a hike on Saturday? Weather looks great.",
            "I'm in! Same trail as last time?",
            "Let's try the coastal path this time. Meet at 9 at the station.",
        ],
    },
    ThreadTemplate {
        subject: "Flight confirmation LIS - BER",
        folder: "Travel",
        people: &[],
        lines: &[
            "Your booking is confirmed. Flight TP 538 departs Lisbon at 07:45 and arrives in Berlin at 12:10.",
        ],
    },
    ThreadTemplate {
        subject: "Your receipt from Corner Bookshop",
        folder: "Receipts",
        people: &[],
        lines: &[
            "Thanks for your purchase! Total: 34.90 EUR. Your order will be ready for pickup tomorrow.",
        ],
    },
    ThreadTemplate {
        subject: "Dinner on Friday",
        folder: "Inbox",
        people: &[1],
        lines: &[
            "Are we still on for dinner on Friday? I booked the Italian place for 8pm.",
            "Perfect, see you there!",
        ],
    },
    ThreadTemplate {
        subject: "Apartment lease renewal",
        folder: "Inbox",
        people: &[4],
        lines: &[
            "Your lease ends on the 31st. Please confirm whether you wish to renew for another 12 months.",
            "Yes, I'd like to renew. Could you send the updated contract?",
            "Of course, the contract is attached. Please sign and return it by the end of the month.",
        ],
    },
    ThreadTemplate {
        subject: "Photos from the wedding",
        folder: "Inbox",
        people: &[3],
        lines: &[
            "I finally uploaded all the wedding photos! The link is in the shared album.",
            "They're beautiful, thank you so much for sorting them.",
        ],
    },
];

const NEWSLETTERS: &[(&str, &str, &str)] = &[
    (
        "This Week in Rust",
        "newsletter@rust-weekly.example",
        "This week: async closures stabilised, a new GPU UI framework release and 12 new crates of the week.",
    ),
    (
        "Design Weekly",
        "newsletter@designweekly.example",
        "Five onboarding patterns that actually convert, plus a deep dive into variable fonts.",
    ),
    (
        "Cloud Status",
        "noreply@cloudstatus.example",
        "Scheduled maintenance in eu-west-1 next Tuesday from 02:00 to 04:00 UTC.",
    ),
];

const CATEGORIES: &[(&str, Color)] = &[
    ("Urgent", 0xef4444),
    ("Follow up", 0xf97316),
    ("Waiting", 0xeab308),
    ("Personal", 0x22c55e),
    ("Project Atlas", 0x3b82f6),
    ("Finance", 0xa855f7),
];

const ATTACHMENTS: &[(&str, &str, u64)] = &[
    ("report.pdf", "application/pdf", 482_113),
    (
        "budget-q4.xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        91_204,
    ),
    ("mockups.png", "image/png", 1_204_332),
    (
        "contract-draft.docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        64_220,
    ),
];

/// Fills an empty store with the sample dataset. `now` anchors every date.
pub fn generate(store: &Store, now: DateTime<Utc>) -> Result<()> {
    store.transaction(|store| {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for (name, color) in CATEGORIES {
            store.save(&mut Category {
                id: 0,
                name: (*name).into(),
                color: *color,
            })?;
        }
        let work = seed_account(store, "Work", "alex.martin@northwind.example", 0x2563eb)?;
        let personal = seed_account(store, "Personal", "alex.martin@mail.example", 0x16a34a)?;
        let work_folders = seed_folders(
            store,
            &work,
            &[
                ("Projects", &["Atlas", "Helios"]),
                ("Clients", &["Acme Corp"]),
            ],
        )?;
        let personal_folders =
            seed_folders(store, &personal, &[("Travel", &[]), ("Receipts", &[])])?;
        seed_contacts(store, work.id, WORK_PEOPLE)?;
        seed_contacts(store, personal.id, PERSONAL_PEOPLE)?;

        let mut count = 0;
        let mut thread_index = 0;
        while count < MESSAGE_TARGET {
            let (account, folders, templates, people) = if thread_index % 3 == 2 {
                (
                    &personal,
                    &personal_folders,
                    PERSONAL_THREADS,
                    PERSONAL_PEOPLE,
                )
            } else {
                (&work, &work_folders, WORK_THREADS, WORK_PEOPLE)
            };
            let template = &templates[thread_index % templates.len()];
            let round = thread_index / templates.len();
            count += seed_thread(
                store,
                &mut rng,
                now,
                account,
                folders,
                template,
                people,
                round,
                thread_index,
            )?;
            if thread_index % 4 == 0 {
                count += seed_newsletter(store, &mut rng, now, &work, &work_folders, thread_index)?;
            }
            thread_index += 1;
        }

        seed_calendar(store, now, &work, &personal)?;
        seed_tasks(store, now, work.id, personal.id)?;
        seed_rules(store, &work, &personal, &personal_folders)?;
        Ok(())
    })
}

fn seed_account(store: &Store, name: &str, email: &str, color: Color) -> Result<Account> {
    let mut account = Account {
        id: 0,
        name: name.into(),
        email: email.into(),
        color,
    };
    store.save(&mut account)?;
    Ok(account)
}

/// Standard folders plus `custom` folders nested under the inbox (one extra level allowed).
fn seed_folders(
    store: &Store,
    account: &Account,
    custom: &[(&str, &[&str])],
) -> Result<Vec<Folder>> {
    let mut folders = Vec::new();
    let mut add =
        |name: &str, role: FolderRole, parent_id: Option<Id>, position: i64| -> Result<Id> {
            let mut folder = Folder {
                id: 0,
                account_id: account.id,
                parent_id,
                name: name.into(),
                role,
                position,
            };
            store.save(&mut folder)?;
            let id = folder.id;
            folders.push(folder);
            Ok(id)
        };
    let inbox = add("Inbox", FolderRole::Inbox, None, 0)?;
    for (index, (name, children)) in custom.iter().enumerate() {
        let parent = add(name, FolderRole::Custom, Some(inbox), index as i64)?;
        for (child_index, child) in children.iter().enumerate() {
            add(child, FolderRole::Custom, Some(parent), child_index as i64)?;
        }
    }
    for (position, (name, role)) in [
        ("Drafts", FolderRole::Drafts),
        ("Sent", FolderRole::Sent),
        ("Archive", FolderRole::Archive),
        ("Junk", FolderRole::Junk),
        ("Trash", FolderRole::Trash),
    ]
    .into_iter()
    .enumerate()
    {
        add(name, role, None, position as i64 + 1)?;
    }
    Ok(folders)
}

fn folder_named(folders: &[Folder], name: &str) -> Id {
    folders
        .iter()
        .find(|folder| folder.name == name)
        .or_else(|| {
            folders
                .iter()
                .find(|folder| folder.role == FolderRole::Inbox)
        })
        .map_or(0, |folder| folder.id)
}

fn folder_with_role(folders: &[Folder], role: FolderRole) -> Id {
    folders
        .iter()
        .find(|folder| folder.role == role)
        .map_or(0, |folder| folder.id)
}

fn seed_contacts(store: &Store, account_id: Id, people: &[Person]) -> Result<()> {
    for (index, person) in people.iter().enumerate() {
        store.save(&mut Contact {
            id: 0,
            account_id,
            display_name: person.name.into(),
            emails: vec![person.email.into()],
            phones: vec![person.phone.into()],
            company: person.company.into(),
            job_title: person.title.into(),
            notes: String::new(),
            favorite: index < 2,
        })?;
    }
    Ok(())
}

fn first_name(name: &str) -> &str {
    name.split_whitespace().next().unwrap_or(name)
}

fn preview_of(body: &str) -> String {
    let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(120).collect()
}

#[allow(clippy::too_many_arguments)]
fn seed_thread(
    store: &Store,
    rng: &mut Rng,
    now: DateTime<Utc>,
    account: &Account,
    folders: &[Folder],
    template: &ThreadTemplate,
    people: &[Person],
    round: usize,
    thread_index: usize,
) -> Result<usize> {
    let me = Address::new("Alex Martin", account.email.clone());
    let participants: Vec<Address> = template
        .people
        .iter()
        .filter_map(|&index| people.get(index))
        .map(|person| Address::new(person.name, person.email))
        .collect();
    let inbox_folder = folder_named(folders, template.folder);
    let sent_folder = folder_with_role(folders, FolderRole::Sent);
    let thread_id = format!("thread-{}-{thread_index}", account.id);
    let subject = if round == 0 {
        template.subject.to_owned()
    } else {
        format!("{} (#{})", template.subject, round + 1)
    };
    // Spread threads over the history window, newest threads last.
    let age_minutes = rng.below((HISTORY_DAYS * 24 * 60) as u64) as i64;
    let mut date = now - Duration::minutes(age_minutes);
    let mut written = 0;

    for (step, line) in template.lines.iter().enumerate() {
        if date > now {
            break;
        }
        // Alternate replies between participants and "me"; automated threads have no replies.
        let from_me = !participants.is_empty() && step % 2 == 1 && rng.percent(70);
        let from = if from_me {
            me.clone()
        } else if participants.is_empty() {
            // Automated sender (booking, receipts...).
            Address::new(
                format!("{} notifications", template.folder),
                format!("noreply@{}.example", template.folder.to_lowercase()),
            )
        } else {
            rng.pick(&participants).clone()
        };
        let mut to = if from_me {
            participants.clone()
        } else {
            vec![me.clone()]
        };
        let cc = if !from_me && participants.len() > 1 {
            participants
                .iter()
                .filter(|p| p.email != from.email)
                .cloned()
                .collect()
        } else {
            to.retain(|address| address.email != from.email);
            Vec::new()
        };
        let greeting = if from_me {
            participants
                .first()
                .map_or("Hi,".to_owned(), |p| format!("Hi {},", first_name(&p.name)))
        } else {
            "Hi Alex,".to_owned()
        };
        let body_text = format!(
            "{greeting}\n\n{line}\n\nBest,\n{}",
            first_name(from.display())
        );
        let age = now - date;
        let attachments = if step == 0 && rng.percent(25) {
            let (name, mime, size) = rng.pick(ATTACHMENTS);
            vec![Attachment {
                name: (*name).into(),
                mime: (*mime).into(),
                size: *size,
            }]
        } else {
            Vec::new()
        };
        let categories = if rng.percent(18) {
            vec![rng.pick(CATEGORIES).0.to_owned()]
        } else {
            Vec::new()
        };
        let mut message = Message {
            id: 0,
            account_id: account.id,
            folder_id: if from_me { sent_folder } else { inbox_folder },
            thread_id: thread_id.clone(),
            from,
            to,
            cc,
            subject: if step == 0 {
                subject.clone()
            } else {
                format!("Re: {subject}")
            },
            date,
            preview: preview_of(line),
            body_text,
            body_html: None,
            flags: MessageFlags {
                read: from_me || (age > Duration::days(2) && rng.percent(92)) || rng.percent(40),
                flagged: !from_me && rng.percent(8),
                pinned: !from_me && thread_index % 37 == 0,
            },
            importance: if rng.percent(6) {
                Importance::High
            } else {
                Importance::Normal
            },
            categories,
            attachments,
        };
        store.save(&mut message)?;
        written += 1;
        date += Duration::minutes(20 + rng.below(6 * 60) as i64);
    }
    Ok(written)
}

fn seed_newsletter(
    store: &Store,
    rng: &mut Rng,
    now: DateTime<Utc>,
    account: &Account,
    folders: &[Folder],
    index: usize,
) -> Result<usize> {
    let (name, email, text) = NEWSLETTERS[index % NEWSLETTERS.len()];
    let date = now - Duration::minutes(rng.below((HISTORY_DAYS * 24 * 60) as u64) as i64);
    let html = format!(
        "<html><body><h1>{name}</h1><p>{text}</p><p><a href=\"https://example.com\">Read more</a></p></body></html>"
    );
    store.save(&mut Message {
        id: 0,
        account_id: account.id,
        folder_id: folder_with_role(folders, FolderRole::Inbox),
        thread_id: format!("newsletter-{index}"),
        from: Address::new(name, email),
        to: vec![Address::new("Alex Martin", account.email.clone())],
        cc: Vec::new(),
        subject: format!("{name} - {}", date.format("%b %d")),
        date,
        preview: preview_of(text),
        body_text: format!("{text}\n\nRead more: https://example.com"),
        body_html: Some(html),
        flags: MessageFlags {
            read: rng.percent(60),
            flagged: false,
            pinned: false,
        },
        importance: Importance::Low,
        categories: Vec::new(),
        attachments: Vec::new(),
    })?;
    Ok(1)
}

fn at(date: NaiveDate, hour: i64, minute: i64) -> DateTime<Utc> {
    date.and_time(NaiveTime::MIN).and_utc() + Duration::minutes(hour * 60 + minute)
}

fn attendees(people: &[Person], indices: &[usize]) -> Vec<Attendee> {
    indices
        .iter()
        .filter_map(|&index| people.get(index))
        .enumerate()
        .map(|(position, person)| Attendee {
            address: Address::new(person.name, person.email),
            status: match position % 3 {
                0 => ResponseStatus::Accepted,
                1 => ResponseStatus::Tentative,
                _ => ResponseStatus::NeedsAction,
            },
            optional: position == 2,
        })
        .collect()
}

fn seed_calendar(
    store: &Store,
    now: DateTime<Utc>,
    work: &Account,
    personal: &Account,
) -> Result<()> {
    let mut work_cal = Calendar {
        id: 0,
        account_id: work.id,
        name: "Calendar".into(),
        color: 0x2563eb,
    };
    let mut team_cal = Calendar {
        id: 0,
        account_id: work.id,
        name: "Team".into(),
        color: 0x9333ea,
    };
    let mut personal_cal = Calendar {
        id: 0,
        account_id: personal.id,
        name: "Personal".into(),
        color: 0x16a34a,
    };
    let mut birthdays = Calendar {
        id: 0,
        account_id: personal.id,
        name: "Birthdays".into(),
        color: 0xf59e0b,
    };
    for calendar in [
        &mut work_cal,
        &mut team_cal,
        &mut personal_cal,
        &mut birthdays,
    ] {
        store.save(calendar)?;
    }

    let today = now.date_naive();
    let monday = today - Duration::days(i64::from(today.weekday().num_days_from_monday()));
    let history_start = monday - Duration::weeks(6);

    let add = |calendar: &Calendar,
               title: &str,
               location: &str,
               start: DateTime<Utc>,
               minutes: i64,
               recurrence: Option<&str>,
               people: &[usize]| {
        let mut event = CalendarEvent {
            id: 0,
            calendar_id: calendar.id,
            title: title.into(),
            location: location.into(),
            description: format!("{title}. Agenda and notes in the shared doc."),
            start,
            end: start + Duration::minutes(minutes),
            all_day: false,
            recurrence: recurrence.map(str::to_owned),
            attendees: attendees(WORK_PEOPLE, people),
            reminders: vec![10],
            categories: Vec::new(),
        };
        store.save(&mut event).map(|_| ())
    };

    // Recurring work events.
    add(
        &team_cal,
        "Daily standup",
        "Teams",
        at(history_start, 9, 30),
        15,
        Some("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"),
        &[0, 1, 2, 3],
    )?;
    add(
        &work_cal,
        "1:1 with Sofia",
        "Room Kepler",
        at(history_start + Duration::days(1), 14, 0),
        30,
        Some("FREQ=WEEKLY;BYDAY=TU"),
        &[0],
    )?;
    add(
        &team_cal,
        "Sprint planning",
        "Room Hubble",
        at(history_start, 10, 0),
        60,
        Some("FREQ=WEEKLY;INTERVAL=2;BYDAY=MO"),
        &[0, 1, 3],
    )?;
    add(
        &work_cal,
        "Monthly business review",
        "Board room",
        at(history_start + Duration::days(3), 16, 0),
        60,
        Some("FREQ=MONTHLY;BYDAY=1TH"),
        &[5, 3],
    )?;

    // One-off work events around today.
    add(
        &work_cal,
        "Design review: onboarding",
        "Room Kepler",
        at(today + Duration::days(1), 11, 0),
        60,
        None,
        &[2, 3],
    )?;
    add(
        &work_cal,
        "Acme renewal call",
        "Teams",
        at(today + Duration::days(2), 15, 0),
        45,
        None,
        &[6, 7],
    )?;
    add(
        &work_cal,
        "Helios launch",
        "Main stage",
        at(today + Duration::days(9), 17, 0),
        90,
        None,
        &[0, 1, 2, 3, 4],
    )?;
    add(
        &work_cal,
        "Incident postmortem",
        "Room Hubble",
        at(today - Duration::days(3), 13, 0),
        60,
        None,
        &[1, 0, 4],
    )?;
    add(
        &work_cal,
        "Interview: backend candidate",
        "Room Kepler",
        at(today + Duration::days(4), 10, 0),
        60,
        None,
        &[8],
    )?;

    // Personal events.
    let personal_event = |calendar: &Calendar,
                          title: &str,
                          start: DateTime<Utc>,
                          minutes: i64,
                          all_day: bool,
                          recurrence: Option<&str>| {
        store
            .save(&mut CalendarEvent {
                id: 0,
                calendar_id: calendar.id,
                title: title.into(),
                location: String::new(),
                description: String::new(),
                start,
                end: start + Duration::minutes(minutes),
                all_day,
                recurrence: recurrence.map(str::to_owned),
                attendees: Vec::new(),
                reminders: if all_day { vec![24 * 60] } else { vec![30] },
                categories: vec!["Personal".into()],
            })
            .map(|_| ())
    };
    personal_event(
        &personal_cal,
        "Dentist",
        at(today + Duration::days(3), 8, 30),
        45,
        false,
        None,
    )?;
    personal_event(
        &personal_cal,
        "Dinner with Sam",
        at(today + Duration::days(4), 19, 0),
        120,
        false,
        None,
    )?;
    personal_event(
        &personal_cal,
        "Gym",
        at(history_start + Duration::days(2), 18, 30),
        60,
        false,
        Some("FREQ=WEEKLY;BYDAY=WE,SA"),
    )?;
    personal_event(
        &personal_cal,
        "Team offsite",
        at(today + Duration::days(14), 0, 0),
        2 * 24 * 60,
        true,
        None,
    )?;
    personal_event(
        &birthdays,
        "Claire's birthday",
        at(today + Duration::days(6), 0, 0),
        24 * 60,
        true,
        Some("FREQ=YEARLY"),
    )?;
    Ok(())
}

fn seed_tasks(store: &Store, now: DateTime<Utc>, work_id: Id, personal_id: Id) -> Result<()> {
    let today = now.date_naive();
    let tasks: &[(Id, &str, Option<i64>, bool, Importance, &str)] = &[
        (
            work_id,
            "Review Atlas storage RFC",
            Some(1),
            false,
            Importance::High,
            "Project Atlas",
        ),
        (
            work_id,
            "Send Q4 cost breakdown to Sofia",
            Some(0),
            false,
            Importance::High,
            "Finance",
        ),
        (
            work_id,
            "Prepare Acme renewal questions",
            Some(2),
            false,
            Importance::Normal,
            "Follow up",
        ),
        (
            work_id,
            "Book offsite venue",
            Some(-1),
            false,
            Importance::Normal,
            "",
        ),
        (
            work_id,
            "Write postmortem action items",
            Some(-4),
            true,
            Importance::Normal,
            "",
        ),
        (
            work_id,
            "Update onboarding copy",
            None,
            false,
            Importance::Low,
            "",
        ),
        (
            personal_id,
            "Sign lease renewal",
            Some(5),
            false,
            Importance::High,
            "Personal",
        ),
        (
            personal_id,
            "Buy birthday gift for Claire",
            Some(4),
            false,
            Importance::Normal,
            "Personal",
        ),
        (
            personal_id,
            "Renew passport",
            Some(30),
            false,
            Importance::Normal,
            "",
        ),
        (
            personal_id,
            "Pay electricity bill",
            Some(-2),
            true,
            Importance::Normal,
            "Finance",
        ),
    ];
    for (index, (account_id, title, due_in, completed, importance, category)) in
        tasks.iter().enumerate()
    {
        store.save(&mut Task {
            id: 0,
            account_id: *account_id,
            title: (*title).into(),
            notes: String::new(),
            due: due_in.map(|days| today + Duration::days(days)),
            completed: *completed,
            importance: *importance,
            categories: if category.is_empty() {
                Vec::new()
            } else {
                vec![(*category).into()]
            },
            created: now - Duration::days(10 - index as i64),
        })?;
    }
    Ok(())
}

fn seed_rules(
    store: &Store,
    work: &Account,
    personal: &Account,
    personal_folders: &[Folder],
) -> Result<()> {
    store.save(&mut Rule {
        id: 0,
        account_id: work.id,
        name: "Newsletters are read".into(),
        enabled: true,
        conditions: vec![RuleCondition::FromContains("newsletter@".into())],
        actions: vec![RuleAction::MarkRead],
    })?;
    store.save(&mut Rule {
        id: 0,
        account_id: personal.id,
        name: "File receipts".into(),
        enabled: true,
        conditions: vec![RuleCondition::SubjectContains("receipt".into())],
        actions: vec![
            RuleAction::MoveTo(folder_named(personal_folders, "Receipts")),
            RuleAction::AddCategory("Finance".into()),
        ],
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn generates_realistic_dataset() {
        let store = Store::open_in_memory().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 11, 14, 12, 0, 0).unwrap();
        generate(&store, now).unwrap();

        let accounts = store.list_accounts().unwrap();
        assert_eq!(accounts.len(), 2);
        let folders = store.list_folders(accounts[0].id).unwrap();
        assert!(
            folders
                .iter()
                .any(|f| f.name == "Atlas" && f.parent_id.is_some())
        );

        let total: usize = accounts
            .iter()
            .flat_map(|a| store.list_folders(a.id).unwrap())
            .map(|f| store.list_messages(f.id).unwrap().len())
            .sum();
        assert!(
            (MESSAGE_TARGET..MESSAGE_TARGET + 20).contains(&total),
            "got {total} messages"
        );

        let inbox = store
            .folder_by_role(accounts[0].id, FolderRole::Inbox)
            .unwrap()
            .unwrap();
        let messages = store.list_messages(inbox.id).unwrap();
        assert!(
            messages
                .iter()
                .all(|m| m.date <= now && m.date >= now - Duration::days(HISTORY_DAYS + 1))
        );
        assert!(
            messages
                .iter()
                .any(|m| store.list_thread(&m.thread_id).unwrap().len() > 1)
        );
        assert!(store.unread_count(inbox.id).unwrap() > 0);

        let week = store
            .list_events_between(now, now + Duration::days(7))
            .unwrap();
        assert!(week.iter().any(|e| e.recurrence.is_some()));
        assert!(store.list_contacts().unwrap().len() >= 10);
        assert_eq!(store.list_tasks().unwrap().len(), 10);
        assert_eq!(store.list_categories().unwrap().len(), CATEGORIES.len());
    }
}
