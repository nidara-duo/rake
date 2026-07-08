$commands = @(
    "scoop checkup",
    "scoop list",
    "scoop status",
    "scoop bucket list",
    "scoop bucket add nonportable",
    "scoop bucket rm nonportable",
    "scoop search rustup"
)

$runs = 20
$benchFile = "bench.txt"

$null = New-Item -Path $benchFile -ItemType File -Force

for ($i = 1; $i -le $runs; $i++) {
    Write-Host "Iteration $i/$runs..." -ForegroundColor Yellow

    foreach ($cmd in $commands) {
        $elapsed = Measure-Command {
            try {
                Invoke-Expression $cmd *>$null
            } catch {}
        }

        $ms = [Math]::Round($elapsed.TotalMilliseconds)

        "$cmd | $ms ms" | Out-File -FilePath $benchFile -Append -Encoding utf8
    }
}
