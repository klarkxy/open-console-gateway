use super::*;

#[test]
fn exact_product_identity_keeps_the_path_before_origin_reduction() {
    for (url, product, balance) in [
        (
            "https://API.STEPFUN.COM:443/v1",
            OfficialService::StepFunApi,
            true,
        ),
        (
            "https://api.stepfun.com/step_plan",
            OfficialService::StepFunPlan,
            false,
        ),
        (
            "https://api.stepfun.com/step_plan/v1",
            OfficialService::StepFunPlan,
            false,
        ),
        (
            "https://api.stepfun.com/step_planet",
            OfficialService::StepFunApi,
            true,
        ),
        (
            "https://api.deepseek.com/step_plan/v1",
            OfficialService::DeepSeek,
            true,
        ),
        (
            "https://api.moonshot.cn/v1",
            OfficialService::MoonshotCn,
            true,
        ),
        (
            "https://api.moonshot.ai/v1",
            OfficialService::MoonshotGlobal,
            true,
        ),
    ] {
        assert_eq!(identify(url), Some(product), "{url}");
        assert_eq!(has_official_balance(url), balance, "{url}");
        assert_eq!(
            crate::api_balance::probe_from_endpoint(url).is_some(),
            balance
        );
        assert_eq!(
            crate::billing::is_stepfun_plan_endpoint(url),
            product == OfficialService::StepFunPlan
        );
    }
}

#[test]
fn every_official_service_requires_the_exact_https_origin() {
    for host in [
        "api.stepfun.com",
        "api.deepseek.com",
        "api.moonshot.cn",
        "api.moonshot.ai",
    ] {
        for url in [
            format!("http://{host}/v1"),
            format!("https://{host}:444/v1"),
            format!("https://{host}.evil.test/v1"),
            format!("https://user:secret@{host}/v1"),
            format!("https://{host}/v1?key=x"),
            format!("https://{host}/v1#section"),
        ] {
            assert_eq!(identify(&url), None, "{url}");
            assert!(!has_official_balance(&url));
        }
    }
}
