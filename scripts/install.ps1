# Toolport installer for Windows. One-liner:
#   irm https://raw.githubusercontent.com/tsouth89/toolport/main/scripts/install.ps1 | iex
#
# Downloads the NSIS installer for the latest release, verifies it against the
# SHA-256 GitHub publishes for that asset, and runs it silently (per-user, no
# admin). macOS/Linux use scripts/install.sh instead.
#
# Options (the pipe-to-iex form can't take parameters, so each has an env var):
#   -Version 1.12.0      TOOLPORT_VERSION=1.12.0    install a specific release
#   -Interactive         TOOLPORT_INTERACTIVE=1     run the setup wizard instead of /S
#   -DownloadOnly        TOOLPORT_DOWNLOAD_ONLY=1   fetch + verify, don't install
#   -AllowUnverified     TOOLPORT_ALLOW_UNVERIFIED=1  install even with no published checksum
#
# To pass parameters through the one-liner:
#   & ([scriptblock]::Create((irm https://raw.githubusercontent.com/tsouth89/toolport/main/scripts/install.ps1))) -Version 1.12.0

param(
    [string]$Version,
    [switch]$Interactive,
    [switch]$DownloadOnly,
    [switch]$AllowUnverified
)

$prevErrorAction = $ErrorActionPreference
$prevProgress = $ProgressPreference
$ErrorActionPreference = "Stop"

# Everything lives in one function so that piping this script into `iex` doesn't
# leave a pile of helpers behind in the caller's session.
function Install-Toolport {
    $repo = "tsouth89/toolport"
    $releasesUrl = "https://github.com/$repo/releases"

    function Say($msg) { Write-Host "> $msg" -ForegroundColor Cyan }
    function Note($msg) { Write-Host "  $msg" -ForegroundColor DarkGray }
    function Warn($msg) { Write-Host "! $msg" -ForegroundColor Yellow }

    # Env fallbacks for the `irm | iex` form, which can't bind parameters.
    function EnvFlag($name) {
        $v = [Environment]::GetEnvironmentVariable($name)
        return $v -and $v -notin @("0", "false", "no", "off")
    }
    if (-not $Version -and $env:TOOLPORT_VERSION) { $Version = $env:TOOLPORT_VERSION }
    if (EnvFlag "TOOLPORT_INTERACTIVE") { $Interactive = $true }
    if (EnvFlag "TOOLPORT_DOWNLOAD_ONLY") { $DownloadOnly = $true }
    if (EnvFlag "TOOLPORT_ALLOW_UNVERIFIED") { $AllowUnverified = $true }

    # --- Preflight -----------------------------------------------------------
    # $IsWindows only exists on PowerShell 6+; Windows PowerShell 5.1 is Windows by
    # definition.
    $onWindows = if ($null -ne $IsWindows) { $IsWindows } else { $true }
    if (-not $onWindows) {
        throw "This installer is for Windows. On macOS or Linux run: curl -fsSL https://raw.githubusercontent.com/$repo/main/scripts/install.sh | bash"
    }
    if ($PSVersionTable.PSVersion -lt [version]"5.1") {
        throw "PowerShell 5.1 or newer is required (found $($PSVersionTable.PSVersion))."
    }

    # PROCESSOR_ARCHITECTURE reports the *process* arch, so a 32-bit PowerShell on
    # 64-bit Windows says x86; PROCESSOR_ARCHITEW6432 carries the real machine arch
    # in that case.
    $arch = $env:PROCESSOR_ARCHITEW6432
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
    switch ($arch) {
        "AMD64" { $wantArch = "x64" }
        "ARM64" { $wantArch = "arm64" }
        default {
            throw "Unsupported architecture '$arch'. Toolport ships a 64-bit build only; see $releasesUrl."
        }
    }

    # Windows PowerShell 5.1 defaults to SSL3/TLS1.0 on older builds, which
    # api.github.com refuses. Harmless on PowerShell 7 (this property is ignored).
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    } catch {
        Note "Could not raise the TLS version; continuing with the system default."
    }

    # --- Resolve the release -------------------------------------------------
    $tag = $null
    if ($Version) {
        # Validate before interpolating into a URL.
        if ($Version -notmatch "^v?\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$") {
            throw "'$Version' doesn't look like a version (expected something like 1.12.0)."
        }
        $tag = if ($Version.StartsWith("v")) { $Version } else { "v$Version" }
        $apiUrl = "https://api.github.com/repos/$repo/releases/tags/$tag"
    } else {
        $apiUrl = "https://api.github.com/repos/$repo/releases/latest"
    }

    $headers = @{
        "Accept"               = "application/vnd.github+json"
        "X-GitHub-Api-Version" = "2022-11-28"
    }
    # Optional: lifts the unauthenticated 60-requests/hour limit, which shared office
    # or CI egress IPs can exhaust without any help from this script.
    $token = if ($env:GITHUB_TOKEN) { $env:GITHUB_TOKEN } else { $env:GH_TOKEN }
    if ($token) { $headers["Authorization"] = "Bearer $token" }

    Say "Looking up the $(if ($tag) { $tag } else { 'latest' }) Toolport release"
    try {
        $release = Invoke-RestMethod -Uri $apiUrl -Headers $headers -UserAgent "toolport-install.ps1" -TimeoutSec 30
    } catch {
        $status = $null
        if ($_.Exception.Response) { $status = [int]$_.Exception.Response.StatusCode }
        if ($status -eq 404) {
            # $tag is null on the releases/latest lookup, which 404s when every
            # release is still a draft.
            if ($tag) {
                throw "No release tagged $tag. See $releasesUrl for what's published."
            }
            throw "There is no published Toolport release yet. See $releasesUrl."
        }
        if ($status -eq 403 -or $status -eq 429) {
            throw "GitHub rate-limited this machine. Wait an hour, or set `$env:GITHUB_TOKEN to a personal access token and re-run."
        }
        throw "Couldn't reach the GitHub releases API ($apiUrl): $($_.Exception.Message)"
    }

    $tag = $release.tag_name
    if (-not $tag) { throw "The GitHub API returned a release with no tag name." }

    # Prefer this machine's architecture, then fall back to the x64 build, which
    # ARM64 Windows runs under emulation. `-setup.exe` also excludes the updater
    # signature asset (`-setup.exe.sig`).
    $installers = @($release.assets | Where-Object { $_.name -like "*-setup.exe" })
    $asset = $installers | Where-Object { $_.name -like "*_$wantArch-setup.exe" } | Select-Object -First 1
    if (-not $asset -and $wantArch -ne "x64") {
        $asset = $installers | Where-Object { $_.name -like "*_x64-setup.exe" } | Select-Object -First 1
        if ($asset) { Note "No native $wantArch build in $tag; using the x64 installer (Windows runs it emulated)." }
    }
    if (-not $asset) {
        throw "$tag has no Windows installer (looked for *_$wantArch-setup.exe). Check $releasesUrl/tag/$tag."
    }

    $url = $asset.browser_download_url
    if (-not $url -or -not $url.StartsWith("https://")) {
        throw "Refusing to download $($asset.name): the release lists no https download URL for it."
    }

    # --- Checksum, decided before anything is downloaded ---------------------
    # GitHub publishes a per-asset digest ("sha256:...") on the releases API. That
    # is the only checksum this project publishes, so if it's missing there is
    # nothing to verify against and we stop rather than install blind.
    $digest = $asset.digest
    $hashAlgorithm = $null
    $expectedHash = $null
    if ($digest -and $digest -match "^(sha256|sha384|sha512):([0-9a-fA-F]+)$") {
        $hashAlgorithm = $Matches[1].ToUpperInvariant()
        $expectedHash = $Matches[2].ToLowerInvariant()
    } elseif ($AllowUnverified) {
        Warn "GitHub publishes no usable checksum for $($asset.name) and -AllowUnverified was passed."
        Warn "Installing an unverified binary. Nothing here can tell you it wasn't tampered with."
    } else {
        throw @"
GitHub publishes no checksum for $($asset.name), so this download can't be verified.
Refusing to install it. Either:
  - download it yourself from $releasesUrl/tag/$tag and check it against a source you trust, or
  - re-run with -AllowUnverified (or `$env:TOOLPORT_ALLOW_UNVERIFIED=1) to install anyway.
"@
    }

    # --- Download ------------------------------------------------------------
    $work = Join-Path ([IO.Path]::GetTempPath()) ("toolport-install-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Path $work -Force | Out-Null
    try {
        # GetFileName so a release asset named with a path separator can't place
        # the download outside the temp directory.
        $exe = Join-Path $work ([IO.Path]::GetFileName($asset.name))
        $sizeMb = if ($asset.size) { " ({0:N1} MB)" -f ($asset.size / 1MB) } else { "" }
        Say "Downloading $($asset.name)$sizeMb"
        # Progress rendering makes Invoke-WebRequest roughly an order of magnitude
        # slower on Windows PowerShell 5.1; the restore is in the outer finally.
        $ProgressPreference = "SilentlyContinue"
        try {
            # No API headers here: the asset URL is public and redirects to a
            # pre-signed objects.githubusercontent.com URL, which rejects requests
            # that also carry an Authorization header.
            Invoke-WebRequest -Uri $url -OutFile $exe -UserAgent "toolport-install.ps1" -UseBasicParsing -TimeoutSec 600
        } catch {
            throw "Download failed ($url): $($_.Exception.Message)"
        }
        if (-not (Test-Path $exe) -or (Get-Item $exe).Length -eq 0) {
            throw "Download produced an empty file ($url)."
        }
        if ($asset.size -and (Get-Item $exe).Length -ne $asset.size) {
            throw "Download is $((Get-Item $exe).Length) bytes but the release says $($asset.size). Treating it as truncated."
        }

        # --- Verify ----------------------------------------------------------
        if ($expectedHash) {
            $actual = (Get-FileHash -Path $exe -Algorithm $hashAlgorithm).Hash.ToLowerInvariant()
            if ($actual -ne $expectedHash) {
                throw @"
$hashAlgorithm mismatch for $($asset.name). Deleting the download and stopping.
  expected $expectedHash
  got      $actual
"@
            }
            Say "$hashAlgorithm verified: $actual"
        }

        # Authenticode is a separate question from "are these the bytes GitHub
        # published": releases built before the signing secrets landed are unsigned,
        # so a missing signature is reported, not fatal. A signature that doesn't
        # match the file it's on is fatal.
        $signature = Get-AuthenticodeSignature -FilePath $exe
        $publisher = $null
        # A CN containing a comma is quoted in the subject ('CN="South, Brandon", O=...'),
        # so match the quoted form first or the name comes out truncated.
        if ($signature.SignerCertificate -and $signature.SignerCertificate.Subject -match 'CN=("[^"]*"|[^,]+)') {
            $publisher = $Matches[1].Trim('"').Trim()
        }
        switch ($signature.Status) {
            "Valid" { Say "Authenticode signature valid$(if ($publisher) { ": $publisher" })" }
            "NotSigned" { Warn "This build isn't code-signed. Windows may warn about an unknown publisher." }
            "HashMismatch" { throw "The Authenticode signature on $($asset.name) doesn't match its contents. Not installing." }
            default {
                Warn "Authenticode status: $($signature.Status)$(if ($publisher) { " ($publisher)" })."
                if ($expectedHash) {
                    Note "The published $hashAlgorithm matched, so this is usually a certificate-store or clock problem on this machine."
                }
            }
        }

        if ($DownloadOnly) {
            # Move it somewhere that survives the temp cleanup below.
            $kept = Join-Path ([Environment]::GetFolderPath("UserProfile")) "Downloads\$($asset.name)"
            if (-not (Test-Path (Split-Path $kept))) { $kept = Join-Path ([IO.Path]::GetTempPath()) $asset.name }
            Move-Item -Path $exe -Destination $kept -Force
            $verified = if ($expectedHash) { "Verified installer" } else { "Unverified installer" }
            Say "$verified saved to $kept (not installed: -DownloadOnly)"
            return
        }

        # --- Install -----------------------------------------------------------
        # Silent by default: someone who typed a one-liner into a terminal has already
        # made the decision the wizard would ask about, and a GUI popping out of a
        # piped command is a surprise. -Interactive opts back into the wizard.
        # The NSIS bundle installs per-user (Tauri's default install mode), so
        # neither path needs administrator rights.
        if ($Interactive) {
            Say "Running the installer (wizard)"
            $proc = Start-Process -FilePath $exe -Wait -PassThru
        } else {
            Say "Installing silently (pass -Interactive for the wizard)"
            $proc = Start-Process -FilePath $exe -ArgumentList "/S" -Wait -PassThru
        }
        if ($proc.ExitCode -ne 0) {
            $hint = switch ($proc.ExitCode) {
                1223 { " (the elevation prompt was cancelled)" }
                default { "" }
            }
            throw "The installer exited with code $($proc.ExitCode)$hint. Nothing was changed if it failed early; otherwise re-run it from $releasesUrl/tag/$tag to see the error in the wizard."
        }
    } finally {
        Remove-Item -Path $work -Recurse -Force -ErrorAction SilentlyContinue
    }

    # --- Confirm -------------------------------------------------------------
    # A zero exit code from NSIS isn't proof on its own (a cancelled wizard can
    # return zero), so read back the uninstall entry the installer writes.
    $uninstallKeys = @(
        "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*",
        "HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*",
        "HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*"
    )
    $entry = Get-ItemProperty -Path $uninstallKeys -ErrorAction SilentlyContinue |
        Where-Object { $_.DisplayName -eq "Toolport" } |
        Select-Object -First 1

    if (-not $entry) {
        Warn "The installer reported success but no 'Toolport' entry appeared in Add or Remove Programs."
        Warn "If you cancelled the wizard, re-run this script. Otherwise install manually from $releasesUrl/tag/$tag."
        return
    }

    # NSIS writes these quoted, and the app's binary is named after the crate
    # (conduit.exe), not the product, so take the name the installer recorded.
    # Both values are treated as optional: the install already succeeded by the
    # time we read them, so a key that's missing a value is worth a vaguer
    # message, not a failure report for a working install.
    $installDir = if ($entry.InstallLocation) { $entry.InstallLocation.Trim('"') } else { $null }
    Say "Installed Toolport $($entry.DisplayVersion)$(if ($installDir) { " to $installDir" })"
    if ($entry.DisplayVersion -and $tag -and $entry.DisplayVersion -ne $tag.TrimStart("v")) {
        Warn "That's not the $tag this script downloaded - an existing newer install may have won."
    }
    $launcher = if ($installDir -and $entry.MainBinaryName) {
        Join-Path $installDir $entry.MainBinaryName
    } elseif ($entry.DisplayIcon) {
        $entry.DisplayIcon.Trim('"')
    }
    if ($launcher -and (Test-Path $launcher)) {
        Note "Launch it from the Start menu, or run: & '$launcher'"
    } else {
        Note "Launch it from the Start menu."
    }
}

try {
    Install-Toolport
} catch {
    Write-Host "x $($_.Exception.Message)" -ForegroundColor Red
    # `exit` inside `iex` would close the user's shell, so only a real script
    # invocation gets a non-zero exit code; the one-liner just reports and stops.
    if ($PSCommandPath) { exit 1 }
} finally {
    $ErrorActionPreference = $prevErrorAction
    $ProgressPreference = $prevProgress
    # Under `iex` the function is defined in the caller's session, so clear it too
    # rather than leaving an Install-Toolport behind in their shell.
    Remove-Item Function:\Install-Toolport -ErrorAction SilentlyContinue
}
