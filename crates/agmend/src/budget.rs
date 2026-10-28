//! Private worker budget steps for the Claude Code launcher.

/// Millionths of a dollar, with no floating-point conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Money(u64);

#[derive(Debug, thiserror::Error)]
#[error("invalid or out-of-range total_cost_usd")]
pub(crate) struct InvalidCost;

impl Money {
    pub(crate) fn usd(self) -> String {
        format!("{}.{:06}", self.0 / 1_000_000, self.0 % 1_000_000)
    }
    /// Parses a raw JSON number token, not a quoted string or an f64 rendering.
    /// Nonnegative JSON decimal syntax; out-of-range amounts saturate.
    /// Fractions below one millionth are truncated, so rounding cannot fire a
    /// step early. Validate the entire token before saturating an overflow.
    pub(crate) fn from_usd(value: &str) -> Result<Self, InvalidCost> {
        if value.is_empty() {
            return Err(InvalidCost);
        }
        let (decimal, exponent) = match value.split_once(['e', 'E']) {
            Some((decimal, exponent)) => {
                let (negative, digits) = if let Some(digits) = exponent.strip_prefix('-') {
                    (true, digits)
                } else {
                    (false, exponent.strip_prefix('+').unwrap_or(exponent))
                };
                if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(InvalidCost);
                }
                // Both scans are bounded by the input token length.
                let exponent = digits.bytes().fold(0_i64, |value, byte| {
                    value
                        .saturating_mul(10)
                        .saturating_add(i64::from(byte - b'0'))
                });
                (decimal, if negative { -exponent } else { exponent })
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
        // Validate before conversion so a malformed suffix cannot trigger Stop.
        for byte in whole.bytes().chain(fraction.bytes()) {
            if !byte.is_ascii_digit() {
                return Err(InvalidCost);
            }
        }
        let fraction_digits = i64::try_from(fraction.len()).map_err(|_| InvalidCost)?;
        let digits = whole
            .bytes()
            .chain(fraction.bytes())
            .skip_while(|byte| *byte == b'0');
        let significant = i64::try_from(digits.clone().count()).map_err(|_| InvalidCost)?;
        if significant == 0 {
            return Ok(Self(0));
        }
        let places = exponent
            .saturating_sub(fraction_digits)
            .saturating_add(significant)
            .saturating_add(6);
        if places <= 0 {
            return Ok(Self(0));
        }
        if places > 20 {
            return Ok(Self(u64::MAX));
        }
        // At most 20 integer micro-dollar digits, padding with zeros if needed.
        let micros = digits
            .chain(std::iter::repeat(b'0'))
            .take(places as usize)
            .fold(0_u64, |value, byte| {
                value
                    .saturating_mul(10)
                    .saturating_add(u64::from(byte - b'0'))
            });
        Ok(Self(micros))
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

    pub(crate) fn remaining_usd(&self) -> String {
        Money(self.cap.0.saturating_sub(self.spent.0)).usd()
    }

    pub(crate) fn exhausted(&self) -> bool {
        self.spent >= self.cap
    }

    /// Adds one batch's reported cost to the managed agent's lifetime spend.
    pub(crate) fn add_cost_usd(
        &mut self,
        value: &str,
        fire: impl FnMut(Step),
    ) -> Result<(), InvalidCost> {
        let cost = Money::from_usd(value)?;
        let total = Money(self.spent.0.saturating_add(cost.0));
        self.observe_total_cost_usd(&total.usd(), fire)
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
