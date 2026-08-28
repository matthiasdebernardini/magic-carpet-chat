//! The mock's demo data, flattened.
//!
//! The design page computes these rows in JavaScript from two seed accounts.
//! Nothing here talks to a relay or a wallet — the Dashboard is a picture of a
//! working instance, so the numbers are the mock's numbers.

use crate::palette::*;

pub struct Account {
    pub initials: &'static str,
    pub name: &'static str,
    pub npub: &'static str,
    pub hue: (u32, u32),
}

pub const ACCOUNTS: [Account; 2] = [
    Account {
        initials: "AK",
        name: "Personal",
        npub: "npub1m4gk7w2…qx3f",
        hue: (HUE_AK_FROM, HUE_AK_TO),
    },
    Account {
        initials: "BO",
        name: "Brainstorm ops",
        npub: "npub1q7rf3k9…u2wd",
        hue: (HUE_BO_FROM, HUE_BO_TO),
    },
];

/// The active account, whose figures fill the sidebar's three stat cards.
pub const ACTIVE: usize = 0;

pub const SUMMARY: &str = "2 accounts · 4 tasks · watching magic-carpet.brainstorm.world";

pub const EARNED: &str = "188,000";
pub const EARNED_NOTE: &str = "14 bounties claimed";
pub const SPENT: &str = "96,400";
pub const SPENT_NOTE: &str = "34 bounties paid";
pub const BALANCE: &str = "128,400";

/// A row of "Needs attention". `account` indexes [`ACCOUNTS`].
pub struct Task {
    pub dot: u32,
    pub title: &'static str,
    pub description: &'static str,
    pub account: usize,
    /// A task nobody can act on yet carries a "planned" tag instead of a badge.
    pub planned: bool,
    pub fixable: bool,
}

pub const TASKS: [Task; 4] = [
    Task {
        dot: TEXT_DIM,
        title: "Associate agent with owner",
        description: "Link this payer to its owning npub.",
        account: 0,
        planned: true,
        fixable: false,
    },
    Task {
        dot: RED,
        title: "Top up your wallet",
        description: "Balance 12,930 sats is below the 25,000 sats of open bounties it backs.",
        account: 1,
        planned: false,
        fixable: true,
    },
    Task {
        dot: AMBER,
        title: "Publish your Treasure Map",
        description: "No kind-10040 event found — other apps can't locate your rank provider.",
        account: 1,
        planned: false,
        fixable: true,
    },
    Task {
        dot: AMBER,
        title: "Gain a follower",
        description: "You need at least one verified follower for a trust score.",
        account: 1,
        planned: false,
        fixable: true,
    },
];

pub struct Event {
    pub dot: u32,
    pub text: &'static str,
    pub account: usize,
    pub when: &'static str,
}

pub const ACTIVITY: [Event; 9] = [
    Event {
        dot: GREEN,
        text: "Paid 5,000 sats to 7f3a2b9c1e4d… for \"done — PR #14\" on the NIP-05 walkthrough bounty",
        account: 0,
        when: "today 14:22",
    },
    Event {
        dot: ACCENT,
        text: "Paying 12,000 sats to 4e9b7c2a8d1f… on the relay-benchmark bounty",
        account: 0,
        when: "today 14:07",
    },
    Event {
        dot: RED,
        text: "Refused a claim from c2d8e14f6a9b… — rank 1, below the bounty's minimum of 3",
        account: 0,
        when: "today 11:48",
    },
    Event {
        dot: ACCENT_LIGHT,
        text: "Submitted a 12,000-sat bounty on Protocol docs & guides",
        account: 0,
        when: "today 09:15",
    },
    Event {
        dot: AMBER,
        text: "Received 40,000 sats from npub1zk8w4t2… for your claim on the web-of-trust visualizer",
        account: 0,
        when: "Aug 19",
    },
    Event {
        dot: TEXT_MUTED,
        text: "Created the list Brand & design",
        account: 0,
        when: "Aug 12",
    },
    Event {
        dot: HUE_BO_FROM,
        text: "Wrote your lightning address to your Nostr profile (lud16)",
        account: 0,
        when: "Aug 11",
    },
    Event {
        dot: ACCENT_LIGHT,
        text: "Submitted a 25,000-sat bounty on Docs maintenance",
        account: 1,
        when: "5 days ago",
    },
    Event {
        dot: HUE_BO_FROM,
        text: "Connected an Alby Hub wallet over NWC",
        account: 1,
        when: "6 days ago",
    },
];
