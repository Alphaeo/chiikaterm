# chiika shell integration: marks prompts and commands (OSC 133) so chiika can follow them.
# Passed by chiika with -EncodedCommand after the user's profile; nothing is written to disk.
# Works on Windows PowerShell 5.1 (no `e escape) and PowerShell 7.
if (Test-Path variable:global:__ChiikaPrompt) { return }
$global:__ChiikaPrompt = $function:prompt
$global:__ChiikaFirst = $true

function global:prompt {
    $ok = $global:?
    $code = $global:LASTEXITCODE
    $e = [char]27
    $b = [char]7
    $out = ''
    if (-not $global:__ChiikaFirst) {
        $status = if ($ok) { 0 } elseif ($code) { $code } else { 1 }
        $out += "$e]133;D;$status$b"
    }
    $global:__ChiikaFirst = $false
    $cwd = $executionContext.SessionState.Path.CurrentLocation.ProviderPath
    $out += "$e]9;9;$cwd$b$e]133;A$b"
    $out += & $global:__ChiikaPrompt
    $out += "$e]133;B$b"
    $global:LASTEXITCODE = $code
    $out
}

if (Get-Module PSReadLine) {
    Set-PSReadLineKeyHandler -Chord Enter -ScriptBlock {
        $line = $null
        $cursor = $null
        [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
        $e = [char]27
        $b = [char]7
        $text = $line.Replace('\', '\\').Replace(';', '\x3b').Replace("`n", '\x0a')
        $text = $text.Replace("`r", '').Replace([string]$e, '').Replace([string]$b, '')
        [Console]::Write("$e]633;E;$text$b$e]133;C$b")
        [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
    }
}
