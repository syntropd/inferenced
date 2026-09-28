//! End-to-End Integration Test Suite for systemd-inferenced
//! Covers Tier 3 (Cross-Feature Interactions) and Tier 4 (Real-World Application Scenarios)

mod cross_feature_tests;
mod cross_feature_ext;
mod scenario_streaming;
mod scenario_panic_triage;
mod scenario_psi_paging;
mod scenario_varlink_mgmt;
mod scenario_multitenant;
