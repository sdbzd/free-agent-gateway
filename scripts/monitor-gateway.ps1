param(
    [string]$BaseUrl = "http://127.0.0.1:9000",
    [int]$IntervalSeconds = 30,
    [string]$OutDir = ".local-monitor"
)

$ErrorActionPreference = "Continue"
$root = (Resolve-Path ".").Path
$outPath = Join-Path $root $OutDir
New-Item -ItemType Directory -Force $outPath | Out-Null

$anomalyLog = Join-Path $outPath "anomalies.log"
$snapshotLog = Join-Path $outPath "snapshots.jsonl"
$statePath = Join-Path $outPath "last-state.json"

function Write-Anomaly {
    param([string]$Kind, [string]$Message, [object]$Data = $null)
    $event = [ordered]@{
        ts = (Get-Date).ToString("o")
        kind = $Kind
        message = $Message
        data = $Data
    }
    ($event | ConvertTo-Json -Depth 12 -Compress) | Add-Content -Path $anomalyLog -Encoding UTF8
}

function Get-Json {
    param([string]$Path)
    Invoke-RestMethod -Uri ($BaseUrl.TrimEnd("/") + $Path) -TimeoutSec 15
}

function Value-OrZero {
    param($Value)
    if ($null -eq $Value) { return 0 }
    return $Value
}

$last = @{
    total_errors = 0
    usage_errors = 0
}
if (Test-Path $statePath) {
    try {
        $loaded = Get-Content $statePath -Raw | ConvertFrom-Json
        $last.total_errors = [int64](Value-OrZero $loaded.total_errors)
        $last.usage_errors = [int64](Value-OrZero $loaded.usage_errors)
    } catch {}
}

Write-Anomaly "monitor_start" "Gateway monitor started" @{ base_url = $BaseUrl; interval_seconds = $IntervalSeconds }

while ($true) {
    try {
        $status = Get-Json "/admin/status"
        $usage = Get-Json "/admin/metadata/usage"

        $summary = [ordered]@{
            ts = (Get-Date).ToString("o")
            healthy = $status.healthy_count
            unhealthy = $status.unhealthy_count
            exhausted = $status.exhausted_count
            total_errors = $status.total_errors
            usage_tokens = $usage.summary.total_tokens
            usage_requests = $usage.summary.total_requests
            usage_errors = $usage.summary.total_errors
        }
        ($summary | ConvertTo-Json -Compress) | Add-Content -Path $snapshotLog -Encoding UTF8

        if ((Value-OrZero $status.unhealthy_count) -gt 0) {
            Write-Anomaly "provider_unhealthy" "One or more providers are unhealthy" $status.providers
        }
        if ((Value-OrZero $status.exhausted_count) -gt 0) {
            Write-Anomaly "provider_exhausted" "One or more providers have no available keys" $status.providers
        }
        foreach ($provider in @($status.providers)) {
            foreach ($key in @($provider.keys)) {
                if ($key.status -ne "available") {
                    Write-Anomaly "key_not_available" "$($provider.name) key $($key.key_id) is $($key.status)" @{
                        provider = $provider.name
                        key_id = $key.key_id
                        status = $key.status
                        last_error_status = $key.last_error_status
                        cooldown_until = $key.cooldown_until
                    }
                }
                if ((Value-OrZero $key.fail_count) -ge 2) {
                    Write-Anomaly "key_fail_count" "$($provider.name) key $($key.key_id) fail_count=$($key.fail_count)" @{
                        provider = $provider.name
                        key_id = $key.key_id
                        fail_count = $key.fail_count
                        last_error_status = $key.last_error_status
                    }
                }
            }
        }

        if ((Value-OrZero $status.total_errors) -gt $last.total_errors) {
            Write-Anomaly "gateway_error_increase" "Gateway total_errors increased" @{
                previous = $last.total_errors
                current = $status.total_errors
            }
        }
        if ((Value-OrZero $usage.summary.total_errors) -gt $last.usage_errors) {
            Write-Anomaly "model_error_increase" "Model usage errors increased" @{
                previous = $last.usage_errors
                current = $usage.summary.total_errors
            }
        }

        $last.total_errors = [int64](Value-OrZero $status.total_errors)
        $last.usage_errors = [int64](Value-OrZero $usage.summary.total_errors)
        ($last | ConvertTo-Json -Compress) | Set-Content -Path $statePath -Encoding UTF8
    } catch {
        Write-Anomaly "monitor_error" $_.Exception.Message
    }

    Start-Sleep -Seconds $IntervalSeconds
}
