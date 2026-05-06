use rust_decimal::Decimal;

pub const ZERO_DECIMAL_PAIR: (Decimal, Decimal) = (Decimal::ZERO, Decimal::ZERO);

pub struct Factor {
    pub numerator: Decimal,
    pub denominator: Decimal,
}

impl Factor {
    pub fn new(numerator: Decimal, denominator: Decimal) -> Self {
        Self { numerator, denominator }
    }

    pub fn apply(&self, value: Decimal) -> Decimal {
        if self.denominator == Decimal::ZERO {
            Decimal::ZERO
        } else {
            value * self.numerator / self.denominator
        }
    }
}

pub fn zero_factor() -> Factor {
    Factor::new(Decimal::ZERO, Decimal::ONE)
}
