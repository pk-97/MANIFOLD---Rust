// ──────────────────────────────────────
// PipelineProgress (port of PercussionPipelineProgressParser.cs)
// ──────────────────────────────────────

/// Port of Unity PipelineProgress struct.
#[derive(Debug, Clone, Default)]
pub struct PipelineProgress {
    pub progress01: f32,
    pub message: String,
    pub is_error: bool,
    pub has_progress: bool,
}

/// Port of Unity PercussionPipelineProgressParser.
/// Parses pipeline output lines into structured progress updates.
/// Supports the MANIFOLD_PROGRESS protocol and heuristic fallback for Demucs output.
pub struct PercussionPipelineProgressParser;

impl PercussionPipelineProgressParser {
    const PROGRESS_PREFIX: &'static str = "MANIFOLD_PROGRESS|";

    // Heuristic progress mapping constants.
    const DEMUCS_PROGRESS_MIN: f32 = 0.22;
    const DEMUCS_PROGRESS_MAX: f32 = 0.72;
    const DEMUCS_GENERIC_PROGRESS: f32 = 0.30;
    const WRITING_JSON_PROGRESS: f32 = 0.84;
    const FINALIZING_SUMMARY_PROGRESS: f32 = 0.87;
    const GATHERING_METADATA_PROGRESS: f32 = 0.80;
    const ERROR_PROGRESS: f32 = 0.18;

    pub fn parse_line(&self, raw_line: &str, is_stderr: bool) -> PipelineProgress {
        let mut result = PipelineProgress::default();

        if raw_line.trim().is_empty() {
            return result;
        }

        let line = raw_line.trim();

        if Self::try_parse_structured_progress(line, &mut result) {
            return result;
        }

        Self::try_parse_heuristic_progress(line, is_stderr, &mut result);
        result
    }

    fn try_parse_structured_progress(line: &str, result: &mut PipelineProgress) -> bool {
        if !line.starts_with(Self::PROGRESS_PREFIX) {
            return false;
        }

        let first = match line.find('|') {
            Some(i) => i,
            None => return false,
        };
        let second = match line[first + 1..].find('|') {
            Some(i) => first + 1 + i,
            None => return false,
        };

        if second <= first + 1 || second >= line.len() - 1 {
            return false;
        }

        let progress_token = line[first + 1..second].trim();
        let stage_message = line[second + 1..].trim();

        let progress01: f32 = match progress_token.parse() {
            Ok(v) => v,
            Err(_) => return false,
        };

        let stage_message = if stage_message.trim().is_empty() {
            "analysis running"
        } else {
            stage_message
        };

        result.progress01 = progress01.clamp(0.0, 1.0);
        result.message = stage_message.to_string();
        result.has_progress = true;
        true
    }

    fn try_parse_heuristic_progress(line: &str, is_stderr: bool, result: &mut PipelineProgress) {
        let lower = line.to_lowercase();

        if let Some(percent) = Self::try_extract_percent(&lower) {
            // Mathf.Lerp(DemucsProgressMin, DemucsProgressMax, percent / 100f)
            let t = (percent / 100.0).clamp(0.0, 1.0);
            result.progress01 = Self::DEMUCS_PROGRESS_MIN
                + (Self::DEMUCS_PROGRESS_MAX - Self::DEMUCS_PROGRESS_MIN) * t;
            result.message = "separating stems (Demucs)".to_string();
            result.has_progress = true;
            return;
        }

        if lower.contains("demucs") {
            result.progress01 = Self::DEMUCS_GENERIC_PROGRESS;
            result.message = "separating stems (Demucs)".to_string();
            result.has_progress = true;
            return;
        }

        if lower.starts_with("wrote ") || lower.contains(" events -> ") {
            result.progress01 = Self::WRITING_JSON_PROGRESS;
            result.message = "writing analysis JSON".to_string();
            result.has_progress = true;
            return;
        }

        if lower.contains("estimated bpm") || lower.contains("event counts") {
            result.progress01 = Self::FINALIZING_SUMMARY_PROGRESS;
            result.message = "finalizing analysis summary".to_string();
            result.has_progress = true;
            return;
        }

        if lower.contains("analysis source")
            || lower.contains("percussion profile")
            || lower.contains("bass profile")
        {
            result.progress01 = Self::GATHERING_METADATA_PROGRESS;
            result.message = "analysis complete, gathering metadata".to_string();
            result.has_progress = true;
            return;
        }

        if is_stderr && lower.starts_with("error") {
            result.progress01 = Self::ERROR_PROGRESS;
            result.message = "analysis backend reported an error".to_string();
            result.is_error = true;
            result.has_progress = true;
        }
    }

    fn try_extract_percent(line: &str) -> Option<f32> {
        if line.is_empty() {
            return None;
        }

        let chars: Vec<char> = line.chars().collect();
        let mut percent_idx = 0usize;

        loop {
            // Find next '%'
            let found = chars[percent_idx..].iter().position(|&c| c == '%');
            let found = match found {
                Some(i) => percent_idx + i,
                None => break,
            };

            if found > 0 {
                let mut token_start = found - 1;
                // Walk back over digits and '.'
                while token_start > 0
                    && (chars[token_start].is_ascii_digit() || chars[token_start] == '.')
                {
                    token_start -= 1;
                }
                if !chars[token_start].is_ascii_digit() && chars[token_start] != '.' {
                    token_start += 1;
                }

                if token_start < found {
                    let token: String = chars[token_start..found].iter().collect();
                    if let Ok(parsed) = token.parse::<f32>() {
                        return Some(parsed.clamp(0.0, 100.0));
                    }
                }
            }

            percent_idx = found + 1;
            if percent_idx >= chars.len() {
                break;
            }
        }

        None
    }
}
