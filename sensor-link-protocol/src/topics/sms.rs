use super::TopicPayloadSerialize;
use crate::MAX_MESSAGE_LEN;
use serde::{Deserialize, Serialize};

/// Request to send an SMS
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct SMSRequest {
    /// unique ID of the SMS request for tracking
    pub id: String,
    /// List of phone numbers to send the SMS to
    pub phone_numbers: Vec<String>,
    /// SMS message content
    pub message: String,
}

/// SMS Send Result codes
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone, Copy)]
pub enum SMSResult {
    Success,
    Fail,
}

/// Result of sending an SMS
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct SMSResponse {
    /// unique ID of the SMS request for tracking
    pub id: String,

    /// Result code
    pub result: SMSResult,

    /// List of phone numbers that failed to send the SMS to
    pub failed_phone_numbers: Vec<String>,
}

impl SMSResponse {
    pub fn success(id: String) -> Self {
        SMSResponse {
            id,
            result: SMSResult::Success,
            failed_phone_numbers: Vec::new(),
        }
    }

    pub fn fail(id: String, phone_numbers: Vec<String>) -> Self {
        SMSResponse {
            id,
            result: SMSResult::Fail,
            failed_phone_numbers: phone_numbers,
        }
    }
}

pub fn parse_request<'a>(bytes: &'a [u8]) -> Option<SMSRequest> {
    serde_json::from_slice::<SMSRequest>(bytes).ok()
}

pub fn format_request(req: &SMSRequest) -> Result<String, ()> {
    serde_json::to_string(req).map_err(|_| ())
}

pub fn format_response(resp: &SMSResponse) -> Result<String, ()> {
    serde_json::to_string(resp).map_err(|_| ())
}

pub fn parse_response<'a>(bytes: &'a [u8]) -> Option<SMSResponse> {
    serde_json::from_slice::<SMSResponse>(bytes).ok()
}

impl TopicPayloadSerialize<MAX_MESSAGE_LEN> for SMSRequest {}

impl TopicPayloadSerialize<MAX_MESSAGE_LEN> for SMSResponse {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_deserialize_sms_types() {
        let req = SMSRequest {
            id: String::from("123"),
            phone_numbers: vec![String::from("123"), String::from("456")],
            message: String::from("test"),
        };

        let s = format_request(&req).unwrap();

        println!("serialized: {}", s);

        let parsed = parse_request(s.as_bytes()).unwrap();

        assert_eq!(req, parsed);
    }

    #[test]
    fn parse_sms_response() {
        let s = String::from("{\"id\":\"671b5b7c550467140a52c2bf\",\"result\":\"Success\",\"failed_phone_numbers\":[]}");
        let resp = parse_response(s.as_bytes());
        assert!(resp.is_some());
    }
}
