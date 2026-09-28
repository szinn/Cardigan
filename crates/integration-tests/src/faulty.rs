//! Wraps the real adapter the SyncService uses: counts writes and injects
//! faults, so no test hook lives in production code.

use std::{
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use cg_core::{
    AddressBookError, Error,
    addressbook::{AddressBook, Changes, Collection, MultigetResult, Precondition, SyncToken},
    contact::{ETag, Href},
};

/// A server write the wrapper can fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Write {
    Put,
    Delete,
}

/// What happens once the real write has reached the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code, reason = "used by later scenarios")]
pub(crate) enum Fault {
    /// Panic: the process "dies" and nothing after the write runs.
    Crash,
    /// The server applied the write but the response was lost.
    LostResponse,
}

/// Work to run just before a PUT is sent (for example a user's edit that
/// makes the PUT's `If-Match` stale).
pub(crate) type Hook = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

pub(crate) struct FaultyBook {
    inner: Arc<dyn AddressBook>,
    puts: AtomicUsize,
    deletes: AtomicUsize,
    fault: Mutex<Option<(Write, Fault)>>,
    before_put: Mutex<Option<Hook>>,
}

impl FaultyBook {
    pub(crate) fn new(inner: Arc<dyn AddressBook>) -> Self {
        Self {
            inner,
            puts: AtomicUsize::new(0),
            deletes: AtomicUsize::new(0),
            fault: Mutex::new(None),
            before_put: Mutex::new(None),
        }
    }

    /// PUTs and DELETEs attempted since the last reset.
    pub(crate) fn writes(&self) -> usize {
        self.puts.load(Ordering::SeqCst) + self.deletes.load(Ordering::SeqCst)
    }

    pub(crate) fn reset_counts(&self) {
        self.puts.store(0, Ordering::SeqCst);
        self.deletes.store(0, Ordering::SeqCst);
    }

    /// The next `write` reaches the server, then `fault` happens.
    #[allow(dead_code, reason = "used by later scenarios")]
    pub(crate) fn fault_after_next(&self, write: Write, fault: Fault) {
        *self.fault.lock().expect("fault lock") = Some((write, fault));
    }

    /// Runs `hook` just before the next PUT is sent.
    #[allow(dead_code, reason = "used by later scenarios")]
    pub(crate) fn before_next_put(&self, hook: Hook) {
        *self.before_put.lock().expect("hook lock") = Some(hook);
    }

    fn after(&self, write: Write) -> Result<(), Error> {
        let fault = {
            let mut slot = self.fault.lock().expect("fault lock");
            match *slot {
                Some((armed, fault)) if armed == write => {
                    *slot = None;
                    Some(fault)
                }
                _ => None,
            }
        };
        match fault {
            Some(Fault::Crash) => panic!("simulated crash after a {write:?}"),
            Some(Fault::LostResponse) => Err(AddressBookError::Transient("simulated lost response".into()).into()),
            None => Ok(()),
        }
    }
}

#[async_trait::async_trait]
impl AddressBook for FaultyBook {
    async fn discover(&self) -> Result<Collection, Error> {
        self.inner.discover().await
    }

    async fn changes_since(&self, token: Option<&SyncToken>) -> Result<Changes, Error> {
        self.inner.changes_since(token).await
    }

    async fn list_etags(&self) -> Result<Vec<(Href, ETag)>, Error> {
        self.inner.list_etags().await
    }

    async fn multiget(&self, hrefs: &[Href]) -> Result<MultigetResult, Error> {
        self.inner.multiget(hrefs).await
    }

    async fn put(&self, href: &Href, body: &[u8], precondition: Precondition) -> Result<Option<ETag>, Error> {
        let hook = self.before_put.lock().expect("hook lock").take();
        if let Some(hook) = hook {
            hook().await;
        }
        self.puts.fetch_add(1, Ordering::SeqCst);
        let etag = self.inner.put(href, body, precondition).await?;
        self.after(Write::Put)?;
        Ok(etag)
    }

    async fn delete(&self, href: &Href, if_match: Option<&ETag>) -> Result<(), Error> {
        self.deletes.fetch_add(1, Ordering::SeqCst);
        self.inner.delete(href, if_match).await?;
        self.after(Write::Delete)
    }
}
