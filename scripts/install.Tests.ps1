# Pester tests for scripts/install.ps1 (SBS-713).
#
# The script is piped into `iex` from a pinned commit, so a regression in asset
# selection, checksum enforcement, or the silent-install flag reaches users'
# machines with no build step in between. Nothing else in CI executes it.
#
# These drive the real script end to end with the network and the installer
# mocked, so the assertions are about the behaviour a user gets rather than
# about extracted helpers. `Get-FileHash` is deliberately NOT mocked: the
# download mock writes known bytes and the fake release carries their real
# digest, so the checksum tests fail if verification is skipped or inverted.
#
# Run: pwsh -NoProfile -Command "Invoke-Pester -Path scripts/install.Tests.ps1"

# Minimal stand-ins for the HTTP failure shape the script reads
# (`$_.Exception.Response.StatusCode`). Classes must live at file scope.
class FakeHttpResponse {
    [int]$StatusCode
    FakeHttpResponse([int]$status) { $this.StatusCode = $status }
}
class FakeHttpException : System.Exception {
    [object]$Response
    FakeHttpException([string]$message, [int]$status) : base($message) {
        $this.Response = [FakeHttpResponse]::new($status)
    }
}

BeforeAll {
    # These fixtures are deliberately global, not $script:. A Pester mock body
    # runs in the scope of the code that called the mocked command -- here, the
    # scope of install.ps1 -- so a $script: variable declared in this file
    # resolves to nothing inside the download and release mocks below.
    $global:TpInstallScript = Join-Path $PSScriptRoot "install.ps1"

    # 64 known bytes stand in for the downloaded installer. The fake release
    # advertises this exact size and digest, so the size and checksum gates see
    # a genuinely consistent asset unless a test deliberately breaks one.
    $global:TpFakeBytes = [byte[]](1..64)
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $global:TpFakeSha256 = -join ($sha.ComputeHash($global:TpFakeBytes) | ForEach-Object { $_.ToString("x2") })
    } finally {
        $sha.Dispose()
    }

    function New-FakeAsset {
        param(
            [string]$Name,
            [object]$Digest = $global:TpFakeSha256,
            [object]$Size = 64,
            [string]$Url
        )
        if (-not $Url) {
            $Url = "https://github.com/tsouth89/toolport/releases/download/v1.13.0/$Name"
        }
        [pscustomobject]@{
            name                 = $Name
            browser_download_url = $Url
            digest               = if ($null -eq $Digest) { $null } else { "sha256:$Digest" }
            size                 = $Size
        }
    }

    function New-FakeRelease {
        param([object[]]$Assets, [string]$Tag = "v1.13.0")
        [pscustomobject]@{ tag_name = $Tag; assets = @($Assets) }
    }

    # Invoke the script and return its console output plus the exit code it set.
    # `exit` inside a script called with `&` ends that script only, so a refusal
    # is observable here rather than tearing down the test run.
    function Invoke-Installer {
        param([hashtable]$Parameters = @{})
        $global:LASTEXITCODE = 0
        $output = & $global:TpInstallScript @Parameters 6>&1 | Out-String
        [pscustomobject]@{
            Output   = $output
            ExitCode = $LASTEXITCODE
        }
    }
}

AfterAll {
    Remove-Variable -Name TpInstallScript, TpFakeBytes, TpFakeSha256 -Scope Global -ErrorAction SilentlyContinue
}

Describe "install.ps1" {
    BeforeEach {
        # The script reads the machine architecture and four TOOLPORT_* env
        # vars. Pin them per test so the developer's real environment cannot
        # change the outcome, and restore them afterwards.
        $script:SavedEnv = @{}
        foreach ($name in @(
                "PROCESSOR_ARCHITECTURE", "PROCESSOR_ARCHITEW6432",
                "TOOLPORT_VERSION", "TOOLPORT_INTERACTIVE",
                "TOOLPORT_DOWNLOAD_ONLY", "TOOLPORT_ALLOW_UNVERIFIED",
                "GITHUB_TOKEN", "GH_TOKEN")) {
            $script:SavedEnv[$name] = [Environment]::GetEnvironmentVariable($name)
            Remove-Item "Env:\$name" -ErrorAction SilentlyContinue
        }
        $env:PROCESSOR_ARCHITECTURE = "AMD64"

        Mock Invoke-RestMethod {
            [pscustomobject]@{
                tag_name = "v1.13.0"
                assets   = @(New-FakeAsset -Name "Toolport_1.13.0_x64-setup.exe")
            }
        }
        Mock Invoke-WebRequest { [IO.File]::WriteAllBytes($OutFile, $global:TpFakeBytes) }
        Mock Get-AuthenticodeSignature { [pscustomobject]@{ Status = "NotSigned"; SignerCertificate = $null } }
        Mock Start-Process { [pscustomobject]@{ ExitCode = 0 } }
        # The post-install confirmation reads the uninstall registry key.
        Mock Get-ItemProperty {
            [pscustomobject]@{
                DisplayName     = "Toolport"
                DisplayVersion  = "1.13.0"
                InstallLocation = '"C:\Users\test\AppData\Local\Toolport"'
                MainBinaryName  = "conduit.exe"
            }
        }
        # -DownloadOnly keeps the file; never touch the real Downloads folder.
        Mock Move-Item { }
    }

    AfterEach {
        foreach ($name in $script:SavedEnv.Keys) {
            $value = $script:SavedEnv[$name]
            if ($null -eq $value) {
                Remove-Item "Env:\$name" -ErrorAction SilentlyContinue
            } else {
                Set-Item "Env:\$name" -Value $value
            }
        }
    }

    Context "release lookup" {
        It "rejects a malformed -Version before making any network call" {
            $result = Invoke-Installer @{ Version = "1.13; rm -rf /" }

            $result.Output | Should -Match "doesn't look like a version"
            $result.ExitCode | Should -Be 1
            Should -Invoke Invoke-RestMethod -Times 0 -Exactly
        }

        It "accepts a bare version and asks for the v-prefixed tag" {
            Invoke-Installer @{ Version = "1.13.0" } | Out-Null

            Should -Invoke Invoke-RestMethod -Times 1 -Exactly -ParameterFilter {
                $Uri -eq "https://api.github.com/repos/tsouth89/toolport/releases/tags/v1.13.0"
            }
        }

        It "asks for the latest release when no version is given" {
            Invoke-Installer | Out-Null

            Should -Invoke Invoke-RestMethod -Times 1 -Exactly -ParameterFilter {
                $Uri -eq "https://api.github.com/repos/tsouth89/toolport/releases/latest"
            }
        }

        It "reads TOOLPORT_VERSION when the parameter is absent" {
            $env:TOOLPORT_VERSION = "1.12.0"

            Invoke-Installer | Out-Null

            Should -Invoke Invoke-RestMethod -Times 1 -Exactly -ParameterFilter {
                $Uri -eq "https://api.github.com/repos/tsouth89/toolport/releases/tags/v1.12.0"
            }
        }

        It "explains a 404 on a specific tag" {
            Mock Invoke-RestMethod { throw [FakeHttpException]::new("Not Found", 404) }

            $result = Invoke-Installer @{ Version = "9.9.9" }

            $result.Output | Should -Match "No release tagged v9\.9\.9"
            $result.ExitCode | Should -Be 1
        }

        It "explains a 404 on the latest lookup as nothing being published yet" {
            Mock Invoke-RestMethod { throw [FakeHttpException]::new("Not Found", 404) }

            $result = Invoke-Installer

            $result.Output | Should -Match "no published Toolport release yet"
        }

        It "explains a rate limit rather than reporting a generic failure" {
            Mock Invoke-RestMethod { throw [FakeHttpException]::new("rate limited", 403) }

            $result = Invoke-Installer

            $result.Output | Should -Match "rate-limited this machine"
        }

        It "sends the token as a bearer when GITHUB_TOKEN is set" {
            $env:GITHUB_TOKEN = "ghp_example"

            Invoke-Installer | Out-Null

            Should -Invoke Invoke-RestMethod -Times 1 -Exactly -ParameterFilter {
                $Headers["Authorization"] -eq "Bearer ghp_example"
            }
        }
    }

    Context "asset selection" {
        It "refuses an architecture Toolport does not ship" {
            $env:PROCESSOR_ARCHITECTURE = "x86"

            $result = Invoke-Installer

            $result.Output | Should -Match "Unsupported architecture 'x86'"
            Should -Invoke Invoke-RestMethod -Times 0 -Exactly
        }

        It "uses the real machine architecture from PROCESSOR_ARCHITEW6432" {
            # A 32-bit PowerShell on 64-bit Windows reports x86 in
            # PROCESSOR_ARCHITECTURE; the real arch is in the W6432 variable.
            $env:PROCESSOR_ARCHITECTURE = "x86"
            $env:PROCESSOR_ARCHITEW6432 = "AMD64"

            $result = Invoke-Installer

            $result.Output | Should -Not -Match "Unsupported architecture"
            Should -Invoke Start-Process -Times 1 -Exactly
        }

        It "prefers the native arm64 build when the release has one" {
            Mock Invoke-RestMethod {
                [pscustomobject]@{
                    tag_name = "v1.13.0"
                    assets   = @(
                        New-FakeAsset -Name "Toolport_1.13.0_x64-setup.exe"
                        New-FakeAsset -Name "Toolport_1.13.0_arm64-setup.exe"
                    )
                }
            }
            $env:PROCESSOR_ARCHITECTURE = "ARM64"

            $result = Invoke-Installer

            $result.Output | Should -Match "Toolport_1\.13\.0_arm64-setup\.exe"
            $result.Output | Should -Not -Match "using the x64 installer"
        }

        It "falls back to the x64 build on arm64 and says so" {
            $env:PROCESSOR_ARCHITECTURE = "ARM64"

            $result = Invoke-Installer

            $result.Output | Should -Match "No native arm64 build in v1\.13\.0"
            Should -Invoke Invoke-WebRequest -Times 1 -Exactly -ParameterFilter {
                $Uri -like "*Toolport_1.13.0_x64-setup.exe"
            }
        }

        It "never selects the updater signature asset" {
            Mock Invoke-RestMethod {
                [pscustomobject]@{
                    tag_name = "v1.13.0"
                    assets   = @(
                        New-FakeAsset -Name "Toolport_1.13.0_x64-setup.exe.sig"
                        New-FakeAsset -Name "Toolport_1.13.0_x64-setup.exe"
                    )
                }
            }

            Invoke-Installer | Out-Null

            Should -Invoke Invoke-WebRequest -Times 1 -Exactly -ParameterFilter {
                $Uri -like "*Toolport_1.13.0_x64-setup.exe"
            }
        }

        It "reports a release that carries no Windows installer" {
            Mock Invoke-RestMethod {
                [pscustomobject]@{
                    tag_name = "v1.13.0"
                    assets   = @(New-FakeAsset -Name "Toolport_1.13.0_amd64.deb")
                }
            }

            $result = Invoke-Installer

            $result.Output | Should -Match "has no Windows installer"
            Should -Invoke Invoke-WebRequest -Times 0 -Exactly
        }

        It "refuses a download URL that is not https" {
            Mock Invoke-RestMethod {
                [pscustomobject]@{
                    tag_name = "v1.13.0"
                    assets   = @(New-FakeAsset -Name "Toolport_1.13.0_x64-setup.exe" `
                            -Url "http://example.invalid/Toolport_1.13.0_x64-setup.exe")
                }
            }

            $result = Invoke-Installer

            $result.Output | Should -Match "no https download URL"
            Should -Invoke Invoke-WebRequest -Times 0 -Exactly
        }
    }

    Context "checksum enforcement" {
        It "refuses to install when the release publishes no checksum" {
            Mock Invoke-RestMethod {
                [pscustomobject]@{
                    tag_name = "v1.13.0"
                    assets   = @(New-FakeAsset -Name "Toolport_1.13.0_x64-setup.exe" -Digest $null)
                }
            }

            $result = Invoke-Installer

            $result.Output | Should -Match "publishes no checksum"
            $result.ExitCode | Should -Be 1
            Should -Invoke Invoke-WebRequest -Times 0 -Exactly
            Should -Invoke Start-Process -Times 0 -Exactly
        }

        It "installs an unverified asset only when -AllowUnverified is passed" {
            Mock Invoke-RestMethod {
                [pscustomobject]@{
                    tag_name = "v1.13.0"
                    assets   = @(New-FakeAsset -Name "Toolport_1.13.0_x64-setup.exe" -Digest $null)
                }
            }

            $result = Invoke-Installer @{ AllowUnverified = $true }

            $result.Output | Should -Match "Installing an unverified binary"
            Should -Invoke Start-Process -Times 1 -Exactly
        }

        It "treats TOOLPORT_ALLOW_UNVERIFIED=1 like the switch" {
            Mock Invoke-RestMethod {
                [pscustomobject]@{
                    tag_name = "v1.13.0"
                    assets   = @(New-FakeAsset -Name "Toolport_1.13.0_x64-setup.exe" -Digest $null)
                }
            }
            $env:TOOLPORT_ALLOW_UNVERIFIED = "1"

            Invoke-Installer | Out-Null

            Should -Invoke Start-Process -Times 1 -Exactly
        }

        It "treats TOOLPORT_ALLOW_UNVERIFIED=0 as not set" {
            Mock Invoke-RestMethod {
                [pscustomobject]@{
                    tag_name = "v1.13.0"
                    assets   = @(New-FakeAsset -Name "Toolport_1.13.0_x64-setup.exe" -Digest $null)
                }
            }
            $env:TOOLPORT_ALLOW_UNVERIFIED = "0"

            $result = Invoke-Installer

            $result.Output | Should -Match "publishes no checksum"
            Should -Invoke Start-Process -Times 0 -Exactly
        }

        It "verifies a matching digest and reports it" {
            $result = Invoke-Installer

            $result.Output | Should -Match "SHA256 verified: $($global:TpFakeSha256)"
            $result.ExitCode | Should -Be 0
            Should -Invoke Start-Process -Times 1 -Exactly
        }

        It "stops on a digest that does not match the downloaded bytes" {
            $wrong = "0" * 64
            Mock Invoke-RestMethod {
                [pscustomobject]@{
                    tag_name = "v1.13.0"
                    assets   = @(New-FakeAsset -Name "Toolport_1.13.0_x64-setup.exe" -Digest ("0" * 64))
                }
            }

            $result = Invoke-Installer

            $result.Output | Should -Match "SHA256 mismatch"
            $result.Output | Should -Match "expected $wrong"
            $result.ExitCode | Should -Be 1
            Should -Invoke Start-Process -Times 0 -Exactly
        }

        It "treats a short download as truncated before hashing it" {
            Mock Invoke-WebRequest { [IO.File]::WriteAllBytes($OutFile, [byte[]](1..8)) }

            $result = Invoke-Installer

            $result.Output | Should -Match "Treating it as truncated"
            Should -Invoke Start-Process -Times 0 -Exactly
        }

        It "rejects an empty download" {
            Mock Invoke-WebRequest { [IO.File]::WriteAllBytes($OutFile, [byte[]]@()) }

            $result = Invoke-Installer

            $result.Output | Should -Match "empty file"
            Should -Invoke Start-Process -Times 0 -Exactly
        }
    }

    Context "signature handling" {
        It "refuses a file whose Authenticode signature does not match it" {
            Mock Get-AuthenticodeSignature {
                [pscustomobject]@{ Status = "HashMismatch"; SignerCertificate = $null }
            }

            $result = Invoke-Installer

            $result.Output | Should -Match "doesn't match its contents"
            $result.ExitCode | Should -Be 1
            Should -Invoke Start-Process -Times 0 -Exactly
        }

        It "installs an unsigned build but warns about it" {
            $result = Invoke-Installer

            $result.Output | Should -Match "isn't code-signed"
            Should -Invoke Start-Process -Times 1 -Exactly
        }

        It "reports the publisher from a quoted CN containing a comma" {
            Mock Get-AuthenticodeSignature {
                [pscustomobject]@{
                    Status            = "Valid"
                    SignerCertificate = [pscustomobject]@{ Subject = 'CN="South, Brandon", O=Southbound' }
                }
            }

            $result = Invoke-Installer

            $result.Output | Should -Match "signature valid: South, Brandon"
        }

        It "keeps installing when the signature status is only untrusted" {
            Mock Get-AuthenticodeSignature {
                [pscustomobject]@{ Status = "UnknownError"; SignerCertificate = $null }
            }

            $result = Invoke-Installer

            $result.Output | Should -Match "Authenticode status: UnknownError"
            Should -Invoke Start-Process -Times 1 -Exactly
        }
    }

    Context "install invocation" {
        It "installs silently by default" {
            Invoke-Installer | Out-Null

            Should -Invoke Start-Process -Times 1 -Exactly -ParameterFilter {
                ($ArgumentList -join " ") -eq "/S"
            }
        }

        It "runs the wizard with no silent flag under -Interactive" {
            $result = Invoke-Installer @{ Interactive = $true }

            $result.Output | Should -Match "Running the installer \(wizard\)"
            Should -Invoke Start-Process -Times 1 -Exactly -ParameterFilter { -not $ArgumentList }
        }

        It "treats TOOLPORT_INTERACTIVE=1 like the switch" {
            $env:TOOLPORT_INTERACTIVE = "1"

            Invoke-Installer | Out-Null

            Should -Invoke Start-Process -Times 1 -Exactly -ParameterFilter { -not $ArgumentList }
        }

        It "verifies but never installs under -DownloadOnly" {
            $result = Invoke-Installer @{ DownloadOnly = $true }

            $result.Output | Should -Match "Verified installer saved to"
            $result.ExitCode | Should -Be 0
            Should -Invoke Move-Item -Times 1 -Exactly
            Should -Invoke Start-Process -Times 0 -Exactly
        }

        It "labels a -DownloadOnly file unverified when there was no checksum" {
            Mock Invoke-RestMethod {
                [pscustomobject]@{
                    tag_name = "v1.13.0"
                    assets   = @(New-FakeAsset -Name "Toolport_1.13.0_x64-setup.exe" -Digest $null)
                }
            }

            $result = Invoke-Installer @{ DownloadOnly = $true; AllowUnverified = $true }

            $result.Output | Should -Match "Unverified installer saved to"
        }

        It "reports a failing installer exit code" {
            Mock Start-Process { [pscustomobject]@{ ExitCode = 2 } }

            $result = Invoke-Installer

            $result.Output | Should -Match "installer exited with code 2"
            $result.ExitCode | Should -Be 1
        }

        It "explains a cancelled elevation prompt" {
            Mock Start-Process { [pscustomobject]@{ ExitCode = 1223 } }

            $result = Invoke-Installer

            $result.Output | Should -Match "elevation prompt was cancelled"
        }
    }

    Context "post-install confirmation" {
        It "reports the installed version and location" {
            $result = Invoke-Installer

            $result.Output | Should -Match "Installed Toolport 1\.13\.0"
            # The quotes NSIS writes around InstallLocation are stripped.
            $result.Output | Should -Match "to C:\\Users\\test\\AppData\\Local\\Toolport"
        }

        It "warns when no uninstall entry appeared despite a zero exit code" {
            Mock Get-ItemProperty { }

            $result = Invoke-Installer

            $result.Output | Should -Match "no 'Toolport' entry appeared"
            $result.ExitCode | Should -Be 0
        }

        It "warns when an older install won over the version it downloaded" {
            Mock Get-ItemProperty {
                [pscustomobject]@{
                    DisplayName     = "Toolport"
                    DisplayVersion  = "1.20.0"
                    InstallLocation = "C:\Toolport"
                    MainBinaryName  = "conduit.exe"
                }
            }

            $result = Invoke-Installer

            $result.Output | Should -Match "an existing newer install may have won"
        }
    }

    Context "session hygiene" {
        It "leaves no Install-Toolport function behind in the caller's session" {
            Invoke-Installer | Out-Null

            Test-Path "Function:\Install-Toolport" | Should -BeFalse
        }

        It "restores ErrorActionPreference and ProgressPreference" {
            $ErrorActionPreference = "Continue"
            $ProgressPreference = "Continue"

            Invoke-Installer | Out-Null

            $ErrorActionPreference | Should -Be "Continue"
            $ProgressPreference | Should -Be "Continue"
        }
    }
}
