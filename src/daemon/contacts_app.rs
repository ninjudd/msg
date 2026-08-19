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

use crate::contacts::{ContactIndex, handle_key};
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
    /// Ids of every person whose name is exactly `name`.
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

/// The list is joined with linefeeds rather than returned as a list, because
/// osascript prints a returned list comma-joined — and a phone value can
/// legitimately contain a comma, where it can never contain a linefeed.
const FIND: &str = r#"
on run {personName}
  set text item delimiters to linefeed
  tell application "Contacts"
    return (id of every person whose name is personName) as text
  end tell
end run
"#;

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
        Ok(lines(run(FIND, &[name])?))
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
}

/// Nothing asked for is a usage error, said before any Apple Event runs.
fn nothing_to_write() -> Error {
    Error::other("nothing to write: pass --phone, --email, --title, --org, or --note")
}

/// Write `fields` onto the card `id`, appending phones and emails the card
/// does not already carry and replacing the rest. `existing` holds the
/// card's current values per multi-valued field, `None` when the card was
/// just created and holds nothing.
fn apply(
    store: &dyn ContactStore,
    id: &str,
    fields: &Fields<'_>,
    read_existing: bool,
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
        let mut held: Vec<String> = if read_existing {
            store.values(id, field)?
        } else {
            Vec::new()
        }
        .iter()
        .filter_map(|value| handle_key(value))
        .collect();
        for value in values {
            let value = value.trim();
            if value.is_empty() {
                return Err(Error::other(format!("an empty --{}", field.label())));
            }
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
    let name = ask.name.trim();
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

    // The likely intent behind adding a name that already exists is
    // `update`, so the collision is refused rather than resolved — but only
    // refused, since two people can legitimately share a name
    // (contact-writing.md §4).
    if ask.duplicate != Some(true) {
        let held = store.find(name)?;
        if !held.is_empty() {
            return Err(Error::other(format!(
                "{name} is already in Contacts; update them with `msg contacts update`, \
                 or pass --duplicate to add another person with this name"
            )));
        }
    }

    let (first, last) = match name.split_once(' ') {
        Some((first, rest)) => (first, rest.trim()),
        None => (name, ""),
    };
    let id = store.create(first, last)?;
    let (changed, unchanged) = apply(store, &id, &fields, false)?;
    Ok(PersonWriteReply {
        id,
        name: name.to_string(),
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

    let person = index.person(&ask.term)?;
    // The name the card is filed under is the one Contacts.app's `name`
    // answers to; the nickname is what we call them, not what the app does.
    let filed = person.filed_as.as_deref().unwrap_or(&person.name);
    let ids = store.find(filed)?;
    let Some(id) = ids.first() else {
        return Err(Error::other(format!(
            "Contacts.app has nobody named {filed}, though the index resolves them \
             — their record may belong to a source Contacts does not show"
        )));
    };

    let (changed, unchanged) = apply(store, id, &fields, true)?;
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
    /// it — the app's `name` answers to the former.
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
