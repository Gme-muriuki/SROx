use crate::errors::codec_error::CodecError;
use bytes::{Bytes, BytesMut};
use httparse::Request;

#[derive(Debug)]
pub struct ParsedRequest {
    pub method: String,
    pub body: Bytes,
    pub path: String,
    pub version: u8,
    pub headers: Vec<(String, String)>,
}

pub fn parse_request(buf: &BytesMut) -> Result<ParsedRequest, CodecError> {
    if buf.len() > 8 * 1024 {
        return Err(CodecError::RequestTooLarge);
    }

    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut req = Request::new(&mut headers);

    match req.parse(buf) {
        Ok(body_offset) => {
            let version = req.version.ok_or(CodecError::InvalidRequest)?;
            validate_version(version)?;
            validate_framing(req.headers)?;

            let headers = req
                .headers
                .iter()
                .map(|head| {
                    (
                        head.name.to_string(),
                        String::from_utf8_lossy(head.value).to_string(),
                    )
                })
                .collect::<Vec<_>>();

            let len = match body_offset {
                httparse::Status::Complete(len) => len,
                httparse::Status::Partial => 0,
            };

            let body = Bytes::copy_from_slice(&buf[len..]);

            Ok(ParsedRequest {
                method: req.method.ok_or(CodecError::InvalidRequest)?.to_string(),
                body,
                path: req.path.ok_or(CodecError::InvalidRequest)?.to_string(),
                version,
                headers,
            })
        }
        Err(err) => {
            tracing::error!(error = %err, "failed to parse request");
            Err(CodecError::InvalidRequest)
        }
    }
}

pub(self) fn validate_framing(headers: &[httparse::Header]) -> Result<(), CodecError> {
    let has_content_length = headers
        .iter()
        .any(|head| head.name.eq_ignore_ascii_case("content-length"));
    let has_transfer_encoding = headers
        .iter()
        .any(|head| head.name.eq_ignore_ascii_case("transfer-encoding"));

    if has_content_length && has_transfer_encoding {
        return Err(CodecError::AmbiguousFraming);
    }

    Ok(())
}

pub(self) fn validate_version(version: u8) -> Result<(), CodecError> {
    if version != 1 {
        return Err(CodecError::InvalidHttpVersion(format!(
            "HTTP/{}.x",
            version
        )));
    }

    Ok(())
}
