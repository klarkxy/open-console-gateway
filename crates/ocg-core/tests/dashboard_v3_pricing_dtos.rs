//! Retired pricing DTOs stay out of the V3 catalog. Usage and summary costs
//! that remain are nullable: missing evidence is null, not zero.

use ocg_core::dashboard_v3::{CATALOG_TYPE_NAMES, contract_schema};
use serde_json::Value;

const RETIRED_PRICING_TYPES: &[&str] = &[
    "PricingSnapshot",
    "PricingLimits",
    "PricingModel",
    "PricingAdjustment",
    "PricingTimeWindow",
    "PricingRefresh",
    "PricingRefreshStatus",
    "PricingMultiplierChange",
    "PricingRefreshUpdate",
    "PricingRefreshPolicy",
    "PricingMultipliersUpdate",
    "PricingMultiplierWrite",
    "ProviderPricing",
    "PricingAvailability",
    "ProviderPricingSnapshot",
    "ProviderPricingValue",
    "ProviderPricingRefresh",
    "ProviderPricingRefreshUpdate",
    "PricingRevision",
];

fn allows_null(schema: &Value) -> bool {
    if schema.get("type").and_then(Value::as_str) == Some("null") {
        return true;
    }
    if let Some(types) = schema.get("type").and_then(Value::as_array)
        && types.iter().any(|value| value == "null")
    {
        return true;
    }
    schema
        .get("anyOf")
        .and_then(Value::as_array)
        .is_some_and(|items| items.iter().any(allows_null))
}

#[test]
fn retired_pricing_dtos_are_absent_and_remaining_costs_are_nullable() {
    let schema = contract_schema();
    let defs = schema["$defs"].as_object().expect("catalog $defs");
    for name in CATALOG_TYPE_NAMES {
        assert!(defs.contains_key(*name), "schema missing {name}");
    }
    for name in RETIRED_PRICING_TYPES {
        assert!(
            !CATALOG_TYPE_NAMES.contains(name),
            "{name} must leave the V3 catalog"
        );
        assert!(!defs.contains_key(*name), "{name} must leave $defs");
    }

    let usage = defs["UsageWindow"]["properties"].as_object().unwrap();
    for field in ["window5h", "windowWeek", "windowMonth", "pricingRevision"] {
        assert!(allows_null(&usage[field]), "{field} is nullable");
    }
    let summary = defs["ForwardLogSummary"]["properties"].as_object().unwrap();
    assert!(allows_null(&summary["cost"]));
    let home = defs["DashboardSummary"]["properties"].as_object().unwrap();
    for field in ["todayCost", "weekCost", "monthCost"] {
        assert!(allows_null(&home[field]), "{field} is nullable");
    }
    let models = defs["ApplicationModels"]["properties"].as_object().unwrap();
    assert_eq!(models["pricingRevision"]["type"], "string");
    assert!(
        defs["ConnectionInfo"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("primaryKey"),
        "ConnectionInfo remains the secret-bearing connection DTO"
    );
}
