use std::{collections::BTreeSet, fmt};

use super::{
    VCard,
    text::{split_components, unescape},
};

/// The only contact data that may be logged: name and company.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayIdentity {
    name: Option<String>,
    org: Option<String>,
}

impl DisplayIdentity {
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn org(&self) -> Option<&str> {
        self.org.as_deref()
    }
}

impl fmt::Display for DisplayIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name.as_deref().unwrap_or("<no name>"))?;
        match &self.org {
            Some(org) => write!(f, " ({org})"),
            None => Ok(()),
        }
    }
}

/// Normalized keys for baseline pass 3 (identity heuristic). Contains
/// emails and phone numbers — PII — so `Debug` shows counts only and this
/// must never be logged.
#[derive(Clone, PartialEq, Eq)]
pub struct MatchKeys {
    name: Option<String>,
    emails: BTreeSet<String>,
    phones: BTreeSet<String>,
    org: Option<String>,
}

impl MatchKeys {
    /// Same normalized full name AND (a shared email or phone, or — when
    /// neither card has any email or phone — the same ORG).
    pub fn is_match(&self, other: &Self) -> bool {
        let (Some(name), Some(other_name)) = (&self.name, &other.name) else {
            return false;
        };
        if name != other_name {
            return false;
        }
        if !self.has_contact_points() && !other.has_contact_points() {
            return self.org.is_some() && self.org == other.org;
        }
        !self.emails.is_disjoint(&other.emails) || !self.phones.is_disjoint(&other.phones)
    }

    fn has_contact_points(&self) -> bool {
        !self.emails.is_empty() || !self.phones.is_empty()
    }
}

impl fmt::Debug for MatchKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MatchKeys")
            .field("emails", &self.emails.len())
            .field("phones", &self.phones.len())
            .finish_non_exhaustive()
    }
}

impl VCard {
    /// Name (`FN`, else `N`) and first `ORG` component, unescaped and on one
    /// line. Safe to log.
    pub fn display_identity(&self) -> DisplayIdentity {
        DisplayIdentity {
            name: self.display_name(),
            org: self.organization(),
        }
    }

    pub fn match_keys(&self) -> MatchKeys {
        MatchKeys {
            name: self.display_name().map(|name| name.to_lowercase()),
            emails: self
                .properties_named("EMAIL")
                .filter_map(|p| non_empty(unescape(p.value()).trim().to_lowercase()))
                .collect(),
            phones: self.properties_named("TEL").filter_map(|p| normalize_phone(p.value())).collect(),
            org: self.organization().map(|org| org.to_lowercase()),
        }
    }

    fn display_name(&self) -> Option<String> {
        let name = self
            .properties_named("FN")
            .find_map(|p| non_empty(collapse(&unescape(p.value()))))
            .or_else(|| self.name_from_n())?;
        (!looks_like_contact_point(&name)).then_some(name)
    }

    /// `N` is Family;Given;Middle;Prefix;Suffix — shown as
    /// "Prefix Given Middle Family Suffix".
    fn name_from_n(&self) -> Option<String> {
        let n = self.properties_named("N").next()?;
        let parts = split_components(n.value());
        let part = |i: usize| parts.get(i).map_or_default(|s| unescape(s));
        non_empty(collapse(&[3, 1, 2, 0, 4].map(part).join(" ")))
    }

    fn organization(&self) -> Option<String> {
        let org = self.properties_named("ORG").next()?;
        non_empty(collapse(&unescape(split_components(org.value())[0])))
    }
}

fn collapse(s: &str) -> String {
    let without_control: String = s.chars().filter(|c| !c.is_control()).collect();
    without_control.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// An email address or phone number, used as `FN` by contacts with no name.
/// The PII policy forbids ever displaying either, so such an `FN` is treated
/// as no name.
fn looks_like_contact_point(s: &str) -> bool {
    if s.contains('@') {
        return true;
    }
    let has_digit = s.chars().any(|c| c.is_ascii_digit());
    let phone_chars_only = s
        .chars()
        .all(|c| c.is_ascii_digit() || c.is_whitespace() || matches!(c, '+' | '-' | '(' | ')' | '.'));
    has_digit && phone_chars_only
}

fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

/// Digits only, keeping a leading `+`. `None` when there are no digits.
fn normalize_phone(value: &str) -> Option<String> {
    let digits: String = value.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    Some(if value.trim_start().starts_with('+') { format!("+{digits}") } else { digits })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(body: &[&str]) -> VCard {
        let mut lines = vec!["BEGIN:VCARD", "VERSION:3.0", "UID:u1"];
        lines.extend_from_slice(body);
        lines.push("END:VCARD");
        VCard::parse(lines.join("\r\n")).expect("card parses")
    }

    #[test]
    fn display_uses_fn_and_first_org_component() {
        let identity = card(&["FN:Jane Doe", "ORG:Acme Corp;Engineering"]).display_identity();
        assert_eq!(identity.to_string(), "Jane Doe (Acme Corp)");
        assert_eq!(identity.name(), Some("Jane Doe"));
        assert_eq!(identity.org(), Some("Acme Corp"));
    }

    #[test]
    fn display_unescapes_and_collapses_whitespace() {
        assert_eq!(
            card(&["FN:Doe\\, Jane", "ORG:Smith\\; Sons"]).display_identity().to_string(),
            "Doe, Jane (Smith; Sons)"
        );
        assert_eq!(card(&["FN:Jane\\n  Doe"]).display_identity().to_string(), "Jane Doe");
    }

    #[test]
    fn display_falls_back_to_n() {
        assert_eq!(card(&["FN:", "N:Doe;Jane;Q;Dr.;Jr."]).display_identity().to_string(), "Dr. Jane Q Doe Jr.");
        assert_eq!(card(&["N:Doe;Jane;;;"]).display_identity().to_string(), "Jane Doe");
    }

    #[test]
    fn display_without_name() {
        assert_eq!(card(&[]).display_identity().to_string(), "<no name>");
        assert_eq!(card(&["ORG:Acme"]).display_identity().to_string(), "<no name> (Acme)");
    }

    #[test]
    fn display_treats_email_or_phone_fn_as_no_name() {
        assert_eq!(card(&["FN:jane@example.com"]).display_identity().to_string(), "<no name>");
        assert_eq!(card(&["FN:+1 (555) 010-0100"]).display_identity().to_string(), "<no name>");
        assert_eq!(card(&["FN:Agent 47"]).display_identity().to_string(), "Agent 47");
    }

    #[test]
    fn display_strips_control_characters() {
        let display = card(&["FN:Jane\u{1b}[31m Doe"]).display_identity().to_string();
        assert!(!display.contains('\u{1b}'), "{display}");
    }

    #[test]
    fn match_keys_normalize_emails_and_phones() {
        let keys = card(&["FN:Jane  DOE", "EMAIL: Jane@Example.COM ", "TEL:+1 (555) 010-0100", "TEL:555.0199", "TEL:ext"]).match_keys();
        assert_eq!(keys.name.as_deref(), Some("jane doe"));
        assert_eq!(keys.emails, BTreeSet::from(["jane@example.com".to_owned()]));
        assert_eq!(keys.phones, BTreeSet::from(["+15550100100".to_owned(), "5550199".to_owned()]));
    }

    #[test]
    fn same_name_and_shared_contact_point_match() {
        let a = card(&["FN:Jane Doe", "EMAIL:jane@example.com", "EMAIL:other@example.com"]).match_keys();
        let b = card(&["FN:jane  doe", "EMAIL:JANE@example.com"]).match_keys();
        assert!(a.is_match(&b));
        let c = card(&["FN:Jane Doe", "TEL:+1 555 0100"]).match_keys();
        let d = card(&["FN:Jane Doe", "TEL:+1 (555) 0100", "EMAIL:x@example.com"]).match_keys();
        assert!(c.is_match(&d));
    }

    #[test]
    fn same_name_without_shared_contact_point_does_not_match() {
        let a = card(&["FN:Jane Doe", "EMAIL:a@example.com"]).match_keys();
        let b = card(&["FN:Jane Doe", "EMAIL:b@example.com"]).match_keys();
        assert!(!a.is_match(&b));
        let no_contact = card(&["FN:Jane Doe", "ORG:Acme"]).match_keys();
        assert!(!a.is_match(&no_contact));
    }

    #[test]
    fn different_names_never_match() {
        let a = card(&["FN:Jane Doe", "EMAIL:jane@example.com"]).match_keys();
        let b = card(&["FN:Janet Doe", "EMAIL:jane@example.com"]).match_keys();
        assert!(!a.is_match(&b));
        assert!(!card(&[]).match_keys().is_match(&card(&[]).match_keys()));
    }

    #[test]
    fn org_decides_only_when_neither_card_has_contact_points() {
        let a = card(&["FN:Jane Doe", "ORG:Acme;Sales"]).match_keys();
        let b = card(&["FN:Jane Doe", "ORG:ACME"]).match_keys();
        assert!(a.is_match(&b));
        assert!(!a.is_match(&card(&["FN:Jane Doe", "ORG:Other"]).match_keys()));
        assert!(!card(&["FN:Jane Doe"]).match_keys().is_match(&card(&["FN:Jane Doe"]).match_keys()));
    }

    #[test]
    fn match_keys_debug_is_redacted() {
        let debug = format!("{:?}", card(&["FN:Jane Doe", "EMAIL:jane@example.com", "TEL:+1 555 0100"]).match_keys());
        assert!(!debug.contains("jane"), "{debug}");
        assert!(!debug.contains("555"), "{debug}");
    }
}
