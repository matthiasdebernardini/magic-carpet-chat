# Feature 1 walkthrough: Treasure Map status on the Dashboard and the Trust tab.
#
# Runs on the magic-carpet-cua box through agent.py, app pointed at the
# production instance. Two stored accounts: the house key (no Treasure Map,
# no Brainstorm account) and the claimant (Located map, provider 43e3580c…).
# Every candidate is a complete action; Jev picks one id per step and judges
# the postcondition from the OCR text. `click_text` locates a phrase by OCR.
from agent import Step, Cand

FOCUS = ("focus", "Magic Carpet")

FIX_FIRST = Cand(
    "fix-first-map-row",
    "Click the 'Fix' button on the first 'Publish your Treasure Map' row of the Needs attention panel.",
    [FOCUS, ("click_text", {"text": "Fix", "near": "Publish your Treasure Map"})],
)
CHECK_AGAIN = Cand(
    "check-again",
    "Click the 'Check again' button under the Treasure Map card.",
    [FOCUS, ("click_text", "Check again")],
)
ALL_ACCOUNTS = Cand(
    "back-to-all-accounts",
    "Click the '← All accounts' link at the top of the Account page.",
    [FOCUS, ("click_text", "All accounts")],
)
MANAGE_CLAIMANT = Cand(
    "manage-claimant",
    "Click the 'Manage' button on the account card whose npub starts with npub1wdn6rg (the active account).",
    [FOCUS, ("click_text", {"text": "Manage", "near": "carpetv3eScvye"})],
)
VIEW_RAW = Cand(
    "view-raw-event",
    "Click the 'View raw event' toggle under the Treasure Map facts.",
    [FOCUS, ("click_text", "View raw event")],
)
SCROLL_DOWN = Cand(
    "scroll-to-bottom",
    "Scroll the Account page down to its end so the Web of trust card is on screen.",
    [FOCUS, ("scroll", "down")],
)
PROGRESS_TAB = Cand(
    "open-progress-tab",
    "Click the 'Progress' tab in the tab strip.",
    [FOCUS, ("scroll", "up"), ("click_text", "Progress")],
)
DASHBOARD = Cand(
    "back-to-dashboard",
    "Press super+1 to return to the Dashboard.",
    [FOCUS, ("key", "super+1")],
)

STEPS = [
    Step(
        "Wait for the Dashboard's Needs attention panel to finish its relay reads and show the Treasure Map row.",
        [],
        "The Dashboard shows a 'Needs attention' panel with a row titled 'Publish your Treasure Map' and a 'Fix' button, and no word 'Checking'.",
        tries=6, settle=6,
        cues=("Needs attention", "Publish your Treasure Map", "Fix", "Recent activity"),
    ),
    Step(
        "Open the Trust tab of the account that needs a Treasure Map.",
        [FIX_FIRST],
        "An Account page is on screen: a tab strip with 'Identity & keys', 'Trust', 'Progress', 'Wallet', 'Limits & defaults'; a 'Treasure Map' card with a 'Not located' pill and a note that the kind-10040 Treasure Map could not be found and to go to Brainstorm; an 'Open Brainstorm' link and a 'Check again' button.",
        settle=4,
        cues=("Treasure Map", "Not located", "could not be found", "Open Brainstorm", "Check again", "Trusted Assertions"),
    ),
    Step(
        "Re-check the Treasure Map from the relays.",
        [CHECK_AGAIN],
        "The Trust tab is still on screen and the Treasure Map card still reads 'Not located' after the re-check, with no 'Checking' text left.",
        settle=15,
        cues=("Not located", "Check again", "Checking"),
    ),
    Step(
        "Go back to the list of accounts.",
        [ALL_ACCOUNTS],
        "The Accounts screen is on screen: the line 'Each account is its own nsec, wallet, limits, and payment log', an 'Add an account' button, and two account cards ('Magic Carpet House' and 'New account'), each with an npub and a 'Manage' button; one also has a 'Switch' button.",
        settle=3,
        cues=("Each account is its own nsec", "Add an account", "Magic Carpet House", "Switch", "Manage"),
    ),
    Step(
        "Open the Trust tab of the account that already has a Treasure Map.",
        [MANAGE_CLAIMANT],
        "The Trust tab of the account 'New account' shows a Treasure Map card whose pill reads 'Located' (not 'Not located'), a 'Published' date, relay URLs such as wss://nos.lol, the entry '30382:rank', a long hex key starting 43e3580c, 'wss://scores.brainstorm.world', the text 'matches your assigned key' and a 'View raw event' toggle. Dim row labels may be missing from the OCR; judge by the values.",
        settle=12,
        cues=("Located", "Published", "wss://nos.lol", "30382:rank", "43e3580c", "scores.brainstorm.world", "matches your assigned key", "View raw event"),
    ),
    Step(
        "Show the signed kind-10040 event the app verified.",
        [VIEW_RAW],
        "A JSON block is visible under the Treasure Map facts: the toggle now reads 'Hide raw event' and the text contains JSON keys such as \"kind\", \"tags\", \"pubkey\", \"created_at\" or \"sig\", with 10040 among the values. Small monospace text may OCR imperfectly; two or more of those keys is enough.",
        settle=3,
        cues=("Hide raw event", "kind", "tags", "created_at", "sig", "10040"),
    ),
    Step(
        "Scroll down through Trusted Assertions, Minimum rank and Trust Determination to the Web of trust card.",
        [SCROLL_DOWN],
        "The page has scrolled so the 'Web of trust' card is on screen, with 'Following' and 'Verified followers' tiles (numbers under them). The 'Trust Determination' or 'Minimum rank' cards may also be visible above it. Small link text may be missing from the OCR.",
        settle=3,
        cues=("Web of trust", "Following", "Verified followers", "Trust Determination", "Minimum rank"),
    ),
    Step(
        "Show that the other tabs exist but are not built yet.",
        [PROGRESS_TAB],
        "The 'Progress' tab is selected and the page body reads 'Coming soon.'",
        settle=3,
        cues=("Progress", "Coming soon"),
    ),
    Step(
        "Return to the Dashboard.",
        [DASHBOARD],
        "The Dashboard is on screen again with its 'Needs attention' panel.",
        settle=3,
        cues=("Dashboard", "Needs attention", "Recent activity"),
    ),
]
