# Resident parse server. One request per stdin line: the source text as base64 (UTF-8).
# One reply per request: `ok|<n parse errors>|<b64>,<b64>,...` or `err|<message>`.
# The first line written is `ready`.
$ErrorActionPreference = 'Stop'
$utf8 = New-Object System.Text.UTF8Encoding($false)
$reader = New-Object System.IO.StreamReader([Console]::OpenStandardInput(), $utf8)
$writer = New-Object System.IO.StreamWriter([Console]::OpenStandardOutput(), $utf8)
$writer.NewLine = "`n"
$writer.AutoFlush = $true
$isCommand = [Func[System.Management.Automation.Language.Ast, bool]] {
    param($a) $a -is [System.Management.Automation.Language.CommandAst]
}

$writer.WriteLine('ready')

while ($null -ne ($line = $reader.ReadLine())) {
    try {
        $src = $utf8.GetString([Convert]::FromBase64String($line.Trim()))
        $tokens = $null
        $errors = $null
        $ast = [System.Management.Automation.Language.Parser]::ParseInput($src, [ref]$tokens, [ref]$errors)
        $n = if ($null -eq $errors) { 0 } else { $errors.Count }
        $names = New-Object System.Collections.Generic.List[string]
        if ($n -eq 0) {
            foreach ($c in $ast.FindAll($isCommand, $true)) {
                if ($c.CommandElements.Count -gt 0) {
                    $text = $c.CommandElements[0].Extent.Text
                    $names.Add([Convert]::ToBase64String($utf8.GetBytes($text)))
                }
            }
        }
        $writer.WriteLine('ok|' + $n + '|' + ($names -join ','))
    }
    catch {
        $msg = ([string]$_.Exception.Message) -replace "[\r\n|]+", ' '
        $writer.WriteLine('err|' + $msg)
    }
}
