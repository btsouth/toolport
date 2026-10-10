param([Parameter(Mandatory=$true)][string]$Path)
$ErrorActionPreference = 'Stop'
# Read installer tables without installing or executing custom actions.
$installer = New-Object -ComObject WindowsInstaller.Installer
$database = $installer.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @((Resolve-Path $Path).Path, 0))
function Read-Rows([string]$Sql, [string[]]$Columns) {
    $view = $database.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $database, @($Sql))
    $view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) | Out-Null
    while ($record = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)) {
        $row = @{}
        for ($i = 0; $i -lt $Columns.Length; $i++) {
            $row[$Columns[$i]] = $record.GetType().InvokeMember('StringData', 'GetProperty', $null, $record, @($i + 1))
        }
        [pscustomobject]$row
    }
    $view.GetType().InvokeMember('Close', 'InvokeMethod', $null, $view, $null) | Out-Null
}
$directories = @{}
Read-Rows 'SELECT `Directory`, `Directory_Parent`, `DefaultDir` FROM `Directory`' @('Id', 'Parent', 'Name') | ForEach-Object { $directories[$_.Id] = $_ }
$components = @{}
Read-Rows 'SELECT `Component`, `Directory_` FROM `Component`' @('Id', 'Directory') | ForEach-Object { $components[$_.Id] = $_.Directory }
function Directory-Path([string]$Id, [System.Collections.Generic.HashSet[string]]$Seen) {
    if (-not $Id -or $Id -eq 'TARGETDIR') { return '' }
    if (-not $Seen.Add($Id)) { throw "Directory cycle at $Id" }
    $row = $directories[$Id]
    if (-not $row) { throw "Missing directory $Id" }
    $parent = Directory-Path $row.Parent $Seen
    $name = (($row.Name -split ':')[0] -split '\|')[-1]
    if ($Id -in @('ProgramFilesFolder', 'ProgramFiles64Folder', 'LocalAppDataFolder')) { $name = $Id }
    if ($name -eq '.') { return $parent }
    return (@($parent, $name) | Where-Object { $_ }) -join '/'
}
$files = @(Read-Rows 'SELECT `FileName`, `Component_` FROM `File`' @('Name', 'Component') | ForEach-Object {
    $directory = Directory-Path $components[$_.Component] ([System.Collections.Generic.HashSet[string]]::new())
    $name = ($_.Name -split '\|')[-1]
    (@($directory, $name) | Where-Object { $_ }) -join '/'
})
ConvertTo-Json -InputObject $files -Compress
