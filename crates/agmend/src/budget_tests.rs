use super::*;

fn money(value: &str) -> Money {
    Money::from_usd(value).unwrap()
}

#[test]
fn each_step_fires_once_in_order() {
    let mut budget = Budget::new(money("1.00"));
    let mut steps = Vec::new();
    for (value, count) in [
        ("0.799999", 0),
        ("0.80", 1),
        ("0.949999", 1),
        ("0.95", 2),
        ("0.999999", 2),
        ("1.00", 3),
        ("1.05", 3),
    ] {
        budget
            .observe_total_cost_usd(value, |step| steps.push(step))
            .unwrap();
        assert_eq!(steps.len(), count, "cost {value}");
    }
    assert_eq!(steps, [Step::Warn, Step::WrapUp, Step::Stop]);
    assert_eq!(budget.spent(), money("1.05"));
}

#[test]
fn jump_to_cap_fires_all_steps_in_order() {
    let mut budget = Budget::new(money("5"));
    let mut steps = Vec::new();
    budget
        .observe_total_cost_usd("5", |step| steps.push(step))
        .unwrap();
    assert_eq!(steps, [Step::Warn, Step::WrapUp, Step::Stop]);
}

#[test]
fn repeated_equal_values_fire_nothing_new() {
    let mut budget = Budget::new(money("1"));
    let mut steps = Vec::new();
    for value in ["0.8", "0.8", "0.80", "0.95", "0.95", "1", "1"] {
        budget
            .observe_total_cost_usd(value, |step| steps.push(step))
            .unwrap();
    }
    assert_eq!(steps, [Step::Warn, Step::WrapUp, Step::Stop]);
}

#[test]
fn zero_cap_fires_once_on_first_valid_observation() {
    let mut budget = Budget::new(money("0"));
    let mut steps = Vec::new();
    assert!(
        budget
            .observe_total_cost_usd("null", |step| steps.push(step))
            .is_err()
    );
    assert!(steps.is_empty());
    for value in ["0", "0", "0.1"] {
        budget
            .observe_total_cost_usd(value, |step| steps.push(step))
            .unwrap();
    }
    assert_eq!(steps, [Step::Warn, Step::WrapUp, Step::Stop]);
}

#[test]
fn overflow_saturates_spend_and_fires_remaining_steps() {
    for value in [
        "18446744073709.551616",
        "1e100",
        "999999999999999999999999999999999999999999",
        "1e99999999999999999999999999999999999999999",
    ] {
        let mut budget = Budget::new(money("1"));
        let mut steps = Vec::new();
        budget
            .observe_total_cost_usd("0.8", |step| steps.push(step))
            .unwrap();
        budget
            .observe_total_cost_usd(value, |step| steps.push(step))
            .unwrap();
        assert_eq!(budget.spent(), Money(u64::MAX));
        assert_eq!(steps, [Step::Warn, Step::WrapUp, Step::Stop]);
        budget
            .observe_total_cost_usd(value, |step| steps.push(step))
            .unwrap();
        assert_eq!(steps, [Step::Warn, Step::WrapUp, Step::Stop]);
    }
}

#[test]
fn lower_and_malformed_values_preserve_high_water_mark() {
    let mut budget = Budget::new(money("1"));
    let mut steps = Vec::new();
    budget
        .observe_total_cost_usd("0.95", |step| steps.push(step))
        .unwrap();
    for value in ["0.1", "0", "0.8"] {
        budget
            .observe_total_cost_usd(value, |step| steps.push(step))
            .unwrap();
        assert_eq!(budget.spent(), money("0.95"));
    }
    for value in [
        "",
        "-1",
        "-0",
        "NaN",
        "Infinity",
        "null",
        "true",
        "\"0.99\"",
        "01",
        ".8",
        "1.",
        "1e",
        "1e2e3",
        "1 2",
        "0.9x",
        "0.8\n",
        "1e+",
        "1e 2",
        "999999999999999999999999999999999999999999x",
        "999999999999999999999999999999999999999999eNaN",
    ] {
        assert!(
            budget
                .observe_total_cost_usd(value, |step| steps.push(step))
                .is_err(),
            "{value:?}"
        );
        assert_eq!(budget.spent(), money("0.95"));
        assert_eq!(steps, [Step::Warn, Step::WrapUp]);
    }
    budget
        .observe_total_cost_usd("1", |step| steps.push(step))
        .unwrap();
    assert_eq!(steps, [Step::Warn, Step::WrapUp, Step::Stop]);
}

#[test]
fn decimal_conversion_is_fixed_point_and_supports_exponents() {
    assert_eq!(money("0.000001"), Money(1));
    assert_eq!(money("1.23456789"), Money(1_234_567));
    assert_eq!(money("8e-1"), money("0.8"));
    assert_eq!(money("80E-2"), money("0.8"));
    assert_eq!(money("8.0e+1"), money("80"));
    assert_eq!(money("1e-100"), Money(0));
    assert_eq!(money("18446744073709.551615"), Money(u64::MAX));
    assert_eq!(
        money("123456789012345678901234567890123456789012e-40"),
        Money(12_345_678)
    );
    assert_eq!(
        money("0.000000000000000000000000000000000000000001e42"),
        money("1")
    );
    assert_eq!(money("0e999999999999999999999"), Money(0));
    assert_eq!(money("1e-999999999999999999999"), Money(0));
    assert!(Money::from_usd(&"0".repeat(129)).is_err());
}

#[test]
fn large_cap_threshold_math_does_not_overflow() {
    let mut budget = Budget::new(Money(u64::MAX));
    let mut steps = Vec::new();
    budget
        .observe_total_cost_usd("18446744073709.551614", |step| steps.push(step))
        .unwrap();
    assert_eq!(steps, [Step::Warn, Step::WrapUp]);
    budget
        .observe_total_cost_usd("18446744073709.551615", |step| steps.push(step))
        .unwrap();
    assert_eq!(steps, [Step::Warn, Step::WrapUp, Step::Stop]);
}
