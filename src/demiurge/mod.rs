//! # Demiurge — the contribution-accounting core
//!
//! Pure, dependency-free arithmetic for the Demiurge unit, ported from the
//! canonical reference (`linexus-core::vicinagora`) so the Nexus owns the
//! load-bearing formulas directly. The DB-backed models in
//! [`crate::models`] persist the *lots* and *ledger*; this module only knows
//! how to mint, decay, and convert.
//!
//! The three laws are encoded structurally:
//! 1. **Only breath mints** — [`mint_amount`] is the *only* way Demiurge comes
//!    into existence, and it is driven exclusively by human contribution
//!    minutes. There is no institutional mint path.
//! 2. **The floor is never priced** — no function here reads or returns a floor
//!    guarantee; the floor lives outside the ledger entirely.
//! 3. **Everything decays** — every lot carries [`DEMIURGE_EXPIRY_SECS`] and
//!    expires on schedule; see [`is_expired`].

use serde::{Deserialize, Serialize};

/// Basis-point base. 10,000 bps == 1.0×.
pub const ONE_BPS: i64 = 10_000;

/// Seconds in a Gregorian year (365.2425 days), per the white paper.
pub const SECONDS_PER_YEAR: i64 = 31_556_952;

/// A minted lot expires exactly twenty Gregorian years after its mint instant.
pub const DEMIURGE_EXPIRY_SECS: i64 = 20 * SECONDS_PER_YEAR;

/// Default weekly contribution expectation, in hours. Governance-tunable.
pub const WEEKLY_EXPECTATION_HOURS: i64 = 20;

/// Kinds of contribution the ledger recognizes. Each mints at a base rate
/// expressed in Demiurge per hour. `ContentCreation` never mints per-hour — it
/// is funded by sponsorship and attestation, never by views.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContributionKind {
    Labor,
    Education,
    Mentorship,
    Care,
    EmergencyResponse,
    Rehabilitation,
    ContentCreation,
}

impl ContributionKind {
    /// Parse a wire string into a kind. Unknown values fall back to `Labor`.
    #[must_use]
    pub fn from_str_lenient(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "education" => Self::Education,
            "mentorship" => Self::Mentorship,
            "care" => Self::Care,
            "emergency_response" | "emergency" => Self::EmergencyResponse,
            "rehabilitation" | "rehab" => Self::Rehabilitation,
            "content_creation" | "content" => Self::ContentCreation,
            _ => Self::Labor,
        }
    }

    /// Default base rate in Demiurge per hour. These are starting positions an
    /// assembly may amend; they are not founder constants.
    #[must_use]
    pub fn base_rate_per_hour(self) -> i64 {
        match self {
            Self::Mentorship => 10,
            Self::Care | Self::EmergencyResponse => 8,
            Self::Labor => 5,
            Self::Education | Self::Rehabilitation => 4,
            Self::ContentCreation => 0,
        }
    }

    /// Hours of this kind that count toward the weekly expectation. Content
    /// creation is excluded by construction (it never mints per-hour).
    #[must_use]
    pub fn counts_toward_expectation(self) -> bool {
        !matches!(self, Self::ContentCreation)
    }

    /// Whether hours past the weekly expectation earn the overtime multiplier.
    /// You do not earn overtime for studying a twenty-first hour — you study for
    /// yourself — but you do for labor or care the node owes you for.
    #[must_use]
    pub fn overtime_eligible(self) -> bool {
        matches!(
            self,
            Self::Labor | Self::Care | Self::Mentorship | Self::EmergencyResponse
        )
    }
}

/// Multiplier defaults, in basis points. Governance-tunable.
pub const OVERTIME_BPS: i64 = 20_000; // 2.0×
pub const ESSENTIAL_BPS: i64 = 15_000; // 1.5×
pub const COVERAGE_BPS: i64 = 12_500; // 1.25×

/// Combine two multipliers multiplicatively over the base.
/// 1.5× combined with 1.25× yields 1.875× (18,750 bps), carried exactly.
#[must_use]
pub fn combine(a_bps: i64, b_bps: i64) -> i64 {
    ((a_bps as i128) * (b_bps as i128) / (ONE_BPS as i128)) as i64
}

/// The minting formula: rate × minutes × multiplier ÷ (60 × 10,000).
/// All intermediate work is done in `i128` so a long contribution week cannot
/// overflow. The result is a whole, non-negative integer count of Demiurge.
#[must_use]
pub fn mint_amount(rate_per_hour: i64, minutes: i64, multiplier_bps: i64) -> i64 {
    if rate_per_hour <= 0 || minutes <= 0 || multiplier_bps <= 0 {
        return 0;
    }
    let v = (rate_per_hour as i128) * (minutes as i128) * (multiplier_bps as i128)
        / (60i128 * ONE_BPS as i128);
    v.max(0) as i64
}

/// Compute the Demiurge minted for a single contribution span, applying the
/// essential and coverage multipliers, and splitting hours past the weekly
/// expectation into an overtime tier.
///
/// `prior_work_minutes` is the count of overtime-eligible minutes already
/// logged this week *before* this span, so the expectation threshold is honored
/// across a week of spans.
#[must_use]
pub fn mint_span(
    kind: ContributionKind,
    minutes: i64,
    essential: bool,
    coverage: bool,
    prior_work_minutes: i64,
) -> i64 {
    let rate = kind.base_rate_per_hour();
    if rate <= 0 || minutes <= 0 {
        return 0;
    }

    let mut mult = ONE_BPS;
    if essential {
        mult = combine(mult, ESSENTIAL_BPS);
    }
    if coverage {
        mult = combine(mult, COVERAGE_BPS);
    }

    let threshold = WEEKLY_EXPECTATION_HOURS * 60;
    let (regular, overtime) = if kind.overtime_eligible() {
        let reg = (threshold - prior_work_minutes).clamp(0, minutes);
        (reg, minutes - reg)
    } else {
        (minutes, 0)
    };

    let mut earned = mint_amount(rate, regular, mult);
    if overtime > 0 {
        earned += mint_amount(rate, overtime, combine(mult, OVERTIME_BPS));
    }
    earned
}

/// A lot expires twenty years after it was minted.
#[must_use]
pub fn expires_at(minted_at_unix: i64) -> i64 {
    minted_at_unix + DEMIURGE_EXPIRY_SECS
}

/// Whether a lot minted at `minted_at_unix` has decayed as of `now_unix`.
#[must_use]
pub fn is_expired(minted_at_unix: i64, now_unix: i64) -> bool {
    now_unix >= expires_at(minted_at_unix)
}

/// Convert a fiat amount (in minor units, e.g. cents) into Demiurge at a
/// governance-set conversion rate expressed in basis points of Demiurge per
/// fiat minor-unit.
///
/// This is how the Vicinagora payment path turns the *fees and taxes* on a fiat
/// transaction into Demiurge: the surplus that the old world skims is instead
/// minted as contribution credit and routed back into the node. Worth itself is
/// never priced — only the friction is converted.
#[must_use]
pub fn fiat_minor_to_demiurge(fiat_minor: i64, rate_bps: i64) -> i64 {
    if fiat_minor <= 0 || rate_bps <= 0 {
        return 0;
    }
    ((fiat_minor as i128) * (rate_bps as i128) / (ONE_BPS as i128)).max(0) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nurse_heavy_week_matches_white_paper() {
        // 20h essential care, then 10h essential + coverage + overtime.
        let regular = mint_span(ContributionKind::Care, 20 * 60, true, false, 0);
        let overnight = mint_span(ContributionKind::Care, 10 * 60, true, true, 20 * 60);
        assert_eq!(regular, 240);
        assert_eq!(overnight, 300);
        assert_eq!(regular + overnight, 540);
    }

    #[test]
    fn education_never_earns_overtime() {
        // 30h of education: rate 4, no overtime even past the 20h line.
        let earned = mint_span(ContributionKind::Education, 30 * 60, false, false, 0);
        assert_eq!(earned, 30 * 4);
    }

    #[test]
    fn lots_decay_after_twenty_years() {
        let minted = 1_000_000_000;
        assert!(!is_expired(minted, minted + DEMIURGE_EXPIRY_SECS - 1));
        assert!(is_expired(minted, minted + DEMIURGE_EXPIRY_SECS));
    }

    #[test]
    fn fees_convert_to_demiurge() {
        // $5.00 of fees (500 cents) at 1.0× (10,000 bps) -> 500 Demiurge.
        assert_eq!(fiat_minor_to_demiurge(500, ONE_BPS), 500);
        // at 0.5× -> 250.
        assert_eq!(fiat_minor_to_demiurge(500, 5_000), 250);
    }
}
