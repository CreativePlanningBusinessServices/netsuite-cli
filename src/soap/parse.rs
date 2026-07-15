//! Generic SOAP search response parser.
//!
//! Parses by local element name only — namespace prefixes vary by account, so matching
//! must never depend on the prefix bound to a given URN in any particular response.

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use serde_json::{Map, Value, json};

use crate::error::CliError;

#[derive(Debug, Default)]
pub struct SoapSearchResult {
    pub total_records: u64,
    pub page_size: u64,
    pub total_pages: u64,
    pub page_index: u64,
    pub search_id: String,
    pub rows: Vec<Value>,
}

pub fn parse_search_response(xml: &str) -> Result<SoapSearchResult, CliError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut state = ParserState::default();

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                let name = local_name(&element);
                state.handle_open(&name, &element);
                state.path.push(name);
            }
            Ok(Event::Empty(element)) => {
                let name = local_name(&element);
                state.handle_open(&name, &element);
            }
            Ok(Event::Text(text)) => match text.unescape() {
                Ok(value) => state.handle_text(&value),
                Err(read_error) => return Err(unparseable(read_error)),
            },
            Ok(Event::End(_)) => {
                if let Some(closed) = state.path.pop() {
                    state.handle_close(&closed);
                }
            }
            Ok(Event::Eof) => break,
            Err(read_error) => return Err(unparseable(read_error)),
            _ => {}
        }
    }

    state.finish()
}

fn unparseable(read_error: impl std::fmt::Display) -> CliError {
    CliError::Api {
        status: 0,
        message: format!("unparseable SOAP response: {read_error}"),
        details: vec![],
    }
}

fn local_name(element: &BytesStart) -> String {
    String::from_utf8_lossy(element.local_name().as_ref()).into_owned()
}

fn attr(element: &BytesStart, name: &str) -> Option<String> {
    element.attributes().flatten().find_map(|attribute| {
        if attribute.key.local_name().as_ref() == name.as_bytes() {
            Some(String::from_utf8_lossy(&attribute.value).into_owned())
        } else {
            None
        }
    })
}

/// Single-pass parser state, threaded through the quick-xml event loop. Elements are
/// matched by local name only — namespace prefixes vary by NetSuite account.
#[derive(Default)]
struct ParserState {
    result: SoapSearchResult,
    is_success: Option<bool>,
    status_details: Vec<(String, String)>,
    fault: Option<(String, String)>,
    in_fault: bool,
    fault_code_buffer: String,
    fault_string_buffer: String,
    pending_detail_code: Option<String>,
    pending_detail_message: Option<String>,

    // element-path context, tracked as a stack of local names
    path: Vec<String>,

    // row-building state
    current_row: Option<Map<String, Value>>,
    section: Option<String>,            // "basic" or "<name>Join"
    field_key: Option<String>,          // commit key: field name, or customField scriptId
    field_element_name: Option<String>, // element local name closing the current field
    values: Vec<String>,                // searchValues collected for the current field
    search_value_attr_pushed: bool,
}

impl ParserState {
    fn handle_open(&mut self, name: &str, element: &BytesStart) {
        match name {
            "Fault" => {
                self.in_fault = true;
                self.fault_code_buffer.clear();
                self.fault_string_buffer.clear();
            }
            "status" => {
                self.is_success = attr(element, "isSuccess").map(|value| value == "true");
            }
            "statusDetail" => {
                self.pending_detail_code = Some(String::new());
                self.pending_detail_message = Some(String::new());
            }
            "searchRow" => {
                self.current_row = Some(Map::new());
                self.section = None;
                self.field_key = None;
                self.field_element_name = None;
                self.values.clear();
            }
            _ if self.current_row.is_some() => self.handle_row_child_open(name, element),
            _ => {}
        }
    }

    fn handle_row_child_open(&mut self, name: &str, element: &BytesStart) {
        if self.section.is_none() {
            self.section = Some(name.to_string());
        } else if self.field_element_name.is_none() {
            match name {
                "customFieldList" => {} // transparent wrapper, no state change
                "customField" => {
                    self.field_key = attr(element, "scriptId");
                    self.field_element_name = Some("customField".to_string());
                    self.values.clear();
                }
                _ => {
                    self.field_key = Some(name.to_string());
                    self.field_element_name = Some(name.to_string());
                    self.values.clear();
                }
            }
        } else if name == "searchValue" {
            self.search_value_attr_pushed = false;
            if let Some(internal_id) = attr(element, "internalId") {
                self.values.push(internal_id);
                self.search_value_attr_pushed = true;
            }
        }
    }

    fn handle_text(&mut self, value: &str) {
        if self.in_fault {
            match self.path.last().map(String::as_str) {
                Some("faultcode") => self.fault_code_buffer.push_str(value),
                Some("faultstring") => self.fault_string_buffer.push_str(value),
                _ => {}
            }
            return;
        }
        if self.pending_detail_code.is_some() {
            match self.path.last().map(String::as_str) {
                Some("code") => {
                    if let Some(buffer) = self.pending_detail_code.as_mut() {
                        buffer.push_str(value);
                    }
                }
                Some("message") => {
                    if let Some(buffer) = self.pending_detail_message.as_mut() {
                        buffer.push_str(value);
                    }
                }
                _ => {}
            }
            return;
        }
        if self.current_row.is_some() {
            if self.path.last().map(String::as_str) == Some("searchValue")
                && self.field_element_name.is_some()
                && !self.search_value_attr_pushed
            {
                self.values.push(value.to_string());
            }
            return;
        }
        match self.path.last().map(String::as_str) {
            Some("totalRecords") => self.result.total_records = value.parse().unwrap_or(0),
            Some("pageSize") => self.result.page_size = value.parse().unwrap_or(0),
            Some("totalPages") => self.result.total_pages = value.parse().unwrap_or(0),
            Some("pageIndex") => self.result.page_index = value.parse().unwrap_or(0),
            Some("searchId") => self.result.search_id = value.to_string(),
            _ => {}
        }
    }

    fn handle_close(&mut self, closed: &str) {
        if self.in_fault {
            if closed == "Fault" {
                self.fault = Some((
                    self.fault_code_buffer.clone(),
                    self.fault_string_buffer.clone(),
                ));
                self.in_fault = false;
            }
            return;
        }
        if closed == "statusDetail" && self.pending_detail_code.is_some() {
            let code = self.pending_detail_code.take().unwrap_or_default();
            let message = self.pending_detail_message.take().unwrap_or_default();
            self.status_details.push((code, message));
            return;
        }
        if self.current_row.is_some() {
            self.handle_row_child_close(closed);
        }
    }

    fn handle_row_child_close(&mut self, closed: &str) {
        if self.field_element_name.as_deref() == Some(closed) {
            self.commit_field();
        } else if self.section.as_deref() == Some(closed) && self.field_element_name.is_none() {
            self.section = None;
        } else if closed == "searchRow" {
            let row = self.current_row.take().unwrap_or_default();
            self.result.rows.push(Value::Object(row));
            self.section = None;
            self.field_key = None;
            self.field_element_name = None;
            self.values.clear();
        }
    }

    fn commit_field(&mut self) {
        let is_custom_field = self.field_element_name.as_deref() == Some("customField");
        let field_key = self.field_key.clone().unwrap_or_default();
        let key = if is_custom_field {
            field_key
        } else if let Some(join_prefix) = self
            .section
            .as_deref()
            .and_then(|section_name| section_name.strip_suffix("Join"))
        {
            format!("{join_prefix}.{field_key}")
        } else {
            field_key
        };
        let value = match self.values.len() {
            0 => Value::Null,
            1 => json!(self.values[0]),
            _ => json!(self.values),
        };
        if let Some(row) = self.current_row.as_mut() {
            row.insert(key, value);
        }
        self.field_key = None;
        self.field_element_name = None;
        self.values.clear();
    }

    fn finish(self) -> Result<SoapSearchResult, CliError> {
        if let Some((fault_code, fault_string)) = self.fault {
            let combined = format!("{fault_code} {fault_string}").to_lowercase();
            return if combined.contains("credential") || combined.contains("login") {
                Err(CliError::Auth(fault_string))
            } else {
                Err(CliError::Api {
                    status: 500,
                    message: fault_string,
                    details: vec![],
                })
            };
        }
        match self.is_success {
            Some(false) => {
                let is_auth_error = self.status_details.iter().any(|(code, _)| {
                    code.starts_with("INVALID_LOGIN") || code == "INSUFFICIENT_PERMISSION"
                });
                let message = self
                    .status_details
                    .iter()
                    .map(|(code, detail_message)| format!("{code}: {detail_message}"))
                    .collect::<Vec<_>>()
                    .join("; ");
                if is_auth_error {
                    Err(CliError::Auth(message))
                } else {
                    let details = self
                        .status_details
                        .iter()
                        .map(|(code, detail_message)| {
                            json!({"code": code, "message": detail_message})
                        })
                        .collect();
                    Err(CliError::Api {
                        status: 200,
                        message,
                        details,
                    })
                }
            }
            Some(true) => Ok(self.result),
            None => Err(CliError::Api {
                status: 0,
                message: "no searchResult in SOAP response".to_string(),
                details: vec![],
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HAPPY_RESPONSE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/">
 <soapenv:Body>
  <searchResponse xmlns="urn:messages_2025_2.platform.webservices.netsuite.com">
   <platformCore:searchResult xmlns:platformCore="urn:core_2025_2.platform.webservices.netsuite.com">
    <platformCore:status isSuccess="true"/>
    <platformCore:totalRecords>3</platformCore:totalRecords>
    <platformCore:pageSize>2</platformCore:pageSize>
    <platformCore:totalPages>2</platformCore:totalPages>
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

    #[test]
    fn parses_metadata_and_flattens_rows() {
        let result = parse_search_response(HAPPY_RESPONSE).unwrap();
        assert_eq!(result.total_records, 3);
        assert_eq!(result.total_pages, 2);
        assert_eq!(result.page_index, 1);
        assert_eq!(result.page_size, 2);
        assert_eq!(result.search_id, "WEBSERVICES_1234567_ABC");
        assert_eq!(result.rows.len(), 1);
        let row = &result.rows[0];
        assert_eq!(row["tranId"], "INV-1001");
        assert_eq!(row["entity"], "55"); // RecordRef internalId
        assert_eq!(row["otherRefNum"], serde_json::json!(["PO-1", "PO-2"]));
        assert_eq!(row["custbody_example"], "hello"); // custom field by scriptId
        assert_eq!(row["customer.email"], "a@example.com"); // join prefix, "Join" stripped
    }

    #[test]
    fn unsuccessful_status_maps_to_api_error_with_netsuite_code() {
        let xml = r#"<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/"><soapenv:Body>
      <searchResponse xmlns="urn:messages_2025_2.platform.webservices.netsuite.com">
       <searchResult xmlns="urn:core_2025_2.platform.webservices.netsuite.com">
        <status isSuccess="false">
         <statusDetail type="ERROR"><code>SSS_INVALID_SRCH_ID</code><message>That search or mass update does not exist.</message></statusDetail>
        </status>
       </searchResult>
      </searchResponse>
     </soapenv:Body></soapenv:Envelope>"#;
        let error = parse_search_response(xml).unwrap_err();
        match error {
            CliError::Api {
                status, message, ..
            } => {
                assert_eq!(status, 200);
                assert!(message.contains("SSS_INVALID_SRCH_ID"));
                assert!(message.contains("does not exist"));
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[test]
    fn invalid_login_status_maps_to_auth_error() {
        let xml = r#"<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/"><soapenv:Body>
      <searchResponse xmlns="urn:messages_2025_2.platform.webservices.netsuite.com">
       <searchResult xmlns="urn:core_2025_2.platform.webservices.netsuite.com">
        <status isSuccess="false">
         <statusDetail type="ERROR"><code>INVALID_LOGIN_CREDENTIALS</code><message>Invalid login attempt.</message></statusDetail>
        </status>
       </searchResult>
      </searchResponse>
     </soapenv:Body></soapenv:Envelope>"#;
        assert!(matches!(
            parse_search_response(xml).unwrap_err(),
            CliError::Auth(_)
        ));
    }

    #[test]
    fn soap_fault_maps_by_fault_kind() {
        let credential_fault = r#"<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/"><soapenv:Body>
      <soapenv:Fault><faultcode>soapenv:Server.userException</faultcode>
       <faultstring>com.netledger.common.exceptions.InvalidCredentialsException: Invalid login attempt.</faultstring>
      </soapenv:Fault></soapenv:Body></soapenv:Envelope>"#;
        assert!(matches!(
            parse_search_response(credential_fault).unwrap_err(),
            CliError::Auth(_)
        ));

        let other_fault = r#"<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/"><soapenv:Body>
      <soapenv:Fault><faultcode>soapenv:Server</faultcode>
       <faultstring>An unexpected error occurred.</faultstring>
      </soapenv:Fault></soapenv:Body></soapenv:Envelope>"#;
        match parse_search_response(other_fault).unwrap_err() {
            CliError::Api {
                status, message, ..
            } => {
                assert_eq!(status, 500);
                assert!(message.contains("unexpected error"));
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[test]
    fn garbage_input_is_a_network_class_error() {
        assert!(matches!(
            parse_search_response("not xml at all").unwrap_err(),
            CliError::Api { .. } | CliError::Network(_)
        ));
    }
}
