use crate::soap::search_types::SearchType;

const MESSAGES_NS: &str = "urn:messages_2025_2.platform.webservices.netsuite.com";
const CORE_NS: &str = "urn:core_2025_2.platform.webservices.netsuite.com";

pub struct TokenPassport {
    pub account_id: String,
    pub consumer_key: String,
    pub token_id: String,
    pub nonce: String,
    pub timestamp: u64,
    pub signature: String,
}

pub fn xml_escape(raw: &str) -> String {
    raw.chars()
        .map(|ch| match ch {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            '\'' => "&apos;".to_string(),
            other => other.to_string(),
        })
        .collect()
}

pub fn search_envelope(
    passport: &TokenPassport,
    page_size: u64,
    search_type: &SearchType,
    id_attribute: &str,
    saved_search_id: &str,
) -> String {
    let body = format!(
        r#"<search xmlns="{MESSAGES_NS}"><searchRecord xmlns:q1="{}" xsi:type="q1:{}" {}="{}"/></search>"#,
        search_type.namespace,
        search_type.xsi_type,
        id_attribute,
        xml_escape(saved_search_id),
    );
    envelope_with_body(passport, page_size, &body)
}

pub fn search_more_envelope(passport: &TokenPassport, search_id: &str, page_index: u64) -> String {
    let body = format!(
        "<searchMoreWithId xmlns=\"{MESSAGES_NS}\"><searchId>{}</searchId><pageIndex>{page_index}</pageIndex></searchMoreWithId>",
        xml_escape(search_id),
    );
    // searchMoreWithId reuses the pageSize fixed by the original search; the
    // preferences header is still sent because NetSuite requires the header block.
    envelope_with_body(passport, 0, &body)
}

fn envelope_with_body(passport: &TokenPassport, page_size: u64, body: &str) -> String {
    let page_size_element = if page_size > 0 {
        format!("<pageSize>{page_size}</pageSize>")
    } else {
        String::new()
    };
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">"#,
            "<soap:Header>",
            r#"<tokenPassport xmlns="{messages}" xmlns:core="{core}">"#,
            "<core:account>{account}</core:account>",
            "<core:consumerKey>{consumer_key}</core:consumerKey>",
            "<core:token>{token}</core:token>",
            "<core:nonce>{nonce}</core:nonce>",
            "<core:timestamp>{timestamp}</core:timestamp>",
            r#"<core:signature algorithm="HMAC-SHA256">{signature}</core:signature>"#,
            "</tokenPassport>",
            r#"<searchPreferences xmlns="{messages}">"#,
            "<bodyFieldsOnly>true</bodyFieldsOnly>",
            "<returnSearchColumns>true</returnSearchColumns>",
            "{page_size_element}",
            "</searchPreferences>",
            "</soap:Header>",
            "<soap:Body>{body}</soap:Body>",
            "</soap:Envelope>",
        ),
        messages = MESSAGES_NS,
        core = CORE_NS,
        account = xml_escape(&passport.account_id),
        consumer_key = xml_escape(&passport.consumer_key),
        token = xml_escape(&passport.token_id),
        nonce = xml_escape(&passport.nonce),
        timestamp = passport.timestamp,
        signature = xml_escape(&passport.signature),
        page_size_element = page_size_element,
        body = body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo_passport() -> TokenPassport {
        TokenPassport {
            account_id: "1234567_SB1".into(),
            consumer_key: "consumerkey123".into(),
            token_id: "tokenid456".into(),
            nonce: "ABCDEFGHIJKLMNOPQRST".into(),
            timestamp: 1_700_000_000,
            signature: "SIG=".into(),
        }
    }

    #[test]
    fn search_envelope_contains_passport_preferences_and_typed_search_record() {
        let search_type = crate::soap::search_types::lookup("transaction").unwrap();
        let envelope = search_envelope(&demo_passport(), 1000, &search_type, "savedSearchId", "57");
        for expected in [
            r#"<tokenPassport xmlns="urn:messages_2025_2.platform.webservices.netsuite.com""#,
            r#"xmlns:core="urn:core_2025_2.platform.webservices.netsuite.com""#,
            "<core:account>1234567_SB1</core:account>",
            "<core:consumerKey>consumerkey123</core:consumerKey>",
            "<core:token>tokenid456</core:token>",
            "<core:nonce>ABCDEFGHIJKLMNOPQRST</core:nonce>",
            "<core:timestamp>1700000000</core:timestamp>",
            r#"<core:signature algorithm="HMAC-SHA256">SIG=</core:signature>"#,
            "<bodyFieldsOnly>true</bodyFieldsOnly>",
            "<returnSearchColumns>true</returnSearchColumns>",
            "<pageSize>1000</pageSize>",
            r#"<search xmlns="urn:messages_2025_2.platform.webservices.netsuite.com">"#,
            r#"xmlns:q1="urn:sales_2025_2.transactions.webservices.netsuite.com""#,
            r#"xsi:type="q1:TransactionSearchAdvanced""#,
            r#"savedSearchId="57""#,
        ] {
            assert!(
                envelope.contains(expected),
                "missing {expected} in:\n{envelope}"
            );
        }
    }

    #[test]
    fn search_envelope_escapes_attribute_and_text_values() {
        let search_type = crate::soap::search_types::lookup("customer").unwrap();
        let mut passport = demo_passport();
        passport.account_id = r#"A&B<"quote""#.into();
        let envelope = search_envelope(
            &passport,
            5,
            &search_type,
            "savedSearchScriptId",
            r#"custom"search&"#,
        );
        assert!(envelope.contains("<core:account>A&amp;B&lt;&quot;quote&quot;</core:account>"));
        assert!(envelope.contains(r#"savedSearchScriptId="custom&quot;search&amp;""#));
    }

    #[test]
    fn search_more_envelope_carries_search_id_and_page_index() {
        let envelope = search_more_envelope(&demo_passport(), "WEBSERVICES_1234567", 3);
        assert!(envelope.contains("<searchId>WEBSERVICES_1234567</searchId>"));
        assert!(envelope.contains("<pageIndex>3</pageIndex>"));
        assert!(envelope.contains(
            r#"<searchMoreWithId xmlns="urn:messages_2025_2.platform.webservices.netsuite.com">"#
        ));
        assert!(envelope.contains("<core:signature"));
    }
}
