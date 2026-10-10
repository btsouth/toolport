param([Parameter(Mandatory=$true)][string]$Path)
$ErrorActionPreference = 'Stop'
# Read the File table without installing, extracting, or executing custom actions.
$installer = New-Object -ComObject WindowsInstaller.Installer
$database = $installer.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @((Resolve-Path $Path).Path, 0))
$view = $database.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $database, @('SELECT `FileName` FROM `File`'))
$view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) | Out-Null
$files = @()
while ($record = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)) {
    $name = $record.GetType().InvokeMember('StringData', 'GetProperty', $null, $record, @(1))
    $files += ($name -split '\|')[-1]
}
ConvertTo-Json -InputObject @($files) -Compress
