//! Shared `ActiveConfig<T>` with pending + RAII binding.
//!
//! This module implements the cfg-ownership model described in ADR-0008. A
//! single application-scope `static ActiveConfig<T>` holds the *active* cfg
//! (what consumer tasks see when they run a session) and an optional *pending*
//! cfg (what the orchestrator wants applied next). Promotion `pending →
//! active` happens automatically when the refcount of in-use bindings drops
//! to zero.
//!
//! The orchestrator publishes via [`set_pending`] and reads via
//! [`with_active`]; consumer tasks bind via [`acquire`] and release on
//! [`ConfigInUse`] drop.
//!
//! This pattern is intended to give he orchestrator a view of the active
//! cfg, without races while config consumer task(s) are spawning or not running.
//! Consumer hold an RAII receipt that blocks promotion of the pending config while it's bound.
//!
//! [`set_pending`]: ActiveConfig::set_pending
//! [`with_active`]: ActiveConfig::with_active
//! [`acquire`]: ActiveConfig::acquire

use core::cell::RefCell;

use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex, Mutex};

/// Application-scope holder of an active + pending cfg, with refcounted
/// binding semantics.
///
/// Designed to live as a `static` per app. Consumer tasks call [`acquire`]
/// to mark the active cfg as in use; the returned [`ConfigInUse`] is an
/// RAII binding. The orchestrator calls [`set_pending`] to queue a new cfg;
/// promotion happens automatically when the in-use count drops to zero.
///
/// [`acquire`]: ActiveConfig::acquire
/// [`set_pending`]: ActiveConfig::set_pending
pub struct ActiveConfig<T> {
    state: Mutex<CriticalSectionRawMutex, RefCell<State<T>>>,
}

struct State<T> {
    /// The cfg every active [`ConfigInUse`] is currently bound to. This is
    /// what `measuring_config_hash` should report.
    active: Option<T>,

    /// The orchestrator's queued cfg waiting to be applied.
    /// `pending_set` distinguishes "no pending update" from "pending
    /// = None" (ClearConfig): `set_pending(None)` should be honoured even
    /// when `active` is already `None`.
    pending: Option<T>,
    pending_set: bool,

    /// Number of live [`ConfigInUse`] references to `active`. Promotion
    /// (`active = pending.take()`) only runs when this is zero.
    in_use_count: usize,
}

/// RAII handle: "this task is currently bound to the active cfg."
///
/// Holding this pins the active cfg and blocks `pending → active`
/// promotion. **Hold it for the entire scope where any cfg-derived state
/// (filter coefficients, hardware-binding) is in use** — that includes
/// state that was copied out of the cfg into local pipelines, not just
/// uses of the cfg variable itself. The intent is "I am bound to this
/// version of the cfg until I drop you."
///
/// Cloning this bumps the refcount; each clone's Drop decrements
/// independently. Use [`Clone`] to hand the same binding to multiple
/// consumer tasks.
#[must_use = "Dropping `ConfigInUse` releases the binding on the active \
              cfg and allows a pending cfg to be promoted; hold it for the \
              entire scope where any cfg-derived state is in use"]
pub struct ConfigInUse<'a, T> {
    holder: &'a ActiveConfig<T>,
    cfg: T,
}

impl<T> ActiveConfig<T> {
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(RefCell::new(State {
                active: None,
                pending: None,
                pending_set: false,
                in_use_count: 0,
            })),
        }
    }

    /// Mark the active cfg as in use and return a binding carrying a clone
    /// of it. Returns `None` if there is no active cfg.
    ///
    /// The returned [`ConfigInUse`] can be cloned to hand the same active
    /// cfg to multiple tasks; each clone independently increments and
    /// decrements the in-use count.
    pub fn acquire(&self) -> Option<ConfigInUse<'_, T>>
    where
        T: Clone,
    {
        self.state.lock(|cell| {
            let mut state = cell.borrow_mut();
            let cfg = state.active.as_ref()?.clone();
            state.in_use_count += 1;
            Some(ConfigInUse { holder: self, cfg })
        })
    }

    /// Queue a new cfg (or `None` to clear). Promotes immediately if the
    /// in-use count is zero; otherwise sits as pending until the last
    /// `ConfigInUse` drops.
    pub fn set_pending(&self, cfg: Option<T>) {
        self.state.lock(|cell| {
            let mut state = cell.borrow_mut();
            state.pending = cfg;
            state.pending_set = true;
            Self::try_promote(&mut state);
        });
    }

    /// Read the active cfg under the lock without cloning. Pass a closure;
    /// the closure receives `Some(&T)` if there is an active cfg, `None`
    /// otherwise.
    pub fn with_active<R>(&self, f: impl FnOnce(Option<&T>) -> R) -> R {
        self.state.lock(|cell| f(cell.borrow().active.as_ref()))
    }

    /// True if a pending cfg is queued (an update was published but the
    /// active hasn't been promoted because a `ConfigInUse` is still alive).
    pub fn has_pending(&self) -> bool {
        self.state.lock(|cell| cell.borrow().pending_set)
    }

    fn try_promote(state: &mut State<T>) {
        if state.in_use_count == 0 && state.pending_set {
            state.active = state.pending.take();
            state.pending_set = false;
        }
    }
}

impl<T> ConfigInUse<'_, T> {
    /// Borrow the cfg this binding is holding. The borrow's lifetime ties
    /// any derived `&T` use to the binding's lifetime — the borrow checker
    /// will refuse to drop the binding while the borrow is live.
    pub fn cfg(&self) -> &T {
        &self.cfg
    }
}

impl<T> core::fmt::Debug for ActiveConfig<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Reads only the counters under the lock — keeps Debug useful for
        // diagnostics without requiring `T: Debug` (which would force tests
        // to derive Debug on cfg types that don't otherwise need it).
        let (in_use, has_active, has_pending) = self.state.lock(|cell| {
            let s = cell.borrow();
            (s.in_use_count, s.active.is_some(), s.pending_set)
        });
        f.debug_struct("ActiveConfig")
            .field("in_use", &in_use)
            .field("has_active", &has_active)
            .field("has_pending", &has_pending)
            .finish()
    }
}

impl<T> core::fmt::Debug for ConfigInUse<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ConfigInUse").finish_non_exhaustive()
    }
}

impl<T: Clone> Clone for ConfigInUse<'_, T> {
    /// Cloning a binding bumps the refcount: the same active cfg is now
    /// pinned by one more consumer. Each clone's [`Drop`] decrements
    /// independently.
    fn clone(&self) -> Self {
        self.holder.state.lock(|cell| {
            cell.borrow_mut().in_use_count += 1;
        });
        Self {
            holder: self.holder,
            cfg: self.cfg.clone(),
        }
    }
}

impl<T> Drop for ConfigInUse<'_, T> {
    fn drop(&mut self) {
        self.holder.state.lock(|cell| {
            let mut state = cell.borrow_mut();
            state.in_use_count -= 1;
            ActiveConfig::try_promote(&mut state);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simple test cfg type that satisfies `Clone` + `PartialEq` so we can
    /// observe behaviour. The `tag` is used to identify versions in tests.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Cfg {
        tag: u8,
    }

    fn hash_of(cfg: &Cfg) -> u64 {
        cfg.tag as u64
    }

    fn active_hash(holder: &ActiveConfig<Cfg>) -> u64 {
        holder.with_active(|c| c.map(hash_of).unwrap_or(0))
    }

    #[test]
    fn empty_compute_returns_none() {
        let holder = ActiveConfig::<Cfg>::new();
        assert_eq!(active_hash(&holder), 0);
        assert!(holder.acquire().is_none());
    }

    #[test]
    fn set_pending_with_no_consumer_promotes_immediately() {
        let holder = ActiveConfig::<Cfg>::new();
        holder.set_pending(Some(Cfg { tag: 1 }));
        assert_eq!(active_hash(&holder), 1);
    }

    #[test]
    fn acquire_then_set_pending_blocks_promotion() {
        let holder = ActiveConfig::<Cfg>::new();
        holder.set_pending(Some(Cfg { tag: 1 }));

        let binding = holder.acquire().expect("active set");
        assert_eq!(binding.cfg().tag, 1);

        // Pending update arrives — promotion blocked.
        holder.set_pending(Some(Cfg { tag: 2 }));
        assert_eq!(active_hash(&holder), 1);

        // Releasing the binding lets pending promote.
        drop(binding);
        assert_eq!(active_hash(&holder), 2);
    }

    #[test]
    fn two_acquires_both_must_drop_before_promotion() {
        let holder = ActiveConfig::<Cfg>::new();
        holder.set_pending(Some(Cfg { tag: 1 }));

        let a = holder.acquire().expect("active set");
        let b = holder.acquire().expect("active set");

        holder.set_pending(Some(Cfg { tag: 2 }));
        assert_eq!(active_hash(&holder), 1);

        drop(a);
        assert_eq!(active_hash(&holder), 1, "still blocked by b");

        drop(b);
        assert_eq!(active_hash(&holder), 2, "promoted after last drop");
    }

    #[test]
    fn clone_bumps_refcount() {
        let holder = ActiveConfig::<Cfg>::new();
        holder.set_pending(Some(Cfg { tag: 1 }));

        let a = holder.acquire().expect("active set");
        let b = a.clone();

        holder.set_pending(Some(Cfg { tag: 2 }));
        assert_eq!(active_hash(&holder), 1);

        drop(a);
        assert_eq!(active_hash(&holder), 1, "still blocked by b (the clone)");

        drop(b);
        assert_eq!(active_hash(&holder), 2);
    }

    #[test]
    fn clear_via_set_pending_none() {
        let holder = ActiveConfig::<Cfg>::new();
        holder.set_pending(Some(Cfg { tag: 1 }));
        let binding = holder.acquire().expect("active set");

        // ClearConfig path: set_pending(None). pending_set distinguishes
        // this from "no pending update."
        holder.set_pending(None);
        assert_eq!(active_hash(&holder), 1, "blocked while binding alive");

        drop(binding);
        assert_eq!(active_hash(&holder), 0, "active cleared after drop");
        assert!(holder.acquire().is_none());
    }

    #[test]
    fn pending_keeps_latest_when_replaced_before_promote() {
        let holder = ActiveConfig::<Cfg>::new();
        holder.set_pending(Some(Cfg { tag: 1 }));
        let binding = holder.acquire().expect("active set");

        holder.set_pending(Some(Cfg { tag: 2 }));
        holder.set_pending(Some(Cfg { tag: 3 }));
        holder.set_pending(Some(Cfg { tag: 4 }));
        assert_eq!(active_hash(&holder), 1);

        drop(binding);
        assert_eq!(active_hash(&holder), 4, "latest pending wins on promotion");
    }

    #[test]
    fn acquire_returns_none_when_active_cleared() {
        let holder = ActiveConfig::<Cfg>::new();
        holder.set_pending(Some(Cfg { tag: 1 }));
        assert!(holder.acquire().is_some());

        // Now clear it (no consumer holding).
        holder.set_pending(None);
        assert!(holder.acquire().is_none());
    }

    #[test]
    fn has_pending_reflects_queued_update() {
        let holder = ActiveConfig::<Cfg>::new();
        assert!(!holder.has_pending());

        // No consumer: set_pending promotes immediately → no pending left.
        holder.set_pending(Some(Cfg { tag: 1 }));
        assert!(!holder.has_pending());

        // Consumer holds binding; next set_pending stays queued.
        let binding = holder.acquire().expect("active set");
        holder.set_pending(Some(Cfg { tag: 2 }));
        assert!(holder.has_pending());

        drop(binding);
        assert!(!holder.has_pending(), "promoted after binding drop");
    }

    #[test]
    fn cfg_borrow_returns_the_active_at_acquire_time() {
        let holder = ActiveConfig::<Cfg>::new();
        holder.set_pending(Some(Cfg { tag: 1 }));
        let binding = holder.acquire().expect("active set");
        // Bumping pending doesn't change what the binding sees — the
        // binding carries a clone made at acquire time.
        holder.set_pending(Some(Cfg { tag: 2 }));
        assert_eq!(binding.cfg().tag, 1);
    }
}
