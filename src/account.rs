pub fn rest_base(account_id: &str) -> String {
    format!("https://{}.suitetalk.api.netsuite.com", account_domain_id(account_id))
}

pub fn restlet_base(account_id: &str) -> String {
    format!("https://{}.restlets.api.netsuite.com", account_domain_id(account_id))
}

pub fn app_base(account_id: &str) -> String {
    format!("https://{}.app.netsuite.com", account_domain_id(account_id))
}

pub fn account_domain_id(account_id: &str) -> String {
    account_id.to_lowercase().replace('_', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_account_ids_map_to_hyphenated_lowercase_domains() {
        assert_eq!(account_domain_id("123456_SB1"), "123456-sb1");
        assert_eq!(account_domain_id("1234567"), "1234567");
        assert_eq!(rest_base("123456_SB1"), "https://123456-sb1.suitetalk.api.netsuite.com");
        assert_eq!(restlet_base("123456_SB1"), "https://123456-sb1.restlets.api.netsuite.com");
        assert_eq!(app_base("123456_SB1"), "https://123456-sb1.app.netsuite.com");
    }
}
