#[allow(dead_code)]
mod common;

use netsuite_cli::secrets::TbaSecrets;
use netsuite_cli::soap::SoapClient;
use netsuite_cli::soap::search_types;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn demo_secrets() -> TbaSecrets {
    TbaSecrets {
        consumer_key: "consumerkey123".into(),
        consumer_secret: "consumersecret789".into(),
        token_id: Some("tokenid456".into()),
        token_secret: Some("tokensecret012".into()),
    }
}

fn soap_client_for(server: &MockServer) -> SoapClient {
    SoapClient::new(
        reqwest::Client::new(),
        &server.uri(),
        "1234567_SB1",
        demo_secrets(),
    )
    .unwrap()
}

const ONE_ROW_RESPONSE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/">
 <soapenv:Body>
  <searchResponse xmlns="urn:messages_2025_2.platform.webservices.netsuite.com">
   <platformCore:searchResult xmlns:platformCore="urn:core_2025_2.platform.webservices.netsuite.com">
    <platformCore:status isSuccess="true"/>
    <platformCore:totalRecords>1</platformCore:totalRecords>
    <platformCore:pageSize>5</platformCore:pageSize>
    <platformCore:totalPages>1</platformCore:totalPages>
    <platformCore:pageIndex>1</platformCore:pageIndex>
    <platformCore:searchId>WEBSERVICES_1234567_ABC</platformCore:searchId>
    <platformCore:searchRowList>
     <platformCore:searchRow xmlns:tranSales="urn:sales_2025_2.transactions.webservices.netsuite.com" xsi:type="tranSales:TransactionSearchRow" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
      <tranSales:basic xmlns:platformCommon="urn:common_2025_2.platform.webservices.netsuite.com">
       <platformCommon:tranId><platformCore:searchValue>INV-1001</platformCore:searchValue></platformCommon:tranId>
       <platformCommon:entity><platformCore:searchValue internalId="55"/></platformCommon:entity>
       <platformCommon:otherRefNum>
        <platformCore:searchValue>PO-1</platformCore:searchValue>
        <platformCore:searchValue>PO-2</platformCore:searchValue>
       </platformCommon:otherRefNum>
       <platformCommon:customFieldList xmlns:platformCore="urn:core_2025_2.platform.webservices.netsuite.com">
        <platformCore:customField scriptId="custbody_example" xsi:type="platformCore:SearchColumnStringCustomField">
         <platformCore:searchValue>hello</platformCore:searchValue>
        </platformCore:customField>
       </platformCommon:customFieldList>
      </tranSales:basic>
      <tranSales:customerJoin xmlns:platformCommon="urn:common_2025_2.platform.webservices.netsuite.com">
       <platformCommon:email><platformCore:searchValue>a@example.com</platformCore:searchValue></platformCommon:email>
      </tranSales:customerJoin>
     </platformCore:searchRow>
    </platformCore:searchRowList>
   </platformCore:searchResult>
  </searchResponse>
 </soapenv:Body>
</soapenv:Envelope>"#;

#[tokio::test]
async fn search_posts_signed_envelope_with_soapaction() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/NetSuitePort_2025_2"))
        .and(header("SOAPAction", "\"search\""))
        .and(header("Content-Type", "text/xml; charset=utf-8"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ONE_ROW_RESPONSE))
        .expect(1)
        .mount(&server)
        .await;

    let search_type = search_types::lookup("transaction").unwrap();
    let result = soap_client_for(&server)
        .search(&search_type, "savedSearchId", "57", 1000)
        .await
        .unwrap();
    assert_eq!(result.rows.len(), 1);

    let request: &Request = &server.received_requests().await.unwrap()[0];
    let body = String::from_utf8_lossy(&request.body);
    assert!(body.contains(r#"savedSearchId="57""#));
    assert!(body.contains("TransactionSearchAdvanced"));
    assert!(body.contains("<core:account>1234567_SB1</core:account>"));
    assert!(body.contains(r#"algorithm="HMAC-SHA256""#));
    assert!(body.contains("<pageSize>1000</pageSize>"));
}

#[tokio::test]
async fn search_more_posts_search_id_and_page_index() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/NetSuitePort_2025_2"))
        .and(header("SOAPAction", "\"searchMoreWithId\""))
        .respond_with(ResponseTemplate::new(200).set_body_string(ONE_ROW_RESPONSE))
        .mount(&server)
        .await;

    soap_client_for(&server)
        .search_more("WEBSERVICES_1234567_ABC", 2)
        .await
        .unwrap();
    let request = &server.received_requests().await.unwrap()[0];
    let body = String::from_utf8_lossy(&request.body);
    assert!(body.contains("<searchId>WEBSERVICES_1234567_ABC</searchId>"));
    assert!(body.contains("<pageIndex>2</pageIndex>"));
}

#[tokio::test]
async fn http_500_fault_body_is_parsed_not_swallowed() {
    let server = MockServer::start().await;
    let fault = r#"<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/"><soapenv:Body>
      <soapenv:Fault><faultcode>soapenv:Server.userException</faultcode>
       <faultstring>com.netledger.common.exceptions.InvalidCredentialsException: Invalid login attempt.</faultstring>
      </soapenv:Fault></soapenv:Body></soapenv:Envelope>"#;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string(fault))
        .mount(&server)
        .await;
    let search_type = search_types::lookup("customer").unwrap();
    let error = soap_client_for(&server)
        .search(&search_type, "savedSearchId", "1", 5)
        .await
        .unwrap_err();
    assert!(matches!(error, netsuite_cli::error::CliError::Auth(_)));
}

#[test]
fn client_without_minted_token_is_an_auth_error() {
    let mut secrets = demo_secrets();
    secrets.token_id = None;
    let result = SoapClient::new(
        reqwest::Client::new(),
        "https://example.invalid",
        "1234567",
        secrets,
    );
    assert!(matches!(
        result.unwrap_err(),
        netsuite_cli::error::CliError::Auth(_)
    ));
}
