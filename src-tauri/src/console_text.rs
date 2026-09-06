//! Bytes a child process wrote to a pipe, as text.
//!
//! A shell command's output has no declared encoding. Git Bash and most modern
//! tools write UTF-8, but a Windows PowerShell 5.1 or a native program started
//! under `CREATE_NO_WINDOW` gets a hidden console whose code page is the OEM
//! page — 936 on a Chinese system — and writes GBK, and both can appear in the
//! same stream (`echo` of a UTF-8 path followed by `ipconfig`). Decoding all of
//! it as UTF-8 turned every such line into U+FFFD, for the model and for the
//! task page alike.
//!
//! The decision is made per line rather than per byte: a GB2312 character is a
//! valid two-byte UTF-8 sequence about one time in seven (lead C2–DF, trail
//! A1–BF), so a byte-level split would read those as Hebrew or Armenian. A
//! line that is valid UTF-8 as a whole is UTF-8; one that is not is tried as
//! the system ANSI code page strictly, and only then replaced lossily.
//!
//! The streaming decoder never holds a partial line back — a progress bar or a
//! prompt has no newline and must still show — and only carries the few bytes
//! that can be the start of a character cut by a chunk boundary.

#[derive(Clone, Debug)]
pub struct ConsoleTextDecoder {
    ansi_code_page: Option<u32>,
    carry: Vec<u8>,
}

impl ConsoleTextDecoder {
    pub fn new() -> Self {
        Self::with_ansi_code_page(system_ansi_code_page())
    }

    pub fn with_ansi_code_page(ansi_code_page: Option<u32>) -> Self {
        Self {
            ansi_code_page,
            carry: Vec::new(),
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> String {
        if bytes.is_empty() && self.carry.is_empty() {
            return String::new();
        }

        let mut input = std::mem::take(&mut self.carry);
        input.extend_from_slice(bytes);
        let mut output = String::new();
        let mut start = 0;

        for (index, byte) in input.iter().enumerate() {
            if *byte == b'\n' {
                output.push_str(&decode_complete_line(
                    &input[start..=index],
                    self.ansi_code_page,
                ));
                start = index + 1;
            }
        }

        if start < input.len() {
            output.push_str(&self.decode_partial_line(&input[start..]));
        }
        output
    }

    pub fn finish(&mut self) -> String {
        let carry = std::mem::take(&mut self.carry);
        decode_complete_line(&carry, self.ansi_code_page)
    }

    fn decode_partial_line(&mut self, bytes: &[u8]) -> String {
        match std::str::from_utf8(bytes) {
            Ok(text) => text.to_owned(),
            Err(error) if error.error_len().is_none() => {
                let valid_up_to = error.valid_up_to();
                self.carry.extend_from_slice(&bytes[valid_up_to..]);
                String::from_utf8_lossy(&bytes[..valid_up_to]).into_owned()
            }
            Err(_) => {
                #[cfg(windows)]
                if let Some(code_page) = self.ansi_code_page {
                    let decoded_end =
                        dangling_dbcs_lead_start(code_page, bytes).unwrap_or(bytes.len());
                    self.carry.extend_from_slice(&bytes[decoded_end..]);
                    return decode_ansi(
                        bytes.get(..decoded_end).unwrap_or_default(),
                        code_page,
                        false,
                    )
                    .unwrap_or_else(|| {
                        String::from_utf8_lossy(bytes.get(..decoded_end).unwrap_or_default())
                            .into_owned()
                    });
                }

                let decoded_end = incomplete_utf8_suffix_start(bytes).unwrap_or(bytes.len());
                self.carry.extend_from_slice(&bytes[decoded_end..]);
                String::from_utf8_lossy(bytes.get(..decoded_end).unwrap_or_default()).into_owned()
            }
        }
    }
}

impl Default for ConsoleTextDecoder {
    fn default() -> Self {
        Self::new()
    }
}

pub fn decode_console_text(bytes: &[u8]) -> String {
    let mut decoder = ConsoleTextDecoder::new();
    let mut text = decoder.push(bytes);
    text.push_str(&decoder.finish());
    text
}

pub fn system_ansi_code_page() -> Option<u32> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Globalization::GetACP;

        // GetACP takes no pointers and has no caller-maintained invariants.
        let code_page = unsafe { GetACP() };
        return (code_page != 65001).then_some(code_page);
    }

    #[cfg(not(windows))]
    {
        None
    }
}

fn decode_complete_line(bytes: &[u8], ansi_code_page: Option<u32>) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    #[cfg(windows)]
    if let Some(text) = ansi_code_page.and_then(|code_page| decode_ansi(bytes, code_page, true)) {
        return text;
    }
    #[cfg(not(windows))]
    let _ = ansi_code_page;
    String::from_utf8_lossy(bytes).into_owned()
}

fn incomplete_utf8_suffix_start(bytes: &[u8]) -> Option<usize> {
    let search_start = bytes.len().saturating_sub(3);
    for start in search_start..bytes.len() {
        let expected = match bytes[start] {
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => continue,
        };
        let suffix = &bytes[start..];
        if suffix.len() < expected && suffix[1..].iter().all(|byte| (byte & 0xC0) == 0x80) {
            return Some(start);
        }
    }
    None
}

#[cfg(windows)]
fn dangling_dbcs_lead_start(code_page: u32, bytes: &[u8]) -> Option<usize> {
    use windows_sys::Win32::Globalization::IsDBCSLeadByteEx;

    let mut index = 0;
    while index < bytes.len() {
        // The byte is read from `bytes` at a checked index; the API only classifies its value.
        let lead = unsafe { IsDBCSLeadByteEx(code_page, bytes[index]) } != 0;
        if lead {
            if index + 1 == bytes.len() {
                return Some(index);
            }
            index += 2;
        } else {
            index += 1;
        }
    }
    None
}

#[cfg(windows)]
fn decode_ansi(bytes: &[u8], code_page: u32, strict: bool) -> Option<String> {
    use std::ptr::null_mut;
    use windows_sys::Win32::Globalization::{MultiByteToWideChar, MB_ERR_INVALID_CHARS};

    if bytes.is_empty() {
        return Some(String::new());
    }
    let byte_count = i32::try_from(bytes.len()).ok()?;
    let flags = if strict { MB_ERR_INVALID_CHARS } else { 0 };
    // `bytes` is valid for `byte_count` reads; a null output asks Windows for the required length.
    let wide_count =
        unsafe { MultiByteToWideChar(code_page, flags, bytes.as_ptr(), byte_count, null_mut(), 0) };
    if wide_count == 0 {
        return None;
    }
    let mut wide = vec![0_u16; wide_count as usize];
    // `wide` has exactly the capacity returned by the size query and both input/output buffers stay
    // alive for the duration of this call.
    let written = unsafe {
        MultiByteToWideChar(
            code_page,
            flags,
            bytes.as_ptr(),
            byte_count,
            wide.as_mut_ptr(),
            wide_count,
        )
    };
    if written == 0 {
        return None;
    }
    wide.truncate(written as usize);
    Some(String::from_utf16_lossy(&wide))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_utf8_passes_through_unchanged() {
        let mut decoder = ConsoleTextDecoder::with_ansi_code_page(None);
        assert_eq!(
            decoder.push("中文测试\npartial".as_bytes()),
            "中文测试\npartial"
        );
        assert_eq!(decoder.finish(), "");
    }

    #[cfg(windows)]
    #[test]
    fn gbk_line_decodes_to_unicode() {
        let mut decoder = ConsoleTextDecoder::with_ansi_code_page(Some(936));
        assert_eq!(
            decoder.push(&[0xD6, 0xD0, 0xCE, 0xC4, 0xB2, 0xE2, 0xCA, 0xD4, b'\n']),
            "中文测试\n"
        );
    }

    #[cfg(windows)]
    #[test]
    fn utf8_and_gbk_lines_can_share_one_stream() {
        let mut decoder = ConsoleTextDecoder::with_ansi_code_page(Some(936));
        let mut bytes = "UTF-8 中文\n".as_bytes().to_vec();
        bytes.extend_from_slice(&[0xD6, 0xD0, 0xCE, 0xC4, b'\n']);
        assert_eq!(decoder.push(&bytes), "UTF-8 中文\n中文\n");
    }

    #[test]
    fn split_utf8_character_is_carried_between_pushes() {
        let mut decoder = ConsoleTextDecoder::with_ansi_code_page(None);
        assert_eq!(decoder.push(&[0xE4, 0xB8]), "");
        assert_eq!(decoder.push(&[0xAD]), "中");
    }

    #[cfg(windows)]
    #[test]
    fn split_gbk_pair_is_carried_between_pushes() {
        let mut decoder = ConsoleTextDecoder::with_ansi_code_page(Some(936));
        assert_eq!(decoder.push(&[0xD6]), "");
        assert_eq!(decoder.push(&[0xD0]), "中");
    }

    #[test]
    fn partial_line_without_newline_is_emitted_immediately() {
        let mut decoder = ConsoleTextDecoder::with_ansi_code_page(None);
        assert_eq!(decoder.push(b"progress 10%"), "progress 10%");
    }

    #[test]
    fn finish_replaces_an_incomplete_tail() {
        let mut decoder = ConsoleTextDecoder::with_ansi_code_page(None);
        assert_eq!(decoder.push(&[0xE4, 0xB8]), "");
        assert_eq!(decoder.finish(), "�");
        assert_eq!(decoder.push(b"reused"), "reused");
    }

    #[test]
    fn invalid_bytes_without_a_code_page_are_lossy() {
        let mut decoder = ConsoleTextDecoder::with_ansi_code_page(None);
        assert_eq!(decoder.push(&[0xFF]), "�");
    }

    #[test]
    fn decoding_empty_input_is_empty() {
        assert_eq!(decode_console_text(b""), "");
    }
}
