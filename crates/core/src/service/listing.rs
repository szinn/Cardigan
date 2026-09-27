//! The planner's input: what the store held, both sides' complete listings,
//! and the snapshots built from them.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};

use super::{Collections, SyncService};
use crate::{
    AddressBookError, Error,
    addressbook::{AddressBook, Changes, Collection, FetchedCard, SyncToken},
    contact::{ETag, Href, Side, VCard},
    state::{CardFailure, ContactState},
    sync::{Entry, Snapshot, fetch_lists},
    with_read_only_transaction,
};

/// What the state store held when the cycle started.
#[derive(Debug, Default)]
pub(super) struct Stored {
    pub(super) contacts: Vec<ContactState>,
    pub(super) failures: Vec<CardFailure>,
    pub(super) icloud_token: Option<SyncToken>,
    pub(super) fastmail_token: Option<SyncToken>,
}

impl Stored {
    pub(super) fn token(&self, side: Side) -> Option<&SyncToken> {
        match side {
            Side::ICloud => self.icloud_token.as_ref(),
            Side::Fastmail => self.fastmail_token.as_ref(),
        }
    }
}

/// One side's complete membership this cycle.
#[derive(Debug, Default)]
pub(super) struct SideListing {
    pub(super) entries: Vec<(Href, ETag)>,
    /// The token to store once the cycle completes; `None` when the
    /// collection has no `sync-collection`.
    pub(super) token: Option<SyncToken>,
}

impl SideListing {
    pub(super) fn etag(&self, href: &Href) -> Option<ETag> {
        self.entries.iter().find(|(listed, _)| listed == href).map(|(_, etag)| etag.clone())
    }
}

#[derive(Debug, Default)]
pub(super) struct Listed {
    pub(super) icloud: SideListing,
    pub(super) fastmail: SideListing,
}

impl Listed {
    pub(super) fn side(&self, side: Side) -> &SideListing {
        match side {
            Side::ICloud => &self.icloud,
            Side::Fastmail => &self.fastmail,
        }
    }
}

/// The planner's input, plus what was fetched to build it.
#[derive(Debug, Default)]
pub(super) struct Built {
    pub(super) icloud: Snapshot,
    pub(super) fastmail: Snapshot,
    /// Every `(side, href)` fetched this cycle.
    pub(super) fetched: HashSet<(Side, Href)>,
}

impl Built {
    pub(super) fn fetched_on(&self, side: Side) -> usize {
        self.fetched.iter().filter(|(fetched, _)| *fetched == side).count()
    }
}

impl SyncService {
    pub(super) async fn load(&self) -> Result<Stored, Error> {
        with_read_only_transaction!(self, contact_state_repository, card_failure_repository, endpoint_repository, |tx| {
            let contacts = contact_state_repository.list_all(tx).await?;
            let failures = card_failure_repository.list_all(tx).await?;
            let icloud = endpoint_repository.find(tx, Side::ICloud).await?;
            let fastmail = endpoint_repository.find(tx, Side::Fastmail).await?;
            Ok(Stored {
                contacts,
                failures,
                icloud_token: icloud.and_then(|endpoint| endpoint.sync_token).map(SyncToken::from),
                fastmail_token: fastmail.and_then(|endpoint| endpoint.sync_token).map(SyncToken::from),
            })
        })
    }

    /// Both sides' complete membership.
    pub(super) async fn list(&self, collections: &Collections) -> Result<Listed, Error> {
        let listed = async {
            Ok::<_, Error>(Listed {
                icloud: list_side(&*self.icloud, &collections.icloud).await?,
                fastmail: list_side(&*self.fastmail, &collections.fastmail).await?,
            })
        }
        .await;
        listed.inspect_err(|error| self.after_listing_error(error))
    }

    /// Both snapshots. Phase A (`fetch_lists`) picks what to fetch; a card
    /// whose failure group is not yet due is `Held` and not fetched (I1:
    /// every card of one failed op releases together — see `held_hrefs`); a
    /// card the multiget no longer finds was deleted since the listing and
    /// is left out.
    pub(super) async fn build(&self, listed: &Listed, stored: &Stored, now: DateTime<Utc>) -> Result<Built, Error> {
        let lists = fetch_lists(&listed.icloud.entries, &listed.fastmail.entries, &stored.contacts);
        let held = held_hrefs(listed, &stored.failures, now);
        let mut built = Built::default();
        built.icloud = self.snapshot(Side::ICloud, &listed.icloud, &lists.icloud, &held, &mut built.fetched).await?;
        built.fastmail = self
            .snapshot(Side::Fastmail, &listed.fastmail, &lists.fastmail, &held, &mut built.fetched)
            .await?;
        Ok(built)
    }

    async fn snapshot(
        &self,
        side: Side,
        listing: &SideListing,
        wanted: &[Href],
        held: &HashSet<(Side, Href)>,
        fetched: &mut HashSet<(Side, Href)>,
    ) -> Result<Snapshot, Error> {
        let is_held = |href: &Href| held.contains(&(side, href.clone()));
        let to_fetch: Vec<Href> = wanted.iter().filter(|href| !is_held(href)).cloned().collect();
        let found: HashMap<Href, FetchedCard> = if to_fetch.is_empty() {
            HashMap::new()
        } else {
            self.book(side)
                .multiget(&to_fetch)
                .await?
                .found
                .into_iter()
                .map(|card| (card.href.clone(), card))
                .collect()
        };
        let to_fetch: HashSet<Href> = to_fetch.into_iter().collect();

        let mut snapshot = Snapshot::new();
        for (href, etag) in &listing.entries {
            let entry = if is_held(href) {
                Entry::Held(etag.clone())
            } else if to_fetch.contains(href) {
                let Some(card) = found.get(href) else {
                    continue;
                };
                fetched.insert((side, href.clone()));
                Entry::Fetched {
                    etag: card.etag.clone(),
                    card: VCard::parse(card.body.clone()),
                }
            } else {
                Entry::Unchanged(etag.clone())
            };
            snapshot.insert(href.clone(), entry);
        }
        Ok(snapshot)
    }
}

/// The `(side, href)`s to hold rather than fetch: every failure row whose
/// UID group is not yet due (I1). Cards of one failed op share the op's UID
/// (see `executor::failed_cards`), so they are held and released together —
/// a card written late must not surface on its own while its source is
/// still held, or pairing would see it as unique to one side and copy it
/// back, duplicating the contact. A row with no UID (an unreadable card) is
/// its own single-row group. A row's own due-ness compares its stored ETag
/// with its href's current ETag in its side's listing; a row whose href is
/// no longer listed on that side counts as due.
fn held_hrefs(listed: &Listed, failures: &[CardFailure], now: DateTime<Utc>) -> HashSet<(Side, Href)> {
    let is_due = |failure: &CardFailure| {
        let current = listed.side(failure.side).etag(&failure.href);
        failure.is_due(now, current.as_ref())
    };
    let mut held = HashSet::new();
    for failure in failures {
        let group_due = match &failure.uid {
            Some(uid) => failures.iter().filter(|other| other.uid.as_ref() == Some(uid)).all(is_due),
            None => is_due(failure),
        };
        if !group_due {
            held.insert((failure.side, failure.href.clone()));
        }
    }
    held
}

/// One side's full membership: `changes_since(None)` with a fresh token, or
/// `list_etags` when the collection has no `sync-collection`.
pub(super) async fn list_side(book: &dyn AddressBook, collection: &Collection) -> Result<SideListing, Error> {
    if !collection.supports_sync_collection {
        return Ok(SideListing {
            entries: book.list_etags().await?,
            token: None,
        });
    }
    match book.changes_since(None).await? {
        Changes::Delta(set) => Ok(SideListing {
            entries: set.changed,
            token: Some(set.token),
        }),
        Changes::TokenInvalid => Err(AddressBookError::Permanent("server rejected a full listing without a token".to_owned()).into()),
    }
}
