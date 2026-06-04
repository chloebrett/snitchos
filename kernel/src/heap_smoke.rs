// SMOKE TEST — remove once real kernel workloads drive heap metrics.
// Holds the static FactorTable and exposes step()/stats() for the heartbeat
// loop. All logic lives in kernel_core::heap_smoke; this file only adds the
// lock and the static.

use kernel_core::heap_smoke::FactorTable;
use spin::Mutex;

/// Integers to factorize per heartbeat.
pub const BATCH: usize = 200;

/// Evict composite entries every this many heartbeats.
pub const EVICT_EVERY: i64 = 10;

static TABLE: Mutex<Option<FactorTable>> = Mutex::new(None);

pub struct Stats {
    pub entries: usize,
    pub primes: usize,
    pub candidate: u64,
}

pub fn step(heartbeat: i64) {
    let mut guard = TABLE.lock();
    let table = guard.get_or_insert_with(FactorTable::new);
    table.extend(BATCH);
    if heartbeat % EVICT_EVERY == 0 {
        table.evict_composites();
    }
}

pub fn stats() -> Stats {
    let guard = TABLE.lock();
    match guard.as_ref() {
        None => Stats { entries: 0, primes: 0, candidate: 0 },
        Some(t) => Stats {
            entries: t.entry_count(),
            primes: t.prime_count(),
            candidate: t.next_candidate(),
        },
    }
}
