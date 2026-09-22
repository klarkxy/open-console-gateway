//! Static first-party product identity from the complete validated endpoint.
//! Path-sensitive products are classified before reducing a URL to its origin.
use ocg_domain::billing::BillingModel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OfficialService {
    DeepSeek,
    MoonshotCn,
    MoonshotGlobal,
    StepFunApi,
    StepFunPlan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BalanceKind {
    DeepSeek,
    Moonshot,
    StepFun,
}

pub(crate) struct BalanceReader {
    pub kind: BalanceKind,
    pub path: &'static str,
    pub source: &'static str,
    pub unit: &'static str,
}

pub(crate) fn identify(endpoint: &str) -> Option<OfficialService> {
    let url = reqwest::Url::parse(endpoint.trim()).ok()?;
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    match url.host_str()? {
        "api.deepseek.com" => Some(OfficialService::DeepSeek),
        "api.moonshot.cn" => Some(OfficialService::MoonshotCn),
        "api.moonshot.ai" => Some(OfficialService::MoonshotGlobal),
        "api.stepfun.com" => Some(
            if url.path() == "/step_plan" || url.path().starts_with("/step_plan/") {
                OfficialService::StepFunPlan
            } else {
                OfficialService::StepFunApi
            },
        ),
        _ => None,
    }
}

impl OfficialService {
    pub(crate) fn balance_reader(self) -> Option<BalanceReader> {
        let (kind, path, source, unit) = match self {
            Self::DeepSeek => (
                BalanceKind::DeepSeek,
                "user/balance",
                "deepseek-official",
                "cny",
            ),
            Self::MoonshotCn => (
                BalanceKind::Moonshot,
                "v1/users/me/balance",
                "moonshot-official",
                "cny",
            ),
            Self::MoonshotGlobal => (
                BalanceKind::Moonshot,
                "v1/users/me/balance",
                "moonshot-official",
                "usd",
            ),
            Self::StepFunApi => (
                BalanceKind::StepFun,
                "v1/accounts",
                "stepfun-api-official",
                "cny",
            ),
            Self::StepFunPlan => return None,
        };
        Some(BalanceReader {
            kind,
            path,
            source,
            unit,
        })
    }

    pub(crate) fn billing_model(self) -> BillingModel {
        if self == Self::StepFunPlan {
            BillingModel::Credits
        } else {
            BillingModel::Cash
        }
    }
}

pub(crate) fn has_official_balance(endpoint: &str) -> bool {
    identify(endpoint)
        .and_then(OfficialService::balance_reader)
        .is_some()
}

#[cfg(test)]
mod tests;
