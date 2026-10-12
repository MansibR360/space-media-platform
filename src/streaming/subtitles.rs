use crate::error::{ApiError, ApiResult};

pub enum SubtitleFormat {
    Srt,
    Ass,
    Vtt,
}

impl SubtitleFormat {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "srt" => Some(Self::Srt),
            "ass" | "ssa" => Some(Self::Ass),
            "vtt" => Some(Self::Vtt),
            _ => None,
        }
    }
}

pub fn srt_to_vtt(srt: &str) -> String {
    let mut out = String::with_capacity(srt.len() + 8);
    out.push_str("WEBVTT\n\n");

    for block in srt.split("\r\n\r\n").flat_map(|b| b.split("\n\n")) {
        let mut lines = block.lines().peekable();

        if let Some(first) = lines.peek() {
            let first_trimmed = first.trim();
            if first_trimmed.len() <= 6 && first_trimmed.chars().all(|c| c.is_ascii_digit()) {
                lines.next();
            }
        }

        for line in lines {
            if line.contains("-->") {
                out.push_str(&line.replace(',', "."));
            } else {
                out.push_str(line);
            }
            out.push('\n');
        }
        out.push('\n');
    }

    out
}

pub fn convert(content: &str, from: SubtitleFormat) -> ApiResult<String> {
    match from {
        SubtitleFormat::Vtt | SubtitleFormat::Ass => {
            if content.starts_with("WEBVTT") || content.contains("[Script Info]") {
                Ok(content.to_string())
            } else {
                Ok(srt_to_vtt(content))
            }
        }
        SubtitleFormat::Srt => Ok(srt_to_vtt(content)),
    }
}

#[allow(dead_code)]
pub fn validate_srt(input: &str) -> ApiResult<()> {
    if !input.contains("-->") && !input.is_empty() {
        return Err(ApiError::bad_request("not a valid SRT subtitle"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "1\n00:00:01,000 --> 00:00:04,500\nHello there\n\n2\n00:00:05,000 --> 00:00:07,250\nGeneral Kenobi!\n";

    #[test]
    fn converts_timestamps_and_header() {
        let vtt = srt_to_vtt(SAMPLE);
        assert!(vtt.starts_with("WEBVTT"));
        assert!(vtt.contains("00:00:01.000 --> 00:00:04.500"));
        assert!(vtt.contains("General Kenobi!"));
        assert!(!vtt.contains(','));
    }

    #[test]
    fn handles_crlf_blocks() {
        let crlf = SAMPLE.replace('\n', "\r\n");
        let vtt = srt_to_vtt(&crlf);
        assert!(vtt.contains("Hello there"));
        assert!(vtt.contains("WEBVTT"));
    }

    #[test]
    fn empty_input_yields_header_only() {
        let vtt = srt_to_vtt("");
        assert_eq!(vtt.trim(), "WEBVTT");
    }
}
