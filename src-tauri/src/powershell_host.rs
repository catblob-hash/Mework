//! PowerShell setup for the two host surfaces that are *not* the `powershell`
//! tool: the interactive terminal and hook execution.
//!
//! The `powershell` tool matches Claude Code byte for byte, and Claude Code
//! prepends only three statements (`Out-File:Encoding`, `$OutputEncoding`, and
//! `$PSStyle.OutputRendering`). That is deliberately weaker than what these two
//! surfaces need, so they keep the stricter setup that lives here rather than
//! sharing the tool's script.
//!
//! Hooks are the clearer case. A hook receives UTF-8 JSON on stdin and its
//! stdout is parsed back as UTF-8 JSON. Windows PowerShell encodes a redirected
//! pipe with the OEM code page, so all three `[Console]` encodings have to be
//! pinned together: today the two errors cancel out — the hook reads UTF-8 as
//! GBK and writes it back as GBK — and fixing only the output side would turn
//! that accidental round-trip into visible corruption.
//!
//! The terminal is the other. It runs a real PTY the user reads directly, so
//! the file cmdlets' encoding defaults are part of the surface being offered
//! rather than of any tool contract.
//!
//! Nothing here widens the console. A hidden console is 120 columns and the
//! formatter pads every row of a table out to that width; widening it to
//! thousands of columns turns a seven-file `Get-ChildItem` into hundreds of
//! kilobytes of spaces, which is only tolerable with a trimmer on the capture
//! path. The shell tool no longer trims — Claude Code returns the bytes as they
//! came — so the widening it used to pay for went with it.

/// File cmdlets that read or write with an encoding default of their own.
///
/// Windows PowerShell 5.1 decodes a BOM-less file with the ANSI code page — so
/// `Get-Content` on a UTF-8 source turns every CJK line into mojibake, and an
/// invalid trailing byte pair swallows the newline after it, gluing the next
/// line onto the same one — while its writers disagree with each other:
/// `Set-Content` writes ANSI, `Out-File` and `>` write UTF-16, `Export-Csv`
/// writes ASCII. The list is enumerated rather than `*` because a cmdlet whose
/// `-Encoding` takes a `System.Text.Encoding` object (`Send-MailMessage`) would
/// warn on a string default.
pub(crate) const UTF8_CMDLETS: [&str; 7] = [
    "Get-Content",
    "Set-Content",
    "Add-Content",
    "Out-File",
    "Select-String",
    "Import-Csv",
    "Export-Csv",
];

/// The statements that make a PowerShell session speak UTF-8 on every side:
/// the two console pipes, text piped into a native command, and the file
/// cmdlets. The terminal runs them as lines of its bootstrap.
pub(crate) fn strict_text_defaults() -> Vec<String> {
    let cmdlets = UTF8_CMDLETS
        .iter()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(",");
    vec![
        "[Console]::InputEncoding=[System.Text.UTF8Encoding]::new($false)".to_owned(),
        "[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false)".to_owned(),
        "$OutputEncoding=[Console]::OutputEncoding".to_owned(),
        format!("{cmdlets}|ForEach-Object{{$PSDefaultParameterValues[\"${{_}}:Encoding\"]='utf8'}}"),
    ]
}

/// The command as a hook must receive it so its pipes carry UTF-8.
///
/// A newline rather than `;` keeps the preamble out of the excerpt PowerShell
/// prints under an error, which quotes the offending line verbatim. Only the
/// spawned argument is rewritten; the command recorded and shown to the user
/// stays the text the caller wrote.
///
/// `$PSStyle` and `$ProgressPreference` are here because a hook's stdout is
/// parsed, and neither ANSI colour nor a progress bar is valid JSON.
pub(crate) fn wrap_command(command: &str) -> String {
    let mut statements = strict_text_defaults();
    statements.push("if($null -ne $PSStyle){$PSStyle.OutputRendering='PlainText'}".to_owned());
    statements.push("$ProgressPreference='SilentlyContinue'".to_owned());
    format!("{}\n{command}", statements.join(";"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hooks exchange UTF-8 JSON in both directions, so all three console
    /// encodings must be pinned together — see the module comment for why
    /// fixing only the output half is worse than fixing neither.
    #[test]
    fn the_hook_wrapper_pins_every_console_encoding() {
        let wrapped = wrap_command("Write-Output 'hi'");
        assert!(wrapped.contains("[Console]::InputEncoding="));
        assert!(wrapped.contains("[Console]::OutputEncoding="));
        assert!(wrapped.contains("$OutputEncoding=[Console]::OutputEncoding"));
        // The caller's command stays on its own line so a PowerShell error
        // excerpt quotes the command rather than the preamble.
        assert_eq!(wrapped.lines().count(), 2);
        assert_eq!(wrapped.lines().nth(1), Some("Write-Output 'hi'"));
    }

    /// Widening the console is what made trailing-space trimming necessary, and
    /// nothing on this path trims. A hook that lists files must not receive
    /// hundreds of kilobytes of padding it then has to parse as JSON.
    #[test]
    fn the_hook_wrapper_does_not_widen_the_console() {
        let wrapped = wrap_command("Get-ChildItem");
        assert!(!wrapped.contains("BufferSize"));
    }

    /// Every file cmdlet is named explicitly. A `*` default would warn on the
    /// cmdlets whose `-Encoding` is typed as `System.Text.Encoding`.
    #[test]
    fn the_file_cmdlet_defaults_are_enumerated_rather_than_wildcarded() {
        let defaults = strict_text_defaults().join(";");
        for cmdlet in UTF8_CMDLETS {
            assert!(defaults.contains(&format!("'{cmdlet}'")), "{cmdlet}");
        }
        assert!(!defaults.contains("'*'"));
    }
}
