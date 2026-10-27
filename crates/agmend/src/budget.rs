//! Private worker budget steps for the Claude Code launcher.
// shortcut: L17 wires this module and removes this expectation.
#![cfg_attr(not(test), expect(dead_code, reason = "wired by L17"))]

/// Millionths of a dollar, with no floating-point conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Money(u64);

#[derive(Debug, thiserror::Error)]
#[error("invalid or out-of-range total_cost_usd")]
pub(crate) struct InvalidCost;

impl Money {
    /// Parses a raw JSON number token, not a quoted string or an f64 rendering.
    /// At most 128 bytes; nonnegative JSON decimal syntax with an i32 exponent.
    /// Fractions below one millionth are truncated, so rounding cannot fire a
    /// step early. Rejects coefficient or money overflow without changing spend.
    pub(crate) fn from_usd(value: &str) -> Result<Self, InvalidCost> {
        if value.is_empty() || value.len() > 128 {
            return Err(InvalidCost);
        }
        let (decimal, exponent) = match value.split_once(['e', 'E']) {
            Some((decimal, exponent)) => {
                (decimal, exponent.parse::<i32>().map_err(|_| InvalidCost)?)
            }
            None => (value, 0),
        };
        let (whole, fraction) = match decimal.split_once('.') {
            Some((whole, fraction)) if !fraction.is_empty() => (whole, fraction),
            Some(_) => return Err(InvalidCost),
            None => (decimal, ""),
        };
        if whole.is_empty() || (whole.len() > 1 && whole.starts_with('0')) {
            return Err(InvalidCost);
        }
        let mut coefficient = 0_u128;
        // The token's 128-byte limit bounds this loop and its checked arithmetic.
        for byte in whole.bytes().chain(fraction.bytes()) {
            if !byte.is_ascii_digit() {
                return Err(InvalidCost);
            }
            coefficient = coefficient
                .checked_mul(10)
                .and_then(|value| value.checked_add(u128::from(byte - b'0')))
                .ok_or(InvalidCost)?;
        }
        let fraction_digits = i64::try_from(fraction.len()).map_err(|_| InvalidCost)?;
        let shift = i64::from(exponent) + 6 - fraction_digits;
        let power = u32::try_from(shift.unsigned_abs()).map_err(|_| InvalidCost)?;
        let micros = if coefficient == 0 {
            0
        } else if shift >= 0 {
            10_u128
                .checked_pow(power)
                .and_then(|scale| coefficient.checked_mul(scale))
                .ok_or(InvalidCost)?
        } else {
            match 10_u128.checked_pow(power) {
                Some(scale) => coefficient / scale,
                None => 0,
            }
        };
        Ok(Self(u64::try_from(micros).map_err(|_| InvalidCost)?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Warn,
    WrapUp,
    Stop,
}

#[derive(Debug)]
pub(crate) struct Budget {
    cap: Money,
    spent: Money,
    fired: usize,
}

impl Budget {
    /// One tracker per worker session; lower totals never start a new session.
    pub(crate) fn new(cap: Money) -> Self {
        Self {
            cap,
            spent: Money(0),
            fired: 0,
        }
    }

    pub(crate) fn spent(&self) -> Money {
        self.spent
    }

    pub(crate) fn observe_total_cost_usd(
        &mut self,
        value: &str,
        mut fire: impl FnMut(Step),
    ) -> Result<(), InvalidCost> {
        let cost = Money::from_usd(value)?;
        if cost < self.spent {
            return Ok(());
        }
        self.spent = cost;
        // Fixed three-step scan; u128 products fit every u64 money value.
        // A zero cap fires all steps on the first valid observation, even zero.
        for (index, (percent, step)) in
            [(80_u128, Step::Warn), (95, Step::WrapUp), (100, Step::Stop)]
                .into_iter()
                .enumerate()
        {
            if index >= self.fired && u128::from(cost.0) * 100 >= u128::from(self.cap.0) * percent {
                // Mark before delivery so a callback that unwinds cannot repeat it.
                self.fired = index + 1;
                fire(step);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "budget_tests.rs"]
mod tests;
