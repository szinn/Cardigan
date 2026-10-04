use std::collections::{BTreeMap, HashMap, HashSet};

use crate::{
    contact::{ETag, Href, Side, VCard, VCardError},
    state::ContactState,
};

/// One resource in a side's listing this cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// Same href and ETag as its state row: not fetched, not changed. Only
    /// for hrefs `fetch_lists` did not return.
    Unchanged(ETag),
    /// Fetched this cycle. A card that failed to parse carries its error.
    Fetched { etag: ETag, card: Result<VCard, VCardError> },
    /// Present, but not to be acted on this cycle: a failing card not yet due
    /// for retry. Never treated as deleted, never paired.
    Held(ETag),
}

/// The complete membership of one side's collection: every href it holds
/// and nothing else. A synced contact whose UID appears nowhere here was
/// deleted, so a partial listing (a raw sync-collection delta) must never be
/// passed in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    entries: BTreeMap<Href, Entry>,
}

impl Snapshot {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, href: Href, entry: Entry) {
        self.entries.insert(href, entry);
    }

    pub fn get(&self, href: &Href) -> Option<&Entry> {
        self.entries.get(href)
    }

    /// Entries in href order.
    pub fn entries(&self) -> impl Iterator<Item = (&Href, &Entry)> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl FromIterator<(Href, Entry)> for Snapshot {
    fn from_iter<I: IntoIterator<Item = (Href, Entry)>>(iter: I) -> Self {
        Self {
            entries: iter.into_iter().collect(),
        }
    }
}

/// The hrefs to multiget on each side this cycle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchLists {
    pub icloud: Vec<Href>,
    pub fastmail: Vec<Href>,
}

/// Phase A: the hrefs of each side's listing that must be fetched. An href
/// whose ETag equals its state row's ETag on that side is unchanged. That
/// check is also echo suppression: the daemon's own writes come back as
/// unchanged. Every other href (new, or with a changed ETag) is fetched, in
/// listing order. For every row, when one side's card is fetched or gone from
/// its stored href, the other side's card is fetched as well, so a write to it
/// can keep its photo and never replaces a card the daemon has not read
/// (Decision 7). A row whose photos are not yet tracked has both its listed
/// sides fetched, so the planner can record them (CG-15 Decision 2).
pub fn fetch_lists(icloud: &[(Href, ETag)], fastmail: &[(Href, ETag)], state: &[ContactState]) -> FetchLists {
    let mut lists = FetchLists {
        icloud: changed(Side::ICloud, icloud, state),
        fastmail: changed(Side::Fastmail, fastmail, state),
    };
    // Built once, not on every state row: `is_listed`/`Vec::contains` inside
    // the loop below made this quadratic in the number of state rows.
    let icloud_listed: HashSet<&Href> = icloud.iter().map(|(href, _)| href).collect();
    let fastmail_listed: HashSet<&Href> = fastmail.iter().map(|(href, _)| href).collect();
    let icloud_fetch: HashSet<&Href> = lists.icloud.iter().collect();
    let fastmail_fetch: HashSet<&Href> = lists.fastmail.iter().collect();
    let mut extra_icloud = Vec::new();
    let mut extra_fastmail = Vec::new();
    for row in state {
        let (i, f) = (&row.icloud.href, &row.fastmail.href);
        let untracked = !row.photo.tracked;
        if (untracked || touched(f, &fastmail_listed, &fastmail_fetch)) && icloud_listed.contains(i) && !icloud_fetch.contains(i) {
            extra_icloud.push(i.clone());
        }
        if (untracked || touched(i, &icloud_listed, &icloud_fetch)) && fastmail_listed.contains(f) && !fastmail_fetch.contains(f) {
            extra_fastmail.push(f.clone());
        }
    }
    lists.icloud.extend(extra_icloud);
    lists.fastmail.extend(extra_fastmail);
    lists
}

/// Hrefs whose ETag differs from the state row's for `side`, or that state
/// does not know.
fn changed(side: Side, listing: &[(Href, ETag)], state: &[ContactState]) -> Vec<Href> {
    let stored: HashMap<&Href, &ETag> = state
        .iter()
        .map(|row| {
            let resource = row.side(side);
            (&resource.href, &resource.etag)
        })
        .collect();
    listing
        .iter()
        .filter(|(href, etag)| stored.get(href) != Some(&etag))
        .map(|(href, _)| href.clone())
        .collect()
}

/// The stored card is being fetched, or is no longer at its stored href.
fn touched(stored: &Href, listed: &HashSet<&Href>, fetch: &HashSet<&Href>) -> bool {
    fetch.contains(stored) || !listed.contains(stored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::fixtures::{card, row};

    fn listing(entries: &[(&str, &str)]) -> Vec<(Href, ETag)> {
        entries.iter().map(|(href, etag)| (Href::from(*href), ETag::from(*etag))).collect()
    }

    fn state() -> Vec<ContactState> {
        vec![row(1, &card("u1", "Jane Doe"), ("/i/u1.vcf", "i1"), ("/f/u1.vcf", "f1"))]
    }

    fn lists(icloud: &[(&str, &str)], fastmail: &[(&str, &str)], state: &[ContactState]) -> FetchLists {
        fetch_lists(&listing(icloud), &listing(fastmail), state)
    }

    fn hrefs(values: &[&str]) -> Vec<Href> {
        values.iter().map(|v| Href::from(*v)).collect()
    }

    #[test]
    fn unchanged_hrefs_are_not_fetched() {
        let fetch = lists(&[("/i/u1.vcf", "i1"), ("/i/u2.vcf", "i7")], &[("/f/u1.vcf", "f1")], &state());
        assert_eq!(fetch.icloud, hrefs(&["/i/u2.vcf"]));
        assert_eq!(fetch.fastmail, hrefs(&[]));
    }

    #[test]
    fn changed_etag_fetches_the_other_side_too() {
        let fetch = lists(&[("/i/u1.vcf", "i2")], &[("/f/u1.vcf", "f1")], &state());
        assert_eq!(fetch.icloud, hrefs(&["/i/u1.vcf"]));
        assert_eq!(fetch.fastmail, hrefs(&["/f/u1.vcf"]));
    }

    #[test]
    fn stored_etags_are_per_side() {
        // Fastmail's listing is compared with Fastmail's stored resource only.
        let fetch = lists(&[("/i/u1.vcf", "i1")], &[("/i/u1.vcf", "i1"), ("/f/u1.vcf", "f1")], &state());
        assert_eq!(fetch.icloud, hrefs(&[]));
        assert_eq!(fetch.fastmail, hrefs(&["/i/u1.vcf"]));
    }

    #[test]
    fn gone_card_fetches_the_other_side_too() {
        // Gone from its stored href (moved or deleted) counts as touched.
        let fetch = lists(&[("/i/u1.vcf", "i1")], &[], &state());
        assert_eq!(fetch.icloud, hrefs(&["/i/u1.vcf"]));
        assert_eq!(fetch.fastmail, hrefs(&[]));
    }

    #[test]
    fn snapshot_collects_entries_in_href_order() {
        let snapshot: Snapshot = [
            (Href::from("/b.vcf"), Entry::Held(ETag::from("b"))),
            (Href::from("/a.vcf"), Entry::Unchanged(ETag::from("a"))),
        ]
        .into_iter()
        .collect();
        let hrefs: Vec<&str> = snapshot.entries().map(|(href, _)| href.as_str()).collect();
        assert_eq!(hrefs, ["/a.vcf", "/b.vcf"]);
        assert_eq!(snapshot.len(), 2);
        assert!(!snapshot.is_empty());
    }

    #[test]
    fn untracked_rows_are_fetched_on_both_sides() {
        let mut untracked = state();
        untracked[0].photo.tracked = false;
        let mut tracked = state();
        tracked[0].photo.tracked = true;

        let fetch = lists(&[("/i/u1.vcf", "i1")], &[("/f/u1.vcf", "f1")], &untracked);
        assert_eq!((fetch.icloud, fetch.fastmail), (hrefs(&["/i/u1.vcf"]), hrefs(&["/f/u1.vcf"])));

        let fetch = lists(&[("/i/u1.vcf", "i1")], &[("/f/u1.vcf", "f1")], &tracked);
        assert_eq!((fetch.icloud, fetch.fastmail), (hrefs(&[]), hrefs(&[])));
    }
}
