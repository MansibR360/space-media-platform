use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::error::ApiError;
use crate::streaming::storage::{ByteStream, FileStat, content_type_for};

#[derive(Debug, PartialEq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

pub fn parse_range(header: &str, size: u64) -> Option<ByteRange> {
    let spec = header.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }

    let (start_raw, end_raw) = match spec.split_once('-') {
        Some(parts) => parts,
        None => return None,
    };

    let range = if start_raw.is_empty() {
        let suffix: u64 = end_raw.parse().ok()?;
        let start = size.checked_sub(suffix)?;
        ByteRange {
            start,
            end: size.saturating_sub(1),
        }
    } else {
        let start: u64 = start_raw.trim().parse().ok()?;
        let end = if end_raw.is_empty() {
            size.saturating_sub(1)
        } else {
            end_raw
                .trim()
                .parse::<u64>()
                .ok()?
                .min(size.saturating_sub(1))
        };
        if start > end || start >= size && size > 0 && end < start {
            return None;
        }
        ByteRange { start, end }
    };

    if range.start >= size {
        return None;
    }
    Some(range)
}

pub fn range_response(
    path: &std::path::Path,
    stat: &FileStat,
    range_header: Option<&str>,
    stream: ByteStream,
) -> Result<Response, ApiError> {
    let content_type = content_type_for(path);
    let mut headers = HeaderMap::new();
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));

    if stat.size == 0 {
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
        return Ok((StatusCode::OK, headers).into_response());
    }

    match range_header.map(str::trim) {
        Some(spec) => match parse_range(spec, stat.size) {
            Some(range) => {
                let length = range.end - range.start + 1;
                headers.insert(
                    header::CONTENT_RANGE,
                    HeaderValue::from_str(&format!(
                        "bytes {}-{}/{}",
                        range.start, range.end, stat.size
                    ))
                    .map_err(|_| ApiError::internal_msg("invalid content range"))?,
                );
                headers.insert(
                    header::CONTENT_LENGTH,
                    HeaderValue::from_str(&length.to_string())
                        .map_err(|_| ApiError::internal_msg("invalid length"))?,
                );
                Ok((
                    StatusCode::PARTIAL_CONTENT,
                    headers,
                    Body::from_stream(stream),
                )
                    .into_response())
            }
            None => {
                headers.insert(
                    header::CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes */{}", stat.size))
                        .map_err(|_| ApiError::internal_msg("invalid content range"))?,
                );
                Ok((StatusCode::RANGE_NOT_SATISFIABLE, headers).into_response())
            }
        },
        None => {
            headers.insert(
                header::CONTENT_LENGTH,
                HeaderValue::from_str(&stat.size.to_string())
                    .map_err(|_| ApiError::internal_msg("invalid size"))?,
            );
            Ok((StatusCode::OK, headers, Body::from_stream(stream)).into_response())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_ranges() {
        assert_eq!(
            parse_range("bytes=0-499", 1000),
            Some(ByteRange { start: 0, end: 499 })
        );
        assert_eq!(
            parse_range("bytes=500-", 1000),
            Some(ByteRange {
                start: 500,
                end: 999
            })
        );
        assert_eq!(
            parse_range("bytes=990-2000", 1000),
            Some(ByteRange {
                start: 990,
                end: 999
            })
        );
        assert_eq!(
            parse_range("bytes=-100", 1000),
            Some(ByteRange {
                start: 900,
                end: 999
            })
        );
    }

    #[test]
    fn rejects_invalid_ranges() {
        assert_eq!(parse_range("bytes=1000-", 1000), None);
        assert_eq!(parse_range("bytes=a-b", 1000), None);
        assert_eq!(parse_range("bytes=0-1,5-9", 1000), None);
        assert_eq!(parse_range("chunks=0-1", 1000), None);
    }

    #[test]
    fn suffix_on_empty_file() {
        assert_eq!(parse_range("bytes=-10", 0), None);
    }
}
