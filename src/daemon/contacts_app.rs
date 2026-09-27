//! Driving Contacts.app over Apple Events.
//!
//! Only the daemon does this, for the reason sending lives here: the
//! Automation grant lands on whichever process asks, and a grant to the
//! terminal is a grant to everything the terminal runs
//! (contact-writing.md §2). Driving Contacts is a second Automation row for
//! `msgd`, separate from the Messages one, prompted on the first write.
//!
//! The scripts are deliberately dumb — find, create, read, set, append, one
//! purpose each, arguments passed to `on run` rather than interpolated — and
//! everything above them is plain Rust behind [`ContactStore`], so the tests
//! inject a fake store and `cargo test` writes nobody's contacts
//! (contact-writing.md §6).

use std::process::Command;

use crate::contacts::{ContactIndex, filed_name, handle_key};
use crate::daemon::protocol::{PersonAddRequest, PersonUpdateRequest, PersonWriteReply};
use crate::{Error, Result};

/// A multi-valued field on a card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueField {
    Phone,
    Email,
}

impl ValueField {
    /// The nouns Contacts.app's dictionary uses: one element, its
    /// containing list.
    fn nouns(self) -> (&'static str, &'static str) {
        match self {
            Self::Phone => ("phone", "phones"),
            Self::Email => ("email", "emails"),
        }
    }

    fn label(self) -> &'static str {
        self.nouns().0
    }
}

/// A single-valued field, replaced outright when it is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextField {
    Title,
    Org,
    Note,
}

impl TextField {
    /// The property name in Contacts.app's dictionary.
    fn property(self) -> &'static str {
        match self {
            Self::Title => "job title",
            Self::Org => "organization",
            Self::Note => "note",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Org => "org",
            Self::Note => "note",
        }
    }
}

/// The osascript boundary. What crosses it is small on purpose: ids, names,
/// and field values, so the logic that decides what to write sits above it
/// where the tests are.
pub trait ContactStore: Send + Sync {
    /// Ids of every person filed under `name`, composed by
    /// [`filed_name`] and compared the way the resolver compares names.
    fn find(&self, name: &str) -> Result<Vec<String>>;
    /// Create a person and answer their id. `last` may be empty.
    fn create(&self, first: &str, last: &str) -> Result<String>;
    /// Every stored value of one multi-valued field.
    fn values(&self, id: &str, field: ValueField) -> Result<Vec<String>>;
    /// Append one value to a multi-valued field.
    fn append(&self, id: &str, field: ValueField, value: &str) -> Result<()>;
    /// Replace a single-valued field.
    fn set(&self, id: &str, field: TextField, value: &str) -> Result<()>;
}

/// The production store: Contacts.app, one osascript per operation.
pub struct ContactsApp;

/// Every person as `id`, first name, last name, organization, tab-separated,
/// one per line, for [`filed_as`] to match in Rust. Matching the app's own
/// `name` would be one Apple Event instead of four, but the dictionary
/// defines `name` by "the name display order preference setting", so with
/// Contacts showing last names first it answers `Reyes Dana` and no filed
/// name ever matches.
///
/// Lists are joined rather than returned, because osascript prints a
/// returned list comma-joined — and a value can legitimately contain a
/// comma, where it can never contain a linefeed.
const FIND: &str = r#"
on run
  tell application "Contacts"
    set personIds to id of every person
    set firstNames to first name of every person
    set lastNames to last name of every person
    set organizations to organization of every person
  end tell
  set rows to {}
  set text item delimiters to tab
  repeat with i from 1 to count of personIds
    set row to {item i of personIds}
    repeat with column in {item i of firstNames, item i of lastNames, item i of organizations}
      set value to contents of column
      if value is missing value then set value to ""
      set end of row to value
    end repeat
    set end of rows to row as text
  end repeat
  set text item delimiters to linefeed
  return rows as text
end run
"#;

/// The ids among FIND's rows filed under `name`: composed by the resolver's
/// rule, whitespace collapsed, case ignored — as the resolver's own exact
/// match ignores it, and as the `whose name is` this replaced did.
fn filed_as(rows: &str, name: &str) -> Vec<String> {
    let squash = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let wanted = squash(name).to_lowercase();
    rows.lines()
        .filter_map(|row| {
            let mut columns = row.split('\t');
            let id = columns.next().filter(|id| !id.is_empty())?;
            let (first, last, org) = (columns.next(), columns.next(), columns.next());
            let filed = filed_name(first, last, org)?;
            (squash(&filed).to_lowercase() == wanted).then(|| id.to_string())
        })
        .collect()
}

const CREATE: &str = r#"
on run {firstName, lastName}
  tell application "Contacts"
    if lastName is "" then
      set newPerson to make new person with properties {first name:firstName}
    else
      set newPerson to make new person with properties {first name:firstName, last name:lastName}
    end if
    save
    return id of newPerson
  end tell
end run
"#;

fn values_script(field: ValueField) -> String {
    let (singular, _) = field.nouns();
    format!(
        r#"
on run {{personId}}
  set text item delimiters to linefeed
  tell application "Contacts"
    return (value of every {singular} of person id personId) as text
  end tell
end run
"#
    )
}

fn append_script(field: ValueField) -> String {
    let (singular, plural) = field.nouns();
    format!(
        r#"
on run {{personId, newValue}}
  tell application "Contacts"
    make new {singular} at end of {plural} of person id personId with properties {{value:newValue}}
    save
  end tell
end run
"#
    )
}

fn set_script(field: TextField) -> String {
    let property = field.property();
    format!(
        r#"
on run {{personId, newValue}}
  tell application "Contacts"
    set {property} of person id personId to newValue
    save
  end tell
end run
"#
    )
}

/// A refused Apple Event, told apart from an ordinary script failure so it
/// can exit 2 with the remedy. macOS reports it as error -1743 whether the
/// prompt was declined or the switch was later turned off.
fn classify(stderr: &str) -> Error {
    if stderr.contains("-1743") || stderr.contains("Not authorized") {
        return Error::AccessDenied(
            "msgd is not allowed to drive Contacts.\n\
             Allow it under System Settings > Privacy & Security > Automation > msgd > Contacts.\n\
             The entry appears after the first refused attempt; declining the prompt is what\n\
             creates it."
                .to_string(),
        );
    }
    Error::other(stderr.to_string())
}

fn run(script: &str, args: &[&str]) -> Result<String> {
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .args(args)
        .output()
        .map_err(|error| Error::other(format!("could not run osascript: {error}")))?;

    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end_matches(['\n', '\r'])
            .to_string());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if stderr.is_empty() {
        Error::other(format!("osascript failed: {}", output.status))
    } else {
        classify(&stderr)
    })
}

fn lines(joined: String) -> Vec<String> {
    joined
        .lines()
        .map(str::to_string)
        .filter(|line| !line.is_empty())
        .collect()
}

impl ContactStore for ContactsApp {
    fn find(&self, name: &str) -> Result<Vec<String>> {
        Ok(filed_as(&run(FIND, &[])?, name))
    }

    fn create(&self, first: &str, last: &str) -> Result<String> {
        run(CREATE, &[first, last])
    }

    fn values(&self, id: &str, field: ValueField) -> Result<Vec<String>> {
        Ok(lines(run(&values_script(field), &[id])?))
    }

    fn append(&self, id: &str, field: ValueField, value: &str) -> Result<()> {
        run(&append_script(field), &[id, value]).map(|_| ())
    }

    fn set(&self, id: &str, field: TextField, value: &str) -> Result<()> {
        run(&set_script(field), &[id, value]).map(|_| ())
    }
}

/// The fields a write carries, shared by add and update once the target
/// card is settled.
struct Fields<'a> {
    phones: &'a [String],
    emails: &'a [String],
    title: Option<&'a str>,
    org: Option<&'a str>,
    note: Option<&'a str>,
}

impl Fields<'_> {
    fn is_empty(&self) -> bool {
        self.phones.is_empty()
            && self.emails.is_empty()
            && self.title.is_none()
            && self.org.is_none()
            && self.note.is_none()
    }

    /// Refuse a malformed value before the first Apple Event, so a bad one
    /// never leaves a card half-written — or, on add, created and empty.
    fn check(&self) -> Result<()> {
        for (field, values) in [
            (ValueField::Phone, self.phones),
            (ValueField::Email, self.emails),
        ] {
            if values.iter().any(|value| value.trim().is_empty()) {
                return Err(Error::other(format!("an empty --{}", field.label())));
            }
        }
        Ok(())
    }
}

/// Nothing asked for is a usage error, said before any Apple Event runs.
fn nothing_to_write() -> Error {
    Error::other("nothing to write: pass --phone, --email, --title, --org, or --note")
}

/// Write `fields` onto the card `id`, appending phones and emails the person
/// does not already carry and replacing the rest. `cards` are every card the
/// person is filed under, `id` among them, whose values count as already
/// carried: Contacts shows them as one unified card, so a value on any of
/// them is a value the person has. A card just created passes none.
fn apply(
    store: &dyn ContactStore,
    id: &str,
    fields: &Fields<'_>,
    cards: &[String],
) -> Result<(Vec<String>, Vec<String>)> {
    let mut changed = Vec::new();
    let mut unchanged = Vec::new();

    for (field, values) in [
        (ValueField::Phone, fields.phones),
        (ValueField::Email, fields.emails),
    ] {
        if values.is_empty() {
            continue;
        }
        // Keyed the way the resolver keys handles, so a number retyped in
        // another shape is the same number rather than a second phone.
        let mut held = Vec::new();
        for card in cards {
            held.extend(
                store
                    .values(card, field)?
                    .iter()
                    .filter_map(|value| handle_key(value)),
            );
        }
        for value in values {
            let value = value.trim();
            let key = handle_key(value);
            if let Some(key) = &key
                && held.contains(key)
            {
                unchanged.push(format!("{} {value}", field.label()));
                continue;
            }
            store.append(id, field, value)?;
            changed.push(format!("{} {value}", field.label()));
            held.extend(key);
        }
    }

    for (field, value) in [
        (TextField::Title, fields.title),
        (TextField::Org, fields.org),
        (TextField::Note, fields.note),
    ] {
        if let Some(value) = value {
            store.set(id, field, value)?;
            changed.push(format!("{} {value}", field.label()));
        }
    }

    Ok((changed, unchanged))
}

/// `msg contacts add`: create the card, then write the fields onto it.
pub fn add(store: &dyn ContactStore, ask: &PersonAddRequest) -> Result<PersonWriteReply> {
    // Whitespace collapsed first, so the name the duplicate guard looks for
    // is exactly the one the card is created under: first word, then the
    // rest.
    let name = ask.name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return Err(Error::other("no name to add"));
    }
    let fields = Fields {
        phones: &ask.phones,
        emails: &ask.emails,
        title: ask.title.as_deref(),
        org: ask.org.as_deref(),
        note: ask.note.as_deref(),
    };
    // The resolver finds people by their addresses and by nothing else, so
    // a card without one could never be found again by `update` or
    // `resolve` — and would then block its own re-add as a duplicate.
    if fields.phones.is_empty() && fields.emails.is_empty() {
        return Err(Error::other(
            "add needs a --phone or --email: msg finds people by their addresses, \
             so without one it could not find them again",
        ));
    }
    fields.check()?;

    // The likely intent behind adding a name that already exists is
    // `update`, so the collision is refused rather than resolved — but only
    // refused, since two people can legitimately share a name
    // (contact-writing.md §4).
    if ask.duplicate != Some(true) {
        let held = store.find(&name)?;
        if !held.is_empty() {
            return Err(Error::other(format!(
                "{name} is already in Contacts; update them with `msg contacts update`, \
                 or pass --duplicate to add another person with this name"
            )));
        }
    }

    let (first, last) = name.split_once(' ').unwrap_or((&name, ""));
    let id = store.create(first, last)?;
    let (changed, unchanged) = apply(store, &id, &fields, &[])?;
    Ok(PersonWriteReply {
        id,
        name,
        created: true,
        changed,
        unchanged,
    })
}

/// `msg contacts update`: resolve the term the way `person` does, then
/// address Contacts.app by the filed name (contact-writing.md §7).
pub fn update(
    index: &ContactIndex,
    store: &dyn ContactStore,
    ask: &PersonUpdateRequest,
) -> Result<PersonWriteReply> {
    let fields = Fields {
        phones: &ask.phones,
        emails: &ask.emails,
        title: ask.title.as_deref(),
        org: ask.org.as_deref(),
        note: ask.note.as_deref(),
    };
    if fields.is_empty() {
        return Err(nothing_to_write());
    }
    fields.check()?;

    let person = index.person(&ask.term)?;
    // Cards are found by their filed name as `filed_name` composes it from
    // first name, last name, and organization, never by the app's `name`,
    // which follows the display-order preference. The nickname is what we
    // call them and is filed nowhere.
    let filed = person.filed_as.as_deref().unwrap_or(&person.name);
    let ids = store.find(filed)?;
    let Some(id) = ids.first() else {
        return Err(Error::other(format!(
            "Contacts.app has nobody named {filed}, though the index resolves them \
             — their record may belong to a source Contacts does not show"
        )));
    };

    let (changed, unchanged) = apply(store, id, &fields, &ids)?;
    Ok(PersonWriteReply {
        id: id.clone(),
        name: person.name,
        created: false,
        changed,
        unchanged,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Records every operation and answers from a script the test wrote.
    #[derive(Default)]
    struct Fake {
        /// Ids `find` answers with, per exact name.
        people: Vec<(&'static str, Vec<&'static str>)>,
        /// Values `values` answers with, per (id, field).
        held: Vec<((&'static str, ValueField), Vec<&'static str>)>,
        log: Mutex<Vec<String>>,
    }

    impl Fake {
        fn saw(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    impl ContactStore for Fake {
        fn find(&self, name: &str) -> Result<Vec<String>> {
            self.log.lock().unwrap().push(format!("find {name}"));
            Ok(self
                .people
                .iter()
                .find(|(held, _)| *held == name)
                .map(|(_, ids)| ids.iter().map(ToString::to_string).collect())
                .unwrap_or_default())
        }

        fn create(&self, first: &str, last: &str) -> Result<String> {
            self.log
                .lock()
                .unwrap()
                .push(format!("create {first}|{last}"));
            Ok("new-id".to_string())
        }

        fn values(&self, id: &str, field: ValueField) -> Result<Vec<String>> {
            self.log
                .lock()
                .unwrap()
                .push(format!("values {id} {field:?}"));
            Ok(self
                .held
                .iter()
                .find(|((held, kind), _)| *held == id && *kind == field)
                .map(|(_, values)| values.iter().map(ToString::to_string).collect())
                .unwrap_or_default())
        }

        fn append(&self, id: &str, field: ValueField, value: &str) -> Result<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("append {id} {field:?} {value}"));
            Ok(())
        }

        fn set(&self, id: &str, field: TextField, value: &str) -> Result<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("set {id} {field:?} {value}"));
            Ok(())
        }
    }

    fn index() -> ContactIndex {
        ContactIndex::for_test([
            ("+13105551234", "a:1", "Dana Reyes"),
            ("+14155550000", "a:2", "Dana Smith"),
        ])
    }

    #[test]
    fn add_splits_the_name_and_writes_every_field() {
        let fake = Fake::default();
        let reply = add(
            &fake,
            &PersonAddRequest {
                name: "Dana de la Reyes".into(),
                phones: vec!["(310) 555-1234".into()],
                emails: vec!["dana@example.com".into()],
                title: Some("Principal Engineer".into()),
                org: Some("Example Corp".into()),
                note: Some("referred by Sam".into()),
                duplicate: None,
            },
        )
        .unwrap();

        assert!(reply.created);
        assert_eq!(reply.id, "new-id");
        assert_eq!(reply.name, "Dana de la Reyes");
        assert_eq!(
            reply.changed,
            [
                "phone (310) 555-1234",
                "email dana@example.com",
                "title Principal Engineer",
                "org Example Corp",
                "note referred by Sam",
            ]
        );
        assert!(reply.unchanged.is_empty());
        // First word, then the rest — and a fresh card is never read back.
        let saw = fake.saw();
        assert!(
            saw.contains(&"create Dana|de la Reyes".to_string()),
            "{saw:?}"
        );
        assert!(!saw.iter().any(|op| op.starts_with("values")), "{saw:?}");
    }

    #[test]
    fn add_refuses_a_name_that_already_answers() {
        let fake = Fake {
            people: vec![("Dana Reyes", vec!["old-id"])],
            ..Fake::default()
        };
        let ask = PersonAddRequest {
            name: "Dana Reyes".into(),
            phones: vec!["3105559876".into()],
            ..Default::default()
        };
        match add(&fake, &ask) {
            Err(Error::Other(message)) => {
                assert!(message.contains("already in Contacts"), "{message}");
                assert!(message.contains("--duplicate"), "{message}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        // Refused before anything was written.
        assert!(!fake.saw().iter().any(|op| op.starts_with("create")));

        // Said on purpose, the same request goes through.
        let again = add(
            &fake,
            &PersonAddRequest {
                duplicate: Some(true),
                ..ask
            },
        )
        .unwrap();
        assert!(again.created);
    }

    /// Stray whitespace cannot slip a name past the duplicate guard: the name
    /// looked for is the name that would be created.
    #[test]
    fn add_checks_the_name_it_would_create() {
        let fake = Fake {
            people: vec![("Dana Reyes", vec!["old-id"])],
            ..Fake::default()
        };
        let outcome = add(
            &fake,
            &PersonAddRequest {
                name: " Dana   Reyes ".into(),
                phones: vec!["3105559876".into()],
                ..Default::default()
            },
        );
        match outcome {
            Err(Error::Other(message)) => {
                assert!(message.starts_with("Dana Reyes is"), "{message}")
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert_eq!(fake.saw(), ["find Dana Reyes"]);
    }

    /// A card with no address is one the resolver never indexes, so it is
    /// refused rather than created unreachable.
    #[test]
    fn add_needs_a_phone_or_email() {
        let fake = Fake::default();
        let outcome = add(
            &fake,
            &PersonAddRequest {
                name: "Robin Adeyemi".into(),
                title: Some("Principal Engineer".into()),
                ..Default::default()
            },
        );
        match outcome {
            Err(Error::Other(message)) => assert!(message.contains("--phone or --email")),
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(fake.saw().is_empty(), "{:?}", fake.saw());
    }

    #[test]
    fn add_needs_a_name() {
        assert!(matches!(
            add(
                &Fake::default(),
                &PersonAddRequest {
                    name: "  ".into(),
                    ..Default::default()
                }
            ),
            Err(Error::Other(_))
        ));
    }

    /// An empty value anywhere in the request is refused before anything is
    /// written, not after the values ahead of it went in.
    #[test]
    fn an_empty_value_is_refused_before_any_write() {
        let fake = Fake::default();
        let outcome = add(
            &fake,
            &PersonAddRequest {
                name: "Robin Adeyemi".into(),
                phones: vec!["3105559876".into()],
                emails: vec![" ".into()],
                ..Default::default()
            },
        );
        assert!(matches!(outcome, Err(Error::Other(_))), "{outcome:?}");
        assert!(fake.saw().is_empty(), "{:?}", fake.saw());

        let fake = Fake {
            people: vec![("Dana Reyes", vec!["card-1"])],
            ..Fake::default()
        };
        let outcome = update(
            &index(),
            &fake,
            &PersonUpdateRequest {
                term: "dana reyes".into(),
                phones: vec!["3105559999".into(), "".into()],
                ..Default::default()
            },
        );
        assert!(matches!(outcome, Err(Error::Other(_))), "{outcome:?}");
        assert!(fake.saw().is_empty(), "{:?}", fake.saw());
    }

    #[test]
    fn update_appends_what_is_new_and_skips_what_is_held() {
        let fake = Fake {
            people: vec![("Dana Reyes", vec!["card-1"])],
            // The card holds the number in a different shape than the
            // request retypes it, which is exactly what must not duplicate.
            held: vec![(("card-1", ValueField::Phone), vec!["+1 (310) 555-1234"])],
            ..Fake::default()
        };
        let reply = update(
            &index(),
            &fake,
            &PersonUpdateRequest {
                term: "dana reyes".into(),
                phones: vec!["310-555-1234".into(), "3105559999".into()],
                title: Some("Staff Engineer".into()),
                ..Default::default()
            },
        )
        .unwrap();

        assert!(!reply.created);
        assert_eq!(reply.id, "card-1");
        assert_eq!(reply.name, "Dana Reyes");
        assert_eq!(reply.changed, ["phone 3105559999", "title Staff Engineer"]);
        assert_eq!(reply.unchanged, ["phone 310-555-1234"]);
        let saw = fake.saw();
        assert!(
            saw.contains(&"append card-1 Phone 3105559999".to_string()),
            "{saw:?}"
        );
        assert!(
            !saw.contains(&"append card-1 Phone 310-555-1234".to_string()),
            "{saw:?}"
        );
    }

    /// A person filed as several cards is one person: a value any of them
    /// carries is already there, even though the write goes to the first.
    #[test]
    fn update_skips_a_value_another_of_their_cards_carries() {
        let fake = Fake {
            people: vec![("Dana Reyes", vec!["card-1", "card-2"])],
            held: vec![(("card-2", ValueField::Email), vec!["dana@example.com"])],
            ..Fake::default()
        };
        let reply = update(
            &index(),
            &fake,
            &PersonUpdateRequest {
                term: "dana reyes".into(),
                emails: vec!["Dana@Example.com".into()],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(reply.id, "card-1");
        assert!(reply.changed.is_empty(), "{:?}", reply.changed);
        assert_eq!(reply.unchanged, ["email Dana@Example.com"]);
        assert!(!fake.saw().iter().any(|op| op.starts_with("append")));
    }

    /// The term resolves the way `person` resolves, refusals included: a
    /// fragment two people answer to is exit 3, not a pick.
    #[test]
    fn update_refuses_an_ambiguous_term() {
        let fake = Fake::default();
        let outcome = update(
            &index(),
            &fake,
            &PersonUpdateRequest {
                term: "dana".into(),
                title: Some("Engineer".into()),
                ..Default::default()
            },
        );
        assert!(matches!(outcome, Err(Error::Ambiguous(_))), "{outcome:?}");
        assert!(fake.saw().is_empty(), "nothing may be written");
    }

    /// The card is addressed by the filed name, not the nickname shown for
    /// it — `find` composes names from first and last name, and a nickname
    /// is neither.
    #[test]
    fn update_addresses_the_card_by_its_filed_name() {
        let index = ContactIndex::for_test([("+13105551234", "a:1", "Dana Reyes")])
            .nicknamed("+13105551234", "Dee");
        let fake = Fake {
            people: vec![("Dana Reyes", vec!["card-1"])],
            ..Fake::default()
        };
        let reply = update(
            &index,
            &fake,
            &PersonUpdateRequest {
                term: "dee".into(),
                org: Some("Example Corp".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(reply.name, "Dee");
        assert!(fake.saw().contains(&"find Dana Reyes".to_string()));
    }

    #[test]
    fn update_with_nothing_to_write_is_refused_before_resolving() {
        let outcome = update(
            &index(),
            &Fake::default(),
            &PersonUpdateRequest {
                term: "dana".into(),
                ..Default::default()
            },
        );
        match outcome {
            // Refused as usage, not as ambiguity: the term was never looked at.
            Err(Error::Other(message)) => assert!(message.contains("nothing to write")),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn update_says_when_the_app_does_not_hold_the_person() {
        let outcome = update(
            &ContactIndex::for_test([("+13105551234", "a:1", "Dana Reyes")]),
            &Fake::default(),
            &PersonUpdateRequest {
                term: "dana".into(),
                note: Some("hello".into()),
                ..Default::default()
            },
        );
        match outcome {
            Err(Error::Other(message)) => {
                assert!(message.contains("nobody named Dana Reyes"), "{message}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// Cards are matched by the name the resolver files them under, built
    /// from first and last name, so the app's display order never enters
    /// into it.
    #[test]
    fn find_matches_the_filed_name_whatever_the_display_order() {
        let rows = [
            "card-1\tDana\tReyes\t",
            "card-2\tdana \tREYES\tExample Corp",
            "card-3\tReyes\tDana\t",
            "card-4\t\t\tExample Corp",
            "card-5\tSam\t\t",
            "\tDana\tReyes\t",
        ]
        .join("\n");
        assert_eq!(filed_as(&rows, "Dana  Reyes"), ["card-1", "card-2"]);
        assert_eq!(filed_as(&rows, "example corp"), ["card-4"]);
        assert_eq!(filed_as(&rows, "Sam"), ["card-5"]);
        assert!(filed_as(&rows, "Dana").is_empty());
        assert!(filed_as("", "Dana Reyes").is_empty());
    }

    /// A refused Apple Event exits 2 with the remedy; anything else a script
    /// says is an ordinary error.
    #[test]
    fn a_refused_apple_event_is_access_denied() {
        let refused =
            classify("execution error: Not authorized to send Apple events to Contacts. (-1743)");
        match refused {
            Error::AccessDenied(message) => assert!(message.contains("Automation"), "{message}"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            classify("execution error: Contacts got an error (-1728)"),
            Error::Other(_)
        ));
    }
}
