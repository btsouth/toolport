/**
 * What Toolport Teams costs, in one place.
 *
 * The Teams tab is the only place in the app that quotes a price, and a price that
 * disagrees with toolport.app/teams is worse than no price at all. These values mirror
 * the pricing section there (and `FREE_SEATS_LIMIT` in the toolport-teams server, which
 * is what actually enforces the free seat count). If you change one, change all three,
 * and check the live page before you do:
 *
 *   https://toolport.app/teams#pricing
 *
 * The app ships on a release cadence and Stripe does not, so treat everything here as a
 * summary with a link, never as the authority. `TEAMS_PRICING_URL` is the authority.
 *
 * Verified against the Teams pricing spec on 2026-10-06.
 */

/** People included on the Free plan before a plan is required. New hosted teams are
 * capped here; teams created before the 2026-10 pricing change keep their old limit of 5.
 * Enforced server-side as `FREE_SEATS_LIMIT`. */
export const TEAMS_FREE_SEATS = 1;

/** People included in the Team plan before per-person pricing applies. */
export const TEAMS_TEAM_SEATS = 10;

/** Monthly price of the Team plan, flat, covering `TEAMS_TEAM_SEATS` people. */
export const TEAMS_BASE_PRICE = 19;

/** Monthly price per person past `TEAMS_TEAM_SEATS` on the Team plan. */
export const TEAMS_SEAT_PRICE = 4;

/** Annual price per additional person: ten months of the monthly rate. */
export const TEAMS_ANNUAL_SEAT_PRICE = 40;

/** Annual price of the Team plan, which is two months off the monthly rate. Quoted in
 * `TEAMS_PAID_LINE` so this number has a render site: an exported constant nothing
 * displays is a number nothing can catch drifting. */
export const TEAMS_ANNUAL_PRICE = 190;

/** Length of the Team trial, in days. No card is taken for it. */
export const TEAMS_TRIAL_DAYS = 14;

/** The free tier, stated the way the pricing page states it. */
export const TEAMS_FREE_LINE = `Free: ${TEAMS_FREE_SEATS} person, 1 device. No card required.`;

/** The paid tier. Deliberately says what the money buys, because seats alone do not
 * explain it: the plan is a flat price for a whole team, not a per-seat charge. Quoting
 * only the per-person number would read as a seat paywall, which is not what the plan is. */
export const TEAMS_PAID_LINE = `Team is $${TEAMS_BASE_PRICE}/month (or $${TEAMS_ANNUAL_PRICE}/year) for your whole team, up to ${TEAMS_TEAM_SEATS} people, then $${TEAMS_SEAT_PRICE}/month per additional person (or $${TEAMS_ANNUAL_SEAT_PRICE}/year on annual billing), and adds access control, rate limits, and audit. Same price hosted or self-hosted.`;

export const PRO_MONTHLY_PRICE = 5;
export const PRO_ANNUAL_PRICE = 48;
export const PRO_LINE = `Pro is $${PRO_MONTHLY_PRICE}/month or $${PRO_ANNUAL_PRICE}/year for one person on unlimited devices. Try it free for 14 days, no card.`;
