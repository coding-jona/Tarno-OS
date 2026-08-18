<#
.SYNOPSIS
    Baut tarno-installer.exe direkt auf Windows - ohne dass vorher irgendwas
    manuell installiert werden muss.

.DESCRIPTION
    Einmal ausfuehren, das Skript macht den Rest:
      1. Prueft, ob Git vorhanden ist - falls nicht, laedt es die aktuelle
         Git-for-Windows-Version herunter und installiert sie still.
      2. Prueft, ob Rust (rustup) vorhanden ist - falls nicht, installiert
         es das GNU-Toolchain-Profil (kein Visual Studio Build Tools
         noetig, im Gegensatz zum MSVC-Standardprofil).
      3. Klont das Tarno-OS-Repo (falls das Skript nicht schon aus einem
         bestehenden Checkout heraus laeuft) und baut tarno-installer im
         Release-Modus.

    tarno-installer ist NUR das Flash-Werkzeug (schreibt ein fertiges
    sdcard.img auf einen USB-Stick) - das eigentliche Tarno-OS-Image selbst
    wird hier NICHT gebaut (das braucht Buildroot, laeuft nicht unter
    Windows). Siehe .github/workflows/build-os-image.yml fuer den
    Image-Build via GitHub Actions.

    NICHT auf echter Windows-Hardware getestet (diese Entwicklungsumgebung
    hat kein Windows) - nur sorgfaeltig gegen die offizielle
    rustup/Git-for-Windows-Dokumentation geschrieben. Bei Problemen bitte
    Rueckmeldung geben.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File build-tarno-installer.ps1
#>

$ErrorActionPreference = "Stop"

function Write-Step($Message) {
    Write-Host ""
    Write-Host "==> $Message" -ForegroundColor Cyan
}

function Test-CommandExists($Name) {
    return [bool](Get-Command $Name -ErrorAction SilentlyContinue)
}

# --- 1. Git ------------------------------------------------------------

Write-Step "Pruefe Git..."
if (Test-CommandExists "git") {
    Write-Host "Git ist bereits installiert: $(git --version)"
}
else {
    Write-Host "Git wurde nicht gefunden - lade Git for Windows herunter..."
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/git-for-windows/git/releases/latest"
    $asset = $release.assets | Where-Object { $_.name -match "64-bit\.exe$" } | Select-Object -First 1
    if (-not $asset) {
        throw "Konnte den Git-for-Windows-64-Bit-Installer nicht finden. Bitte Git manuell von https://git-scm.com/download/win installieren und das Skript erneut starten."
    }
    $installerPath = Join-Path $env:TEMP $asset.name
    Write-Host "Lade $($asset.name) herunter..."
    Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $installerPath

    Write-Host "Installiere Git still (kein Nutzereingriff noetig)..."
    Start-Process -FilePath $installerPath -ArgumentList "/VERYSILENT", "/NORESTART" -Wait

    # PATH fuer die laufende Sitzung nachziehen, ohne dass ein neues
    # Terminal geoeffnet werden muss.
    $env:PATH = "$env:PATH;C:\Program Files\Git\cmd"
    if (-not (Test-CommandExists "git")) {
        throw "Git-Installation abgeschlossen, aber 'git' ist im PATH dieser Sitzung nicht auffindbar. Bitte ein neues PowerShell-Fenster oeffnen und das Skript erneut starten."
    }
    Write-Host "Git installiert: $(git --version)"
}

# --- 2. Rust / rustup ----------------------------------------------------

Write-Step "Pruefe Rust..."
if (Test-CommandExists "cargo") {
    Write-Host "Rust ist bereits installiert: $(cargo --version)"
}
else {
    Write-Host "Rust wurde nicht gefunden - installiere rustup mit dem GNU-Toolchain-Profil..."
    Write-Host "(GNU statt MSVC, damit keine Visual-Studio-Build-Tools noetig sind.)"

    $rustupInit = Join-Path $env:TEMP "rustup-init.exe"
    Invoke-WebRequest -Uri "https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-gnu/rustup-init.exe" -OutFile $rustupInit

    & $rustupInit -y --default-host x86_64-pc-windows-gnu --profile minimal --default-toolchain stable

    # rustup legt cargo/rustc unter %USERPROFILE%\.cargo\bin ab - fuer die
    # laufende Sitzung in den PATH aufnehmen.
    $cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
    $env:PATH = "$env:PATH;$cargoBin"

    if (-not (Test-CommandExists "cargo")) {
        throw "Rust-Installation abgeschlossen, aber 'cargo' ist im PATH dieser Sitzung nicht auffindbar. Bitte ein neues PowerShell-Fenster oeffnen und das Skript erneut starten."
    }
    Write-Host "Rust installiert: $(cargo --version)"
}

# --- 3. Repo finden oder klonen ------------------------------------------

Write-Step "Suche Tarno-OS-Checkout..."

$scriptDir = $PSScriptRoot
$repoRoot = $null

# Fall A: Skript liegt bereits in scripts/windows/ innerhalb eines Checkouts.
$candidateRoot = Split-Path (Split-Path $scriptDir -Parent) -Parent
if (Test-Path (Join-Path $candidateRoot "tarno-installer\Cargo.toml")) {
    $repoRoot = $candidateRoot
    Write-Host "Bestehendes Checkout gefunden: $repoRoot"
}
else {
    # Fall B: Skript wurde einzeln heruntergeladen - Repo daneben klonen.
    $cloneTarget = Join-Path (Get-Location) "Tarno-OS"
    if (Test-Path (Join-Path $cloneTarget "tarno-installer\Cargo.toml")) {
        $repoRoot = $cloneTarget
        Write-Host "Bestehendes Checkout gefunden: $repoRoot"
    }
    else {
        Write-Host "Kein Checkout gefunden - klone https://github.com/coding-jona/Tarno-OS nach $cloneTarget ..."
        git clone --depth 1 https://github.com/coding-jona/Tarno-OS.git $cloneTarget
        $repoRoot = $cloneTarget
    }
}

# --- 4. Bauen -------------------------------------------------------------

Write-Step "Baue tarno-installer (Release-Modus)..."
Push-Location (Join-Path $repoRoot "tarno-installer")
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build ist mit Exit-Code $LASTEXITCODE fehlgeschlagen - siehe Ausgabe oben."
    }
}
finally {
    Pop-Location
}

$exePath = Join-Path $repoRoot "tarno-installer\target\release\tarno-installer.exe"
Write-Step "Fertig!"
Write-Host "tarno-installer.exe liegt hier:" -ForegroundColor Green
Write-Host "  $exePath" -ForegroundColor Green
Write-Host ""
Write-Host "Wichtig: die Datei als Administrator ausfuehren (Rechtsklick -> ""Als Administrator ausfuehren"") -" -ForegroundColor Yellow
Write-Host "Rohschreibzugriff auf ein Laufwerk braucht erhoehte Rechte." -ForegroundColor Yellow
Write-Host ""
Write-Host "tarno-installer schreibt ein fertiges sdcard.img auf einen USB-Stick -" -ForegroundColor Yellow
Write-Host "das Image selbst kommt aus dem Buildroot-Build (GitHub Actions:" -ForegroundColor Yellow
Write-Host "'Build Tarno OS image'), nicht aus diesem Skript." -ForegroundColor Yellow
