//! The five Paddle price ids and the plans they sell.
//!
//! The hub does not talk to Paddle here. Callers pass the ids in from
//! configuration, or from a later catalog sync.

use super::plan::{BusinessStep, Plan};

/// One price on a Paddle checkout or subscription, and how many of it to buy.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LineItem {
    /// Paddle price id, such as `pri_...`.
    pub price_id: String,
    /// How many of that price. Team's seat price uses the extra-seat count.
    pub quantity: u32,
}

/// The price ids for Team and the three Business steps.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Catalog {
    /// Team base price, quantity 1.
    pub team_base: String,
    /// Team extra-seat price. Its quantity is the number of extra seats.
    pub team_seat: String,
    /// Business price for 10 seats.
    pub business_10: String,
    /// Business price for 25 seats.
    pub business_25: String,
    /// Business price for 50 seats.
    pub business_50: String,
}

impl Catalog {
    /// The Paddle items that sell `plan`.
    ///
    /// Team with no extra seats is the base price alone. Extra seats add the
    /// seat price at that quantity. Business is one price at quantity 1.
    #[must_use]
    pub fn items_for(&self, plan: Plan) -> Vec<LineItem> {
        match plan {
            Plan::Team { extra_seats } => {
                let mut items = Vec::with_capacity(if extra_seats == 0 { 1 } else { 2 });
                items.push(LineItem {
                    price_id: self.team_base.clone(),
                    quantity: 1,
                });
                if extra_seats > 0 {
                    items.push(LineItem {
                        price_id: self.team_seat.clone(),
                        quantity: extra_seats,
                    });
                }
                items
            }
            Plan::Business(step) => {
                let price_id = match step {
                    BusinessStep::Seats10 => self.business_10.clone(),
                    BusinessStep::Seats25 => self.business_25.clone(),
                    BusinessStep::Seats50 => self.business_50.clone(),
                };
                vec![LineItem {
                    price_id,
                    quantity: 1,
                }]
            }
        }
    }

    /// The plan `items` sell, or `None` when they are not one of this catalog's shapes.
    ///
    /// The shape is the inverse of [`Self::items_for`]: Team base at quantity 1,
    /// optionally the seat price at a quantity of at least 1, or one Business
    /// price at quantity 1. Order does not matter. Any other combination,
    /// including an unknown price id, is `None`.
    #[must_use]
    pub fn plan_of(&self, items: &[LineItem]) -> Option<Plan> {
        match items {
            [only] => self.single_item(only),
            [first, second] => self.team_with_seat(first, second),
            _ => None,
        }
    }

    /// The five `AGMEN_PADDLE_PRICE_*` lines, each `NAME=value`, ending with a newline.
    ///
    /// `agmen-hub paddle-catalog` prints this. The names are the ones
    /// [`crate::Config`] reads in hosted mode.
    #[must_use]
    pub fn env_lines(&self) -> String {
        format!(
            "\
AGMEN_PADDLE_PRICE_TEAM_BASE={}
AGMEN_PADDLE_PRICE_TEAM_SEAT={}
AGMEN_PADDLE_PRICE_BUSINESS_10={}
AGMEN_PADDLE_PRICE_BUSINESS_25={}
AGMEN_PADDLE_PRICE_BUSINESS_50={}
",
            self.team_base, self.team_seat, self.business_10, self.business_25, self.business_50,
        )
    }

    fn single_item(&self, item: &LineItem) -> Option<Plan> {
        if item.quantity != 1 {
            return None;
        }
        if item.price_id == self.team_base {
            return Some(Plan::Team { extra_seats: 0 });
        }
        let step = if item.price_id == self.business_10 {
            BusinessStep::Seats10
        } else if item.price_id == self.business_25 {
            BusinessStep::Seats25
        } else if item.price_id == self.business_50 {
            BusinessStep::Seats50
        } else {
            return None;
        };
        Some(Plan::Business(step))
    }

    fn team_with_seat(&self, first: &LineItem, second: &LineItem) -> Option<Plan> {
        let (base, seat) = if first.price_id == self.team_base && second.price_id == self.team_seat
        {
            (first, second)
        } else if second.price_id == self.team_base && first.price_id == self.team_seat {
            (second, first)
        } else {
            return None;
        };
        if base.quantity == 1 && seat.quantity >= 1 {
            Some(Plan::Team {
                extra_seats: seat.quantity,
            })
        } else {
            None
        }
    }
}
