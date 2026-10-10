//! What Toolport Teams costs, mirrored for the native shell.
//!
//! The authority is `src/lib/teamsPlan.ts`, which is itself a summary of
//! <https://toolport.app/teams#pricing> and of `FREE_SEATS_LIMIT` in the
//! toolport-teams server. Two shells quoting two different prices is worse than
//! either quoting none, so this file exists only so the GTK shell can say the
//! same sentence the React shell says, and
//! [`tests::the_rust_and_typescript_plan_numbers_agree`] fails the build if the
//! two ever drift apart. Change the TypeScript first, then this.

/// People included on the Free plan before a plan is required.
pub const FREE_SEATS: u32 = 1;
/// People included in the Team plan before per-person pricing applies.
pub const TEAM_SEATS: u32 = 10;
/// Monthly price of the Team plan, flat, covering [`TEAM_SEATS`] people.
pub const BASE_PRICE: u32 = 19;
/// Monthly price per person past [`TEAM_SEATS`].
pub const SEAT_PRICE: u32 = 4;
/// Annual price per additional person: ten months of the monthly rate.
pub const ANNUAL_SEAT_PRICE: u32 = 40;
/// Annual price of the Team plan.
pub const ANNUAL_PRICE: u32 = 190;
/// Length of the Team trial, in days. No card is taken for it.
pub const TRIAL_DAYS: u32 = 14;

/// The free tier, worded as the pricing page words it.
pub fn free_line() -> String {
    format!("Free: {FREE_SEATS} person, 1 device. No card required.")
}

pub fn pro_line() -> String {
    "Pro is $5/month or $48/year for one person on unlimited devices. Try it free for 14 days, no card.".into()
}

/// The paid tier. Says what the money buys, because seats alone do not explain
/// it: the plan is a flat price for a whole team, not a per-seat charge.
pub fn paid_line() -> String {
    format!(
        "Team is ${BASE_PRICE}/month (or ${ANNUAL_PRICE}/year) for your whole team, \
up to {TEAM_SEATS} people, then ${SEAT_PRICE}/month per additional person (or ${ANNUAL_SEAT_PRICE}/year on annual billing), and adds access control, rate limits, and audit. \
Same price hosted or self-hosted."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typescript_number(source: &str, name: &str) -> u32 {
        let needle = format!("export const {name} = ");
        let start = source
            .find(&needle)
            .unwrap_or_else(|| panic!("{name} is no longer declared in teamsPlan.ts"))
            + needle.len();
        source[start..]
            .split(';')
            .next()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or_else(|| panic!("{name} in teamsPlan.ts is not a plain number"))
    }

    /// The two shells must quote one price. This reads the TypeScript the React
    /// shell renders from, so changing one side without the other fails here
    /// rather than shipping two different claims to two sets of users.
    #[test]
    fn the_rust_and_typescript_plan_numbers_agree() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../src/lib/teamsPlan.ts");
        let source = std::fs::read_to_string(path).expect("teamsPlan.ts is readable");
        for (name, ours) in [
            ("TEAMS_FREE_SEATS", FREE_SEATS),
            ("TEAMS_TEAM_SEATS", TEAM_SEATS),
            ("TEAMS_BASE_PRICE", BASE_PRICE),
            ("TEAMS_SEAT_PRICE", SEAT_PRICE),
            ("TEAMS_ANNUAL_SEAT_PRICE", ANNUAL_SEAT_PRICE),
            ("TEAMS_ANNUAL_PRICE", ANNUAL_PRICE),
            ("TEAMS_TRIAL_DAYS", TRIAL_DAYS),
        ] {
            assert_eq!(
                typescript_number(&source, name),
                ours,
                "{name} disagrees between teamsPlan.ts and teams_plan.rs"
            );
        }
    }

    /// Resolve a TypeScript template literal by substituting the constants it
    /// interpolates, so the two shells can be compared on the finished sentence.
    fn typescript_line(source: &str, name: &str) -> String {
        let needle = format!("export const {name} = `");
        let start = source
            .find(&needle)
            .unwrap_or_else(|| panic!("{name} is no longer declared in teamsPlan.ts"))
            + needle.len();
        let raw = &source[start..start + source[start..].find('`').expect("unterminated template")];
        let mut out = raw.replace('\n', " ");
        for (token, value) in [
            ("${TEAMS_FREE_SEATS}", FREE_SEATS),
            ("${TEAMS_TEAM_SEATS}", TEAM_SEATS),
            ("${TEAMS_BASE_PRICE}", BASE_PRICE),
            ("${TEAMS_SEAT_PRICE}", SEAT_PRICE),
            ("${TEAMS_ANNUAL_SEAT_PRICE}", ANNUAL_SEAT_PRICE),
            ("${TEAMS_ANNUAL_PRICE}", ANNUAL_PRICE),
        ] {
            out = out.replace(token, &value.to_string());
        }
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The wording is checked too, not only the numbers: a user comparing the two
    /// shells should not see the same tier described two different ways.
    #[test]
    fn the_free_and_paid_lines_match_the_typescript_wording() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../src/lib/teamsPlan.ts");
        let source = std::fs::read_to_string(path).expect("teamsPlan.ts is readable");
        assert_eq!(typescript_line(&source, "TEAMS_FREE_LINE"), free_line());
        assert_eq!(typescript_line(&source, "TEAMS_PAID_LINE"), paid_line());
    }
}
