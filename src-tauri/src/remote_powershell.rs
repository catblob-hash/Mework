//! The remote file tools', language servers' and Git status probe's scripts,
//! in PowerShell.
//!
//! A Windows machine whose agent shell is PowerShell runs these where a POSIX
//! machine runs the `sh` scripts in [`crate::remote_files`] and
//! [`crate::remote_lsp`]. They keep exactly the same contract, so everything
//! the host does with an answer is shared: the same two header lines (the
//! canonical root, then the canonical target), the same payloads byte for
//! byte, and the same reserved exit codes (64 root missing, 65 outside the
//! root, 66 not found, 67 wrong kind, 68 too large, 69 changed since the probe,
//! 70 write failed, 2 bad pattern, 127 language server not found).
//!
//! What differs is only how a Windows machine is asked:
//!
//! * **Paths** come back as `C:/Users/…` — forward slashes, drive letter
//!   first — which is what the rest of the host already reads a Windows path
//!   as (`lsp_servers::path_to_uri` turns it into `file:///C:/…`). A path the
//!   model or an older workspace record spells the Git Bash way (`/c/…`,
//!   `/cygdrive/c/…`) or with `~` is read as the Windows path it names.
//! * **Canonical** means every symbolic link and junction resolved, walking the
//!   path one component at a time, because confinement has to be decided on
//!   the real location, as it is on POSIX. Case is compared the way NTFS
//!   compares it: insensitively.
//! * **Output** is written as bytes to the standard output stream, never
//!   through PowerShell's formatter, so a file's bytes arrive as they are on
//!   disk and text lines are UTF-8 whatever the console code page.
//! * **Input** (the bytes a write puts in place) is read from the raw input
//!   handle: Windows PowerShell's `[Console]::OpenStandardInput()` never
//!   returns when the input is already waiting as it starts to read, the same
//!   trap the agent upload avoids.
//! * **Regular expressions** are .NET's, which, like the host leg's Rust
//!   `regex`, are Perl-shaped.
//!
//! PowerShell names are case-insensitive: `$c` and `$C` are one variable, so
//! a loop variable at a script's top level must not share a name with the
//! prologue's `$ROOT`, `$REQ`, `$T` and `$C` in any case.
//!
//! Every host-supplied operand is a single-quoted PowerShell literal
//! ([`crate::remote_shell::ps_single_quote`]), refused first when it holds a
//! control character, so nothing a model sends can become code.

use crate::remote_shell::ps_single_quote;

/// Where a script acts: the workspace root as recorded, and whether a path
/// outside it is refused.
pub(crate) struct Target<'a> {
    pub root: &'a str,
    pub confine: bool,
}

/// Whether a script's target must already exist.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Existing,
    ForWrite,
}

/// Helpers every script starts with: strict errors, UTF-8 byte output, the
/// path spellings a Windows machine may be named with, and canonicalization.
const HELPERS: &str = r#"$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
try { [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false) } catch {}
$MeworkOut = [Console]::OpenStandardOutput()
function Out-Line([string]$Text) { $b = [System.Text.Encoding]::UTF8.GetBytes($Text + "`n"); $MeworkOut.Write($b, 0, $b.Length) }
function Out-Bytes([byte[]]$Bytes) { if ($Bytes.Length -gt 0) { $MeworkOut.Write($Bytes, 0, $Bytes.Length) } }
function Out-Err([string]$Text) { [Console]::Error.WriteLine($Text) }
function Quit([int]$Code) { $MeworkOut.Flush(); exit $Code }
function Slash([string]$P) { $P.Replace('\', '/') }
function Native-Path([string]$P) {
  if ($P -eq '~') { return $HOME }
  if ($P.StartsWith('~/') -or $P.StartsWith('~\')) { return [System.IO.Path]::Combine($HOME, $P.Substring(2)) }
  if ($P -match '^/cygdrive/([A-Za-z])(/.*)?$' -or $P -match '^/([A-Za-z])(/.*)?$') {
    $rest = if ($Matches[2]) { $Matches[2] } else { '/' }
    return $Matches[1].ToUpper() + ':' + $rest
  }
  return $P
}
function Canon([string]$P) {
  $full = [System.IO.Path]::GetFullPath($P)
  for ($hop = 0; $hop -lt 40; $hop++) {
    $root = [System.IO.Path]::GetPathRoot($full)
    $parts = @($full.Substring($root.Length).Split([char[]]@('\', '/'), [System.StringSplitOptions]::RemoveEmptyEntries))
    $cur = $root
    $restart = $null
    for ($i = 0; $i -lt $parts.Count; $i++) {
      $next = [System.IO.Path]::Combine($cur, $parts[$i])
      $item = Get-Item -LiteralPath $next -Force -ErrorAction SilentlyContinue
      if ($null -eq $item) {
        $cur = $next
        for ($j = $i + 1; $j -lt $parts.Count; $j++) { $cur = [System.IO.Path]::Combine($cur, $parts[$j]) }
        break
      }
      if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -and $item.Target) {
        $link = [string](@($item.Target)[0])
        if (-not [System.IO.Path]::IsPathRooted($link)) { $link = [System.IO.Path]::Combine($cur, $link) }
        for ($j = $i + 1; $j -lt $parts.Count; $j++) { $link = [System.IO.Path]::Combine($link, $parts[$j]) }
        $restart = [System.IO.Path]::GetFullPath($link)
        break
      }
      $cur = $next
    }
    if ($null -eq $restart) {
      $out = (Slash $cur).TrimEnd('/')
      if ($out -match '^[A-Za-z]:$') { $out += '/' }
      return $out
    }
    $full = $restart
  }
  return $null
}
function Inside([string]$Path, [string]$Root) {
  if ($Path -ieq $Root) { return $true }
  return $Path.StartsWith($Root.TrimEnd('/') + '/', [System.StringComparison]::OrdinalIgnoreCase)
}
function Unix-Seconds([System.IO.FileInfo]$File) { return ([DateTimeOffset]$File.LastWriteTimeUtc).ToUnixTimeSeconds() }
function Sha256-Hex([byte[]]$Bytes) {
  $sha = [System.Security.Cryptography.SHA256]::Create()
  try { return ([System.BitConverter]::ToString($sha.ComputeHash($Bytes))).Replace('-', '').ToLowerInvariant() } finally { $sha.Dispose() }
}
"#;

/// Enters the root, or exits 64. `~` and the Git Bash spellings are read as
/// the Windows paths they name.
fn enter_root(root: &str) -> String {
    format!(
        "try {{ Set-Location -LiteralPath (Native-Path {}) }} catch {{ Out-Err $_.Exception.Message; Quit 64 }}\n",
        ps_single_quote(root.trim())
    )
}

/// The shared prologue: enter the root, resolve and canonicalize the requested
/// path, test confinement, then announce both — the POSIX prologue's contract.
pub(crate) fn prologue(target: &Target<'_>, path: &str, mode: Mode) -> String {
    let mut script = String::with_capacity(HELPERS.len() + 1024);
    script.push_str(HELPERS);
    script.push_str(&enter_root(target.root));
    script.push_str("$ROOT = Canon (Get-Location).ProviderPath\nif (-not $ROOT) { Quit 64 }\n");
    script.push_str(&format!("$REQ = {}\n", ps_single_quote(path)));
    script.push_str(
        "$T = Native-Path $REQ\n\
         if (-not [System.IO.Path]::IsPathRooted($T)) { $T = [System.IO.Path]::Combine($ROOT, $T) }\n",
    );
    if mode == Mode::Existing {
        script.push_str(
            "try { $there = [System.IO.File]::Exists($T) -or [System.IO.Directory]::Exists($T) } catch { $there = $false }\n\
             if (-not $there) { Quit 66 }\n",
        );
    }
    script.push_str("try { $C = Canon $T } catch { Out-Err $_.Exception.Message; Quit 66 }\nif (-not $C) { Quit 66 }\n");
    if target.confine {
        script.push_str("if (-not (Inside $C $ROOT)) { Out-Err $C; Quit 65 }\n");
    }
    script.push_str("Out-Line $ROOT\nOut-Line $C\n");
    script
}

/// What Git says about the target, the PowerShell form of the POSIX
/// `IGNORE_PROBE`: `$Ign` becomes `git`, `root` or `names`
/// ([`crate::search_scope::take_remote_rules`]). With `list`, `$Ignored`
/// holds what `git ls-files` lists as ignored below the target.
///
/// Windows PowerShell turns a native command's redirected stderr into error
/// records, which `$ErrorActionPreference = 'Stop'` would make fatal, so the
/// preference is relaxed around the calls. Every argument is a fixed token:
/// the target is the current location, never an argument, because 5.1
/// re-splits what it hands a native program on spaces and quotes.
fn ignore_probe(list: bool) -> String {
    let listing = if list {
        "\n      $MeworkLines = & git -c core.fsmonitor=false -c core.quotepath=false ls-files --others --ignored --exclude-standard --directory -- . 2>$null\n      if ($LASTEXITCODE -eq 0) { $Ignored = @($MeworkLines | Where-Object { $_ }) } else { $Ign = 'names'; $Ignored = @() }"
    } else {
        ""
    };
    format!(
        r#"$Ign = 'names'
$Ignored = @()
if ([System.IO.Directory]::Exists($C) -and (Get-Command git -CommandType Application -ErrorAction SilentlyContinue)) {{
  $MeworkEap = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  foreach ($MeworkVar in @('GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_COMMON_DIR', 'GIT_OBJECT_DIRECTORY', 'GIT_ALTERNATE_OBJECT_DIRECTORIES', 'GIT_NAMESPACE', 'GIT_CEILING_DIRECTORIES', 'GIT_CONFIG', 'GIT_CONFIG_PARAMETERS', 'GIT_CONFIG_COUNT')) {{ Remove-Item -LiteralPath ('Env:' + $MeworkVar) -ErrorAction SilentlyContinue }}
  $env:GIT_OPTIONAL_LOCKS = '0'
  Push-Location -LiteralPath $C
  try {{
    & git -c core.fsmonitor=false check-ignore -q . 2>$null | Out-Null
    if ($LASTEXITCODE -eq 0) {{ $Ign = 'root' }}
    elseif ($LASTEXITCODE -eq 1) {{
      $Ign = 'git'{listing}
    }}
  }} catch {{ $Ign = 'names'; $Ignored = @() }} finally {{ Pop-Location; $ErrorActionPreference = $MeworkEap }}
}}
"#
    )
}

/// The ignore section of an `ls` or `find` answer: the mode, the ignored
/// entries, an empty line.
const IGNORE_SECTION: &str = "Out-Line $Ign\nforeach ($MeworkEntry in $Ignored) { Out-Line $MeworkEntry }\nOut-Line ''\n";

/// `Collapsed $Shown $Name`: whether the walk leaves a directory unexpanded —
/// version-control metadata always, and what the ignore mode says. The
/// shown path is the one the walk prints, `$C` plus `/`-joined names.
fn collapsed_function() -> String {
    let quoted = |names: &[&str]| {
        names
            .iter()
            .map(|name| ps_single_quote(name))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        r#"$MeworkVcs = @({vcs})
$MeworkDeps = @({deps})
$MeworkPrune = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
foreach ($MeworkEntry in $Ignored) {{
  if ($MeworkEntry.EndsWith('/') -and -not $MeworkEntry.StartsWith('"')) {{ [void]$MeworkPrune.Add($C.TrimEnd('/') + '/' + $MeworkEntry.TrimEnd('/')) }}
}}
function Collapsed([string]$Shown, [string]$Name) {{
  if ($MeworkVcs -contains $Name) {{ return $true }}
  if ($Ign -eq 'names' -and $MeworkDeps -contains $Name) {{ return $true }}
  return $MeworkPrune.Contains($Shown)
}}
"#,
        vcs = quoted(&crate::search_scope::VCS_DIRECTORIES),
        deps = quoted(crate::search_scope::DEPENDENCY_DIRECTORIES),
    )
}

/// Walks a directory the way the POSIX leg's `find` does: pre-order, one entry
/// per line, directories marked with a trailing `/`, links to directories
/// neither marked nor descended into, version-control metadata listed but not
/// entered, at most `limit` lines.
///
/// `name_filter`, when given, is a `-like` pattern the printed entries' names
/// must match; every directory is still walked, as `find -name` walks them.
/// It is a pre-filter for the host's own glob matcher, and PowerShell's
/// `-like` ignores case, so it only ever lets more through, never less.
fn walk(limit: usize, name_filter: Option<&str>) -> String {
    let filter = match name_filter {
        Some(pattern) => format!(
            "($it.Name -like {})",
            ps_single_quote(&pattern.replace('`', "``"))
        ),
        None => "$true".to_owned(),
    };
    format!(
        r#"$global:MeworkLeft = {limit}
function Walk([string]$Dir, [string]$Shown) {{
  try {{ $items = ([System.IO.DirectoryInfo]::new($Dir)).GetFileSystemInfos() }} catch {{ return }}
  foreach ($it in $items) {{
    if ($global:MeworkLeft -le 0) {{ return }}
    $path = $Shown + '/' + $it.Name
    $isDir = ($it -is [System.IO.DirectoryInfo]) -and -not ($it.Attributes -band [System.IO.FileAttributes]::ReparsePoint)
    if ({filter}) {{
      if ($isDir) {{ Out-Line ($path + '/') }} else {{ Out-Line $path }}
      $global:MeworkLeft--
    }}
    if ($isDir -and -not ($MeworkVcs -contains $it.Name)) {{ Walk $it.FullName $path }}
  }}
}}
Walk $C $C.TrimEnd('/')
Quit 0
"#
    )
}

/// `ls`: the entries under the target, `max_depth` levels deep, breadth-first
/// and each directory in name order, at most `limit` lines — so the cut falls
/// on the deepest level reached, as on the host. Ignored directories are
/// printed but not entered.
pub(crate) fn listing(target: &Target<'_>, path: &str, max_depth: u64, limit: usize) -> String {
    let mut script = prologue(target, path, Mode::Existing);
    script.push_str("if (-not [System.IO.Directory]::Exists($C)) { Quit 67 }\n");
    script.push_str(&ignore_probe(true));
    script.push_str(IGNORE_SECTION);
    script.push_str(&collapsed_function());
    script.push_str(&format!(
        r#"$global:MeworkLeft = {limit}
$MeworkLevel = [System.Collections.Generic.List[object]]::new()
$MeworkLevel.Add(@($C, $C.TrimEnd('/')))
for ($MeworkDepth = 1; $MeworkDepth -le {max_depth} -and $MeworkLevel.Count -gt 0; $MeworkDepth++) {{
  $MeworkNext = [System.Collections.Generic.List[object]]::new()
  foreach ($MeworkDir in $MeworkLevel) {{
    try {{ $items = @(([System.IO.DirectoryInfo]::new($MeworkDir[0])).GetFileSystemInfos() | Sort-Object -Property Name) }} catch {{ continue }}
    foreach ($it in $items) {{
      if ($global:MeworkLeft -le 0) {{ Quit 0 }}
      $path = $MeworkDir[1] + '/' + $it.Name
      $isDir = ($it -is [System.IO.DirectoryInfo]) -and -not ($it.Attributes -band [System.IO.FileAttributes]::ReparsePoint)
      if ($isDir) {{ Out-Line ($path + '/') }} else {{ Out-Line $path }}
      $global:MeworkLeft--
      if ($isDir -and -not (Collapsed $path $it.Name)) {{ $MeworkNext.Add(@($it.FullName, $path)) }}
    }}
  }}
  $MeworkLevel = $MeworkNext
}}
Quit 0
"#
    ));
    script
}

/// `find`: every entry under the target, optionally pre-filtered by name,
/// after the ignore section the host ranks the matches by.
pub(crate) fn find(target: &Target<'_>, path: &str, name_filter: Option<&str>, limit: usize) -> String {
    let mut script = prologue(target, path, Mode::Existing);
    script.push_str(&ignore_probe(true));
    script.push_str(IGNORE_SECTION);
    script.push_str(&format!(
        "$MeworkVcs = @({})\n",
        crate::search_scope::VCS_DIRECTORIES
            .iter()
            .map(|name| ps_single_quote(name))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    script.push_str(&walk(limit, name_filter));
    script
}

/// `grep`: `path:line:text` for every matching line, files over 2 MiB and
/// binary files (a NUL in the first 8000 bytes, as `grep -I` decides) skipped
/// under a directory, at most `limit` lines. Inside a Git work tree the files
/// are the ones `git ls-files` shows; otherwise the walk skips what
/// `Collapsed` names. A bad pattern exits 2 with .NET's complaint on stderr;
/// an unreadable file is reported on stderr and skipped.
pub(crate) fn grep(
    target: &Target<'_>,
    path: &str,
    pattern: &str,
    case_sensitive: bool,
    limit: usize,
) -> String {
    let options = if case_sensitive {
        "CultureInvariant"
    } else {
        "CultureInvariant, IgnoreCase"
    };
    let mut script = prologue(target, path, Mode::Existing);
    script.push_str(&format!(
        r#"$Pattern = {pattern}
$Options = [System.Text.RegularExpressions.RegexOptions]'{options}'
try {{ $Rx = [System.Text.RegularExpressions.Regex]::new($Pattern, $Options) }} catch {{
  $why = $_.Exception
  if ($why.InnerException) {{ $why = $why.InnerException }}
  Out-Err $why.Message
  Quit 2
}}
# A whole-file test first, so files with no match cost one regex pass. It is
# skipped for anchors that mean something different in a whole file.
$Prefilter = $null
if ($Pattern -notmatch '\\[AzZG]|\(\?<[=!]') {{ $Prefilter = [System.Text.RegularExpressions.Regex]::new($Pattern, $Options -bor [System.Text.RegularExpressions.RegexOptions]::Multiline) }}
$Utf8 = [System.Text.UTF8Encoding]::new($false, $false)
$global:MeworkLeft = {limit}
function Scan([string]$File, [string]$Shown, [bool]$SkipBinary) {{
  try {{ $bytes = [System.IO.File]::ReadAllBytes($File) }} catch {{ Out-Err ('grep: ' + $Shown + ': ' + $_.Exception.Message); return }}
  if ($SkipBinary -and [Array]::IndexOf($bytes, [byte]0, 0, [Math]::Min($bytes.Length, 8000)) -ge 0) {{ return }}
  $text = $Utf8.GetString($bytes).Replace("`r`n", "`n")
  if ($null -ne $Prefilter -and -not $Prefilter.IsMatch($text)) {{ return }}
  $lines = $text.Split("`n")
  for ($n = 0; $n -lt $lines.Length; $n++) {{
    if ($Rx.IsMatch($lines[$n])) {{
      Out-Line ($Shown + ':' + ($n + 1) + ':' + $lines[$n])
      $global:MeworkLeft--
      if ($global:MeworkLeft -le 0) {{ Quit 0 }}
    }}
  }}
}}
function Tree([string]$Dir, [string]$Shown) {{
  try {{ $items = ([System.IO.DirectoryInfo]::new($Dir)).GetFileSystemInfos() }} catch {{ Out-Err ('grep: ' + $Shown + ': ' + $_.Exception.Message); return }}
  foreach ($it in $items) {{
    if ($it.Attributes -band [System.IO.FileAttributes]::ReparsePoint) {{ continue }}
    $path = $Shown + '/' + $it.Name
    if ($it -is [System.IO.DirectoryInfo]) {{ if (-not (Collapsed $path $it.Name)) {{ Tree $it.FullName $path }} }}
    elseif ($it.Length -le 2097152) {{ Scan $it.FullName $path $true }}
  }}
}}
"#,
        pattern = ps_single_quote(pattern),
    ));
    script.push_str(&ignore_probe(false));
    script.push_str(&collapsed_function());
    script.push_str(
        r#"if (-not [System.IO.Directory]::Exists($C)) { Scan $C $C $true; Quit 0 }
if ($Ign -eq 'git') {
  $MeworkEap = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  Push-Location -LiteralPath $C
  try { $MeworkFiles = @(& git -c core.fsmonitor=false -c core.quotepath=false ls-files --cached --others --exclude-standard -- . 2>$null) } finally { Pop-Location; $ErrorActionPreference = $MeworkEap }
  if ($LASTEXITCODE -ne 0) { $Ign = 'names' }
}
if ($Ign -ne 'git') { Tree $C $C.TrimEnd('/'); Quit 0 }
# A path whose directory has become a junction since Git indexed it is not
# followed, as the walk would not have followed it.
$MeworkLinked = @{}
function Linked([string]$Relative) {
  $cut = $Relative.LastIndexOf('/')
  if ($cut -lt 0) { return $false }
  $dir = $Relative.Substring(0, $cut)
  if ($MeworkLinked.ContainsKey($dir)) { return $MeworkLinked[$dir] }
  $info = [System.IO.DirectoryInfo]::new([System.IO.Path]::Combine($C, $dir))
  $answer = (Linked $dir) -or -not $info.Exists -or [bool]($info.Attributes -band [System.IO.FileAttributes]::ReparsePoint)
  $MeworkLinked[$dir] = $answer
  return $answer
}
$MeworkPrev = $null
foreach ($MeworkFile in $MeworkFiles) {
  if (-not $MeworkFile -or $MeworkFile.StartsWith('"') -or $MeworkFile -eq $MeworkPrev) { continue }
  $MeworkPrev = $MeworkFile
  if (Linked $MeworkFile) { continue }
  try { $MeworkInfo = [System.IO.FileInfo]::new([System.IO.Path]::Combine($C, $MeworkFile)) } catch { continue }
  if (-not $MeworkInfo.Exists -or ($MeworkInfo.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -or $MeworkInfo.Length -gt 2097152) { continue }
  Scan $MeworkInfo.FullName ($C.TrimEnd('/') + '/' + $MeworkFile) $true
}
Quit 0
"#,
    );
    script
}

/// `read`: the modification time in seconds, the size, then the bytes. A file
/// over `max_bytes` exits 68, a non-file 67.
pub(crate) fn read(target: &Target<'_>, path: &str, max_bytes: usize) -> String {
    let mut script = prologue(target, path, Mode::Existing);
    script.push_str(&format!(
        "if (-not [System.IO.File]::Exists($C)) {{ Quit 67 }}\n\
         $F = [System.IO.FileInfo]::new($C)\n\
         if ($F.Length -gt {max_bytes}) {{ Quit 68 }}\n\
         $B = [System.IO.File]::ReadAllBytes($C)\n\
         Out-Line (Unix-Seconds $F)\n\
         Out-Line $B.Length\n\
         Out-Bytes $B\n\
         Quit 0\n"
    ));
    script
}

/// The first trip of `write` and `edit`: `absent`, `file` or `other`, the
/// modification time, a fingerprint (time and SHA-256), then `text` and the
/// bytes when the file is at most `max_text` bytes, or `none`.
pub(crate) fn probe(target: &Target<'_>, path: &str, max_text: u64) -> String {
    let mut script = prologue(target, path, Mode::ForWrite);
    script.push_str(&format!(
        r#"if ([System.IO.File]::Exists($C)) {{
  $F = [System.IO.FileInfo]::new($C)
  $B = [System.IO.File]::ReadAllBytes($C)
  $MT = Unix-Seconds $F
  Out-Line 'file'
  Out-Line $MT
  Out-Line ([string]$MT + ' ' + (Sha256-Hex $B))
  if ($B.Length -le {max_text}) {{ Out-Line 'text'; Out-Bytes $B }} else {{ Out-Line 'none' }}
}} elseif ([System.IO.Directory]::Exists($C)) {{
  Out-Line 'other'; Out-Line '0'; Out-Line 'other'; Out-Line 'none'
}} else {{
  Out-Line 'absent'; Out-Line '0'; Out-Line 'absent'; Out-Line 'none'
}}
Quit 0
"#
    ));
    script
}

/// Reads the whole of standard input as bytes.
const READ_INPUT: &str = r#"function Read-Input {
  if ($PSVersionTable.PSVersion.Major -ge 6) { $in = [Console]::OpenStandardInput() } else {
    $native = [Console].Assembly.GetType('Microsoft.Win32.Win32Native')
    $get = if ($native) { $native.GetMethod('GetStdHandle', [System.Reflection.BindingFlags]'NonPublic, Static') }
    if ($get) { $handle = $get.Invoke($null, @([int]-10)) } else {
      Add-Type -Namespace MeworkInput -Name Native -MemberDefinition '[DllImport("kernel32.dll")] public static extern IntPtr GetStdHandle(int n);'
      $handle = [MeworkInput.Native]::GetStdHandle(-10)
    }
    $in = New-Object System.IO.FileStream((New-Object Microsoft.Win32.SafeHandles.SafeFileHandle($handle, $false)), ([System.IO.FileAccess]::Read))
  }
  $buffer = New-Object System.IO.MemoryStream
  $in.CopyTo($buffer)
  return ,$buffer.ToArray()
}
"#;

/// A script that runs the script it is given on standard input, for a script
/// too long to travel on the command line itself (see
/// `run_environment::run_remote_script`). The script is UTF-8 and runs in this
/// process, so its `exit` is the process's exit status.
pub(crate) fn run_script_from_input() -> String {
    format!("{READ_INPUT}Invoke-Expression ([System.Text.Encoding]::UTF8.GetString((Read-Input)))\n")
}

/// The compare-and-swap write: refuses with 69 unless the file is still what
/// the probe fingerprinted, then puts standard input in place through a
/// temporary file beside it and prints the new modification time.
pub(crate) fn cas_write(target: &Target<'_>, path: &str, fingerprint: &str) -> String {
    let mut script = prologue(target, path, Mode::ForWrite);
    script.push_str(READ_INPUT);
    script.push_str(&format!(
        r#"$FP = {fingerprint}
if ($FP -eq 'absent') {{
  if ([System.IO.File]::Exists($C) -or [System.IO.Directory]::Exists($C)) {{ Quit 69 }}
}} else {{
  if (-not [System.IO.File]::Exists($C)) {{ Quit 69 }}
  $current = [string](Unix-Seconds ([System.IO.FileInfo]::new($C))) + ' ' + (Sha256-Hex ([System.IO.File]::ReadAllBytes($C)))
  if ($current -ne $FP) {{ Quit 69 }}
}}
$D = [System.IO.Path]::GetDirectoryName([System.IO.Path]::GetFullPath($C))
try {{ [void][System.IO.Directory]::CreateDirectory($D) }} catch {{ Out-Err $_.Exception.Message; Quit 70 }}
$TMP = [System.IO.Path]::Combine($D, '.mework-write.' + $PID)
try {{
  [System.IO.File]::WriteAllBytes($TMP, (Read-Input))
  if ([System.IO.File]::Exists($C)) {{
    try {{ [System.IO.File]::Replace($TMP, $C, $null) }} catch {{ [System.IO.File]::Copy($TMP, $C, $true); [System.IO.File]::Delete($TMP) }}
  }} else {{
    [System.IO.File]::Move($TMP, $C)
  }}
}} catch {{
  Out-Err $_.Exception.Message
  if ([System.IO.File]::Exists($TMP)) {{ try {{ [System.IO.File]::Delete($TMP) }} catch {{}} }}
  Quit 70
}}
Out-Line (Unix-Seconds ([System.IO.FileInfo]::new($C)))
Quit 0
"#,
        fingerprint = ps_single_quote(fingerprint),
    ));
    script
}

/// The `lsp` probe: the prologue, the remote home, the project's and the
/// user's configuration file as counted blocks (`path`, byte count, bytes — or
/// `-`), the installed preset commands one per line ended by an empty line,
/// then the file — the POSIX probe's layout.
pub(crate) fn lsp_probe(
    target: &Target<'_>,
    path: &str,
    max_file: u64,
    max_config: u64,
    config_paths: &[String; 2],
    presets: &[&str],
) -> String {
    let mut script = prologue(target, path, Mode::Existing);
    let presets = presets
        .iter()
        .map(|command| ps_single_quote(command))
        .collect::<Vec<_>>()
        .join(", ");
    script.push_str(&format!(
        r#"if (-not [System.IO.File]::Exists($C)) {{ Quit 67 }}
if (([System.IO.FileInfo]::new($C)).Length -gt {max_file}) {{ Quit 68 }}
Out-Line (Slash $HOME)
function Emit([string]$Dir) {{
  foreach ($rel in @({preferred}, {legacy})) {{
    $f = [System.IO.Path]::Combine($Dir, $rel)
    if ([System.IO.File]::Exists($f)) {{
      $b = [System.IO.File]::ReadAllBytes($f)
      if ($b.Length -le {max_config}) {{ Out-Line (Slash $f); Out-Line $b.Length; Out-Bytes $b }} else {{ Out-Line '-' }}
      return
    }}
  }}
  Out-Line '-'
}}
Emit $ROOT
Emit (Slash $HOME)
foreach ($preset in @({presets})) {{ if (Get-Command -Name $preset -CommandType Application -ErrorAction SilentlyContinue) {{ Out-Line $preset }} }}
Out-Line ''
Out-Bytes ([System.IO.File]::ReadAllBytes($C))
Quit 0
"#,
        preferred = ps_single_quote(&config_paths[0]),
        legacy = ps_single_quote(&config_paths[1]),
    ));
    script
}

/// `git check-ignore` over `paths` in `root`; git's own exit status.
pub(crate) fn check_ignore(root: &str, paths: &[String]) -> String {
    let mut script = String::from(HELPERS);
    script.push_str(&enter_root(root));
    script.push_str("& git check-ignore --");
    for path in paths {
        script.push(' ');
        script.push_str(&ps_single_quote(path));
    }
    script.push_str("\nQuit $LASTEXITCODE\n");
    script
}

/// A file's current bytes, for re-syncing a live server's copy: 66 when it is
/// not a file, 68 when it is over `max_bytes`.
pub(crate) fn read_text(canonical: &str, max_bytes: u64) -> String {
    let mut script = String::from(HELPERS);
    script.push_str(&format!(
        "$f = Native-Path {}\n\
         if (-not [System.IO.File]::Exists($f)) {{ Quit 66 }}\n\
         $b = [System.IO.File]::ReadAllBytes($f)\n\
         if ($b.Length -gt {max_bytes}) {{ Quit 68 }}\n\
         Out-Bytes $b\n\
         Quit 0\n",
        ps_single_quote(canonical)
    ));
    script
}

/// Exit 0 when either spelling of the language-server configuration exists
/// under the root, 1 when neither does, 64 when the root cannot be entered.
pub(crate) fn declares(root: &str, config_paths: &[String; 2]) -> String {
    let mut script = String::from(HELPERS);
    script.push_str(&enter_root(root));
    script.push_str(&format!(
        "$R = (Get-Location).ProviderPath\n\
         if ([System.IO.File]::Exists([System.IO.Path]::Combine($R, {})) -or [System.IO.File]::Exists([System.IO.Path]::Combine($R, {}))) {{ Quit 0 }}\n\
         Quit 1\n",
        ps_single_quote(&config_paths[0]),
        ps_single_quote(&config_paths[1]),
    ));
    script
}

/// Starts a language server in `root`: the entry's variables set, the command
/// found as an application (127 when it is not), then run as the last thing on
/// the line — which PowerShell does not pipe through itself, so the server's
/// standard streams are the agent's own, byte for byte, as the proxy's are
/// when the bootstrap starts it the same way.
pub(crate) fn lsp_launch(
    root: &str,
    command: &str,
    args: &[String],
    env: &[(String, String)],
) -> String {
    let mut script = String::from(
        "$ErrorActionPreference = 'Stop'\n$ProgressPreference = 'SilentlyContinue'\n",
    );
    // Only the helpers the launch needs: nothing may touch standard output
    // before the server owns it.
    script.push_str(
        "function Native-Path([string]$P) {\n\
         if ($P -eq '~') { return $HOME }\n\
         if ($P.StartsWith('~/') -or $P.StartsWith('~\\')) { return [System.IO.Path]::Combine($HOME, $P.Substring(2)) }\n\
         if ($P -match '^/cygdrive/([A-Za-z])(/.*)?$' -or $P -match '^/([A-Za-z])(/.*)?$') { $rest = if ($Matches[2]) { $Matches[2] } else { '/' }; return $Matches[1].ToUpper() + ':' + $rest }\n\
         return $P\n\
         }\n",
    );
    script.push_str(&format!(
        "try {{ Set-Location -LiteralPath (Native-Path {}) }} catch {{ [Console]::Error.WriteLine($_.Exception.Message); exit 64 }}\n\
         [System.Environment]::CurrentDirectory = (Get-Location).ProviderPath\n",
        ps_single_quote(root.trim())
    ));
    for (key, value) in env {
        script.push_str(&format!(
            "[System.Environment]::SetEnvironmentVariable({}, {})\n",
            ps_single_quote(key),
            ps_single_quote(value)
        ));
    }
    script.push_str(&format!(
        "$server = Get-Command -Name {} -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1\n\
         if ($null -eq $server) {{ [Console]::Error.WriteLine({}); exit 127 }}\n\
         & $server.Source",
        ps_single_quote(command),
        ps_single_quote(&format!("{command} is not on the remote PATH")),
    ));
    for arg in args {
        script.push(' ');
        script.push_str(&ps_single_quote(arg));
    }
    script.push_str("\nexit $LASTEXITCODE\n");
    script
}

/// The Git status probe of [`crate::remote_git`]: the POSIX probe's reads, in
/// its framing, byte for byte.
///
/// Each read runs Git as a process whose standard output is copied as bytes,
/// never through PowerShell's pipeline, which would decode `status -z`'s NULs
/// and rewrite its line endings. The tracked diffs are digested here with
/// .NET's SHA-256 rather than by `git hash-object`: the digest is opaque to
/// the host, so either serves. Every argument is fixed text without spaces or
/// quotes, so the command line needs no Windows quoting.
pub(crate) fn git_status_probe(root: &str) -> String {
    use crate::remote_git::{
        BRANCHES_FORMAT, DIGEST_DIFF, GIT_ENVIRONMENT, GIT_PREFIX, OPERATIONS, PROBE_MAGIC,
    };
    let mut script = String::from(HELPERS);
    script.push_str(&enter_root(root));
    let cleared = GIT_ENVIRONMENT
        .iter()
        .map(|name| ps_single_quote(name))
        .collect::<Vec<_>>()
        .join(", ");
    script.push_str(&format!(
        r#"$gitCommand = Get-Command git -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
if ($null -eq $gitCommand) {{ Out-Err 'git: command not found'; Quit 127 }}
$MeworkGit = $gitCommand.Path
$MeworkCwd = (Get-Location).ProviderPath
foreach ($name in @({cleared})) {{ Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue }}
$env:GIT_OPTIONAL_LOCKS = '0'; $env:GIT_TERMINAL_PROMPT = '0'; $env:GIT_PAGER = 'cat'; $env:PAGER = 'cat'; $env:LC_ALL = 'C'; $env:LANG = 'C'
function Emit([string]$Name, [int]$Code, $Out, $Err) {{
  if ($null -eq $Out) {{ $Out = New-Object byte[] 0 }}
  if ($null -eq $Err) {{ $Err = New-Object byte[] 0 }}
  Out-Line ('{{0}} {{1}} {{2}} {{3}}' -f $Name, $Code, $Out.Length, $Err.Length)
  Out-Bytes $Out
  Out-Bytes $Err
}}
function Git-Run([string]$Arguments) {{
  $psi = New-Object System.Diagnostics.ProcessStartInfo
  $psi.FileName = $MeworkGit
  $psi.Arguments = {prefix} + ' ' + $Arguments
  $psi.WorkingDirectory = $MeworkCwd
  $psi.UseShellExecute = $false
  $psi.CreateNoWindow = $true
  $psi.RedirectStandardInput = $true
  $psi.RedirectStandardOutput = $true
  $psi.RedirectStandardError = $true
  $proc = [System.Diagnostics.Process]::Start($psi)
  $proc.StandardInput.Close()
  $errText = $proc.StandardError.ReadToEndAsync()
  $buffer = New-Object System.IO.MemoryStream
  $proc.StandardOutput.BaseStream.CopyTo($buffer)
  $proc.WaitForExit()
  return @{{ Code = $proc.ExitCode; Out = $buffer.ToArray(); Err = [System.Text.Encoding]::UTF8.GetBytes($errText.Result) }}
}}
function Run-Read([string]$Name, [string]$Arguments) {{
  $read = Git-Run $Arguments
  Emit $Name $read.Code $read.Out $read.Err
}}
function Run-Digest([string]$Name, [string]$Arguments) {{
  $read = Git-Run $Arguments
  $out = $read.Out
  if ($null -eq $out) {{ $out = New-Object byte[] 0 }}
  Emit $Name $read.Code ([System.Text.Encoding]::UTF8.GetBytes((Sha256-Hex $out))) $read.Err
}}
Out-Line {magic}
$top = Git-Run 'rev-parse --show-prefix --show-toplevel --absolute-git-dir --git-common-dir'
Emit 'rev-parse' $top.Code $top.Out $top.Err
if ($top.Code -ne 0) {{ Quit 0 }}
$lines = [System.Text.Encoding]::UTF8.GetString($top.Out).Split([char]10)
if ($lines.Count -lt 4 -or $lines[0].TrimEnd([char]13) -ne '') {{ Quit 0 }}
$gitDir = $lines[2].TrimEnd([char]13)
Run-Read 'version' '--version'
Run-Read 'status' 'status --porcelain=v2 -z --branch --show-stash --untracked-files=all'
$hasHead = Git-Run 'rev-parse -q --verify HEAD'
if ($hasHead.Code -eq 0) {{ Run-Read 'numstat' 'diff --no-ext-diff --no-textconv --numstat -z HEAD' }} else {{ Run-Read 'numstat' 'diff --no-ext-diff --no-textconv --numstat -z --cached' }}
Run-Digest 'staged-digest' {staged}
Run-Digest 'unstaged-digest' {unstaged}
Run-Read 'branches' {branches}
Run-Read 'upstream-oid' 'rev-parse -q --verify @{{upstream}}^{{commit}}'
Run-Read 'remotes' 'remote -v'
function State-Exists([string]$Relative) {{ return (Test-Path -LiteralPath ([System.IO.Path]::Combine($gitDir, $Relative))) }}
$operation = ''
$stateFiles = @()
"#,
        prefix = ps_single_quote(GIT_PREFIX),
        magic = ps_single_quote(PROBE_MAGIC),
        staged = ps_single_quote(&format!("{DIGEST_DIFF} --cached --")),
        unstaged = ps_single_quote(&format!("{DIGEST_DIFF} --")),
        branches = ps_single_quote(&format!("for-each-ref {BRANCHES_FORMAT} refs/heads")),
    ));
    for (index, (label, markers, files)) in OPERATIONS.iter().enumerate() {
        let test = markers
            .iter()
            .map(|marker| format!("(State-Exists {})", ps_single_quote(marker)))
            .collect::<Vec<_>>()
            .join(" -or ");
        let files = files
            .iter()
            .map(|file| ps_single_quote(file))
            .collect::<Vec<_>>()
            .join(", ");
        script.push_str(if index == 0 { "if (" } else { "} elseif (" });
        script.push_str(&format!(
            "{test}) {{ $operation = {}; $stateFiles = @({files}) ",
            ps_single_quote(label)
        ));
    }
    script.push_str(
        r#"}
$state = $operation + "`n"
foreach ($stateFile in $stateFiles) {
  $statePath = [System.IO.Path]::Combine($gitDir, $stateFile)
  if ([System.IO.File]::Exists($statePath)) { $state += $stateFile + ' ' + (Sha256-Hex ([System.IO.File]::ReadAllBytes($statePath))) + "`n" }
}
Emit 'operation' 0 ([System.Text.Encoding]::UTF8.GetBytes($state)) $null
Quit 0
"#,
    );
    script
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_git_probe_reads_what_the_posix_probe_reads_in_its_framing() {
        let script = git_status_probe("C:/Users/dev/it's");
        assert!(
            script.contains("Native-Path 'C:/Users/dev/it''s'"),
            "{script}"
        );
        for section in [
            "'rev-parse'",
            "'version'",
            "'status'",
            "'numstat'",
            "'staged-digest'",
            "'unstaged-digest'",
            "'branches'",
            "'upstream-oid'",
            "'remotes'",
            "'operation'",
        ] {
            assert!(script.contains(section), "{section}");
        }
        assert!(script.contains("Quit 127"));
        assert!(script.contains("} elseif ((State-Exists 'rebase-merge') -or (State-Exists 'rebase-apply')) { $operation = 'rebase'"), "{script}");
        // Standard output is copied as bytes, never read as text.
        assert!(script.contains("StandardOutput.BaseStream.CopyTo"));
        // Nothing the script calls leaves an object on the pipeline, where
        // PowerShell's formatter would print it into the answer.
        assert!(!script.contains("return $read"));
    }

    fn target(confine: bool) -> Target<'static> {
        Target {
            root: "C:/Users/dev/app",
            confine,
        }
    }

    #[test]
    fn operands_reach_the_script_as_single_quoted_literals() {
        let script = listing(&target(true), "it's $(here)", 2, 11);
        assert!(script.contains("$REQ = 'it''s $(here)'"), "{script}");
        assert!(script.contains("Native-Path 'C:/Users/dev/app'"), "{script}");
        assert!(script.contains("Inside $C $ROOT"), "confinement is compiled in");
        assert!(!listing(&target(false), ".", 2, 11).contains("Inside $C $ROOT"));
        // PowerShell's typographic quotes delimit strings too.
        let script = grep(&target(true), ".", "a\u{2019}b", true, 5);
        assert!(script.contains("'a\u{2019}\u{2019}b'"), "{script}");
    }

    #[test]
    fn every_reserved_exit_code_is_where_the_posix_leg_has_it() {
        let read = read(&target(true), "a.txt", 100);
        for code in ["Quit 64", "Quit 65", "Quit 66", "Quit 67", "Quit 68"] {
            assert!(read.contains(code), "{code}");
        }
        let write = cas_write(&target(true), "a.txt", "absent");
        for code in ["Quit 69", "Quit 70"] {
            assert!(write.contains(code), "{code}");
        }
        assert!(grep(&target(true), ".", "(", false, 5).contains("Quit 2"));
        assert!(lsp_launch("C:/w", "gopls", &[], &[]).contains("exit 127"));
    }

    #[test]
    fn the_launch_writes_nothing_before_the_server_owns_standard_output() {
        let script = lsp_launch(
            "C:/w",
            "typescript-language-server",
            &["--stdio".to_owned()],
            &[("NODE_OPTIONS".to_owned(), "--max-old-space-size=4096".to_owned())],
        );
        assert!(!script.contains("OpenStandardOutput"));
        assert!(!script.contains("Out-Line"));
        assert!(script.contains("& $server.Source '--stdio'\nexit $LASTEXITCODE"), "{script}");
        assert!(script.contains("SetEnvironmentVariable('NODE_OPTIONS', '--max-old-space-size=4096')"));
    }

    /// PowerShell names ignore case, so nothing after the prologue may assign
    /// or loop over `$C`, `$T`, `$ROOT` or `$REQ` in any spelling — the
    /// language-server probe once looped over its presets as `$c` and read
    /// the last preset's name as the file.
    #[test]
    fn no_script_reuses_the_prologues_variables_in_another_case() {
        let paths = [".mework/lsp.json".to_owned(), ".naiword/lsp.json".to_owned()];
        let scripts = [
            listing(&target(true), ".", 2, 11),
            find(&target(true), ".", Some("*.rs"), 10),
            grep(&target(true), ".", "x", false, 10),
            read(&target(true), "a", 10),
            probe(&target(true), "a", 10),
            cas_write(&target(true), "a", "absent"),
            lsp_probe(&target(true), "a", 10, 10, &paths, &["gopls", "clangd"]),
        ];
        let loops = regex::Regex::new(r"(?i)foreach\s*\(\s*\$(c|t|root|req)\s+in").unwrap();
        let assigned = regex::Regex::new(r"(?im)^\s*\$(c|t|root|req)\s*=").unwrap();
        for script in &scripts {
            assert!(!loops.is_match(script), "{script}");
            let body = &script[script.find("Out-Line $C\n").expect("prologue")..];
            assert!(!assigned.is_match(body), "{body}");
        }
    }

    #[test]
    fn the_name_prefilter_escapes_powershells_own_escape() {
        let script = find(&target(true), ".", Some("a`b*.rs"), 10);
        assert!(script.contains("-like 'a``b*.rs'"), "{script}");
        let script = find(&target(true), ".", None, 10);
        assert!(script.contains("if ($true)"), "{script}");
    }
}
