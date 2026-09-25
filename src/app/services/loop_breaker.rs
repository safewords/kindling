//! Noticing that a machine is chainloading the same thing over and over.
//!
//! The classic PXE failure: the server hands a machine iPXE, iPXE comes back
//! and is not recognised, so the server hands it iPXE again. The machine loops
//! until somebody walks over to it, and nothing in any log says so — every
//! individual exchange looks perfectly correct.
//!
//! Recognising iPXE is handled two ways already (option 77 and option 175),
//! and that is where the fix belongs. This is the backstop for the case those
//! miss, and its value is as much diagnostic as corrective: a loop that is
//! *named* in the boot log is a five-minute problem rather than an afternoon.
//!
//! ## Telling a loop from a retry
//!
//! The discriminator is the DHCP transaction id. Firmware retransmitting a
//! `DISCOVER` it thinks was lost reuses its `xid`; a machine that has booted
//! an NBP and come back around starts a *new* transaction with a new one. So
//! counting distinct transaction ids for the same boot file separates "this
//! network is dropping packets" from "this machine is going in circles",
//! which a naive request count cannot.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::pxe::mac::MacAddr;

/// What the breaker thinks of an offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopState {
    /// Nothing unusual.
    Fine,
    /// This machine has been round the same loop this many times.
    Looping { transactions: usize },
}

impl LoopState {
    pub fn is_looping(&self) -> bool {
        matches!(self, LoopState::Looping { .. })
    }
}

#[derive(Debug)]
struct Attempts {
    /// The boot file these transactions were for. A different file means the
    /// policy changed its mind, which is not a loop.
    file: String,
    /// One entry per distinct DHCP transaction, with when it was seen.
    transactions: Vec<(Instant, u32)>,
    /// Set once the machine reaches the script stage, which is proof it got
    /// somewhere. Until it is cleared, this machine is not looping.
    progressed: bool,
}

#[derive(Debug)]
pub struct LoopBreaker {
    window: Duration,
    threshold: usize,
    /// The most machines tracked at once. A boot server on a large network
    /// sees thousands, and this is a diagnostic aid rather than a record: the
    /// bound is what stops it from being a slow memory leak.
    capacity: usize,
    seen: Mutex<HashMap<MacAddr, Attempts>>,
}

impl Default for LoopBreaker {
    fn default() -> Self {
        // Four full transactions inside ninety seconds. A loop iteration is
        // DHCP, a TFTP fetch and an iPXE start — five to ten seconds — so four
        // of them is comfortably inside the window, while a machine that is
        // merely slow never gets there.
        Self::new(Duration::from_secs(90), 4, 4096)
    }
}

impl LoopBreaker {
    pub fn new(window: Duration, threshold: usize, capacity: usize) -> Self {
        Self { window, threshold: threshold.max(2), capacity, seen: Mutex::new(HashMap::new()) }
    }

    pub fn window(&self) -> Duration {
        self.window
    }

    pub fn threshold(&self) -> usize {
        self.threshold
    }

    /// Record an offer, and say whether this machine is going in circles.
    pub fn offer(&self, mac: MacAddr, file: &str, xid: u32, now: Instant) -> LoopState {
        let mut seen = self.lock();

        // Cheap enough to do on the way in, and it means the map never holds
        // a machine that stopped booting an hour ago.
        let window = self.window;
        seen.retain(|_, attempts| {
            attempts.transactions.iter().any(|(at, _)| now.duration_since(*at) < window)
        });

        if seen.len() >= self.capacity && !seen.contains_key(&mac) {
            // Full of machines that are all still active. Tracking is the
            // optional part; answering the machine is not.
            return LoopState::Fine;
        }

        let attempts = seen.entry(mac).or_insert_with(|| Attempts {
            file: file.to_string(),
            transactions: Vec::new(),
            progressed: false,
        });

        // A different file means the decision changed — a pin, a reload, a
        // rule that started matching. Whatever the machine was doing before,
        // it is not the same circle.
        if attempts.file != file {
            attempts.file = file.to_string();
            attempts.transactions.clear();
            attempts.progressed = false;
        }

        attempts.transactions.retain(|(at, _)| now.duration_since(*at) < window);

        // Retransmissions of one `DISCOVER` share an xid. Only a new
        // transaction is another lap.
        if !attempts.transactions.iter().any(|(_, seen)| *seen == xid) {
            attempts.transactions.push((now, xid));
        }

        if attempts.progressed || attempts.transactions.len() < self.threshold {
            LoopState::Fine
        } else {
            LoopState::Looping { transactions: attempts.transactions.len() }
        }
    }

    /// Record that a machine reached the script stage.
    ///
    /// Proof it got somewhere, so whatever it was doing before was not a loop.
    pub fn progressed(&self, mac: MacAddr) {
        let mut seen = self.lock();
        if let Some(attempts) = seen.get_mut(&mac) {
            attempts.progressed = true;
            attempts.transactions.clear();
        }
    }

    /// Stop tracking a machine — after an operator has changed something for
    /// it, so the next boot is judged on its own.
    pub fn forget(&self, mac: MacAddr) {
        self.lock().remove(&mac);
    }

    /// How many machines are being watched, for the health endpoint.
    pub fn tracked(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<MacAddr, Attempts>> {
        // A panic while holding this must not stop the server answering
        // machines for ever after. The data behind it is a diagnostic cache,
        // so recovering it is safe in a way that recovering a half-written
        // invariant would not be.
        self.seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mac(text: &str) -> MacAddr {
        text.parse().unwrap()
    }

    fn breaker() -> LoopBreaker {
        LoopBreaker::new(Duration::from_secs(90), 4, 4096)
    }

    #[test]
    fn a_machine_going_round_the_same_loop_is_noticed() {
        let breaker = breaker();
        let machine = mac("18:66:da:11:22:33");
        let start = Instant::now();

        for lap in 0..3 {
            let at = start + Duration::from_secs(lap * 7);
            assert_eq!(
                breaker.offer(machine, "ipxe.efi", 1000 + lap as u32, at),
                LoopState::Fine,
                "lap {lap} is not yet enough to be sure"
            );
        }

        let fourth = breaker.offer(machine, "ipxe.efi", 1003, start + Duration::from_secs(28));
        assert_eq!(fourth, LoopState::Looping { transactions: 4 });
        assert!(fourth.is_looping());
    }

    #[test]
    fn retransmitting_one_request_is_not_a_loop_however_many_times_it_happens() {
        // The distinction the whole module turns on. A lossy network makes
        // firmware repeat its `DISCOVER`, and treating that as a loop would
        // mean breaking a boot that was merely slow.
        let breaker = breaker();
        let machine = mac("18:66:da:11:22:33");
        let start = Instant::now();

        for attempt in 0..12 {
            let at = start + Duration::from_secs(attempt);
            assert_eq!(
                breaker.offer(machine, "ipxe.efi", 0xabcd, at),
                LoopState::Fine,
                "the same transaction, retried: attempt {attempt}"
            );
        }
    }

    #[test]
    fn laps_that_are_far_enough_apart_are_not_a_loop() {
        // Four boots across an afternoon is a machine being used, not one
        // going in circles.
        let breaker = breaker();
        let machine = mac("18:66:da:11:22:33");
        let start = Instant::now();

        for lap in 0..8 {
            let at = start + Duration::from_secs(lap * 120);
            assert_eq!(
                breaker.offer(machine, "ipxe.efi", lap as u32, at),
                LoopState::Fine,
                "lap {lap} fell outside the window"
            );
        }
    }

    #[test]
    fn reaching_the_script_proves_the_machine_is_not_looping() {
        let breaker = breaker();
        let machine = mac("18:66:da:11:22:33");
        let start = Instant::now();

        for lap in 0..3 {
            breaker.offer(machine, "ipxe.efi", lap, start + Duration::from_secs(lap as u64 * 5));
        }
        breaker.progressed(machine);

        assert_eq!(
            breaker.offer(machine, "ipxe.efi", 99, start + Duration::from_secs(20)),
            LoopState::Fine,
            "it got somewhere, so the count starts again"
        );
    }

    #[test]
    fn a_change_of_boot_file_starts_the_count_again() {
        // A pin, a reload or a rule that started matching. Whatever the
        // machine was doing, it is not the same circle any more.
        let breaker = breaker();
        let machine = mac("18:66:da:11:22:33");
        let start = Instant::now();

        for lap in 0..3 {
            breaker.offer(machine, "ipxe.efi", lap, start + Duration::from_secs(lap as u64 * 5));
        }

        assert_eq!(
            breaker.offer(machine, "pxelinux.0", 50, start + Duration::from_secs(20)),
            LoopState::Fine
        );
    }

    #[test]
    fn two_machines_are_counted_separately() {
        let breaker = breaker();
        let start = Instant::now();
        let looping = mac("18:66:da:11:22:33");
        let fine = mac("52:54:00:11:22:33");

        for lap in 0..4 {
            let at = start + Duration::from_secs(lap as u64 * 5);
            breaker.offer(looping, "ipxe.efi", lap, at);
            assert_eq!(breaker.offer(fine, "ipxe.efi", 7, at), LoopState::Fine);
        }

        assert!(breaker
            .offer(looping, "ipxe.efi", 100, start + Duration::from_secs(25))
            .is_looping());
    }

    #[test]
    fn a_machine_that_stopped_booting_is_forgotten() {
        // Otherwise this is a map that only ever grows.
        let breaker = breaker();
        let start = Instant::now();

        breaker.offer(mac("18:66:da:11:22:33"), "ipxe.efi", 1, start);
        assert_eq!(breaker.tracked(), 1);

        breaker.offer(mac("52:54:00:11:22:33"), "ipxe.efi", 1, start + Duration::from_secs(600));
        assert_eq!(breaker.tracked(), 1, "the first machine aged out");
    }

    #[test]
    fn tracking_stops_at_the_cap_rather_than_growing_without_bound() {
        let breaker = LoopBreaker::new(Duration::from_secs(90), 4, 3);
        let now = Instant::now();

        for index in 0..10u8 {
            let machine = MacAddr::new([0x02, 0, 0, 0, 0, index]);
            // Answering is never blocked by the cache being full.
            assert_eq!(breaker.offer(machine, "ipxe.efi", 1, now), LoopState::Fine);
        }

        assert!(breaker.tracked() <= 3, "tracked {}", breaker.tracked());
    }

    #[test]
    fn a_threshold_below_two_is_raised_because_one_offer_is_never_a_loop() {
        let breaker = LoopBreaker::new(Duration::from_secs(90), 0, 16);
        assert_eq!(breaker.threshold(), 2);

        let machine = mac("18:66:da:11:22:33");
        let now = Instant::now();
        assert_eq!(breaker.offer(machine, "ipxe.efi", 1, now), LoopState::Fine);
        assert!(breaker.offer(machine, "ipxe.efi", 2, now).is_looping());
    }

    #[test]
    fn forgetting_a_machine_clears_what_was_counted_for_it() {
        let breaker = breaker();
        let machine = mac("18:66:da:11:22:33");
        let start = Instant::now();

        for lap in 0..4 {
            breaker.offer(machine, "ipxe.efi", lap, start + Duration::from_secs(lap as u64));
        }
        breaker.forget(machine);

        assert_eq!(
            breaker.offer(machine, "ipxe.efi", 90, start + Duration::from_secs(5)),
            LoopState::Fine
        );
    }
}
